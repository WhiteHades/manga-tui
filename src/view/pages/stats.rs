use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, ToSpan};
use ratatui::widgets::{Block, List, Paragraph, Widget, Wrap};
use throbber_widgets_tui::{Throbber, ThrobberState};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use crate::backend::manga_provider::MangaProvider;
use crate::backend::manga_provider::local::LocalLibraryStats;
use crate::backend::tui::Events;
use crate::global::{ERROR_STYLE, INSTRUCTIONS_STYLE};
use crate::utils::render_search_bar;
use crate::view::widgets::Component;

#[derive(Debug, PartialEq, Eq)]
pub enum StatsState {
    Normal,
    EditingPath,
    Reloading,
    Reloaded,
    Error(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum StatsActions {
    EditPath,
    StopEditingPath,
    ReloadCurrentPath,
    ReloadInputPath,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StatsEvents {
    Reloaded(Result<LocalLibraryStats, String>),
}

pub struct StatsPage<T>
where
    T: MangaProvider,
{
    manga_provider: Arc<T>,
    state: StatsState,
    library_path: Input,
    loading_state: ThrobberState,
    tasks: JoinSet<()>,
    pub global_event_tx: Option<UnboundedSender<Events>>,
    pub local_action_tx: UnboundedSender<StatsActions>,
    pub local_action_rx: UnboundedReceiver<StatsActions>,
    pub local_event_tx: UnboundedSender<StatsEvents>,
    pub local_event_rx: UnboundedReceiver<StatsEvents>,
}

impl<T> Component for StatsPage<T>
where
    T: MangaProvider,
{
    type Actions = StatsActions;

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.tick();

        let [summary_area, path_area, detail_area, status_area] =
            Layout::vertical([Constraint::Length(7), Constraint::Length(4), Constraint::Fill(1), Constraint::Length(4)])
                .margin(1)
                .areas(area);

        self.render_summary(summary_area, frame);
        self.render_path(path_area, frame);
        self.render_details(detail_area, frame);
        self.render_status(status_area, frame);
    }

    fn update(&mut self, action: Self::Actions) {
        match action {
            StatsActions::EditPath => self.edit_path(),
            StatsActions::StopEditingPath => self.stop_editing_path(),
            StatsActions::ReloadCurrentPath => self.reload_current_path(),
            StatsActions::ReloadInputPath => self.reload_input_path(),
        }
    }

    fn clean_up(&mut self) {
        self.state = StatsState::Normal;
        self.tasks.abort_all();
        self.refresh_path_input();
    }

    fn handle_events(&mut self, events: Events) {
        match events {
            Events::Key(key_event) => self.handle_key_events(key_event),
            Events::Tick => self.tick(),
            _ => {},
        }
    }
}

impl<T> StatsPage<T>
where
    T: MangaProvider,
{
    pub fn new(manga_provider: Arc<T>) -> Self {
        let (local_action_tx, local_action_rx) = mpsc::unbounded_channel::<StatsActions>();
        let (local_event_tx, local_event_rx) = mpsc::unbounded_channel::<StatsEvents>();
        let library_path = Input::default().with_value(
            manga_provider
                .local_library_stats()
                .map(|stats| stats.library_path.display().to_string())
                .unwrap_or_default(),
        );

        Self {
            manga_provider,
            state: StatsState::Normal,
            library_path,
            loading_state: ThrobberState::default(),
            tasks: JoinSet::new(),
            global_event_tx: None,
            local_action_tx,
            local_action_rx,
            local_event_tx,
            local_event_rx,
        }
    }

    pub fn with_global_sender(mut self, tx: UnboundedSender<Events>) -> Self {
        self.global_event_tx = Some(tx);
        self
    }

    pub fn is_typing(&self) -> bool {
        self.state == StatsState::EditingPath
    }

    fn stats(&self) -> Option<LocalLibraryStats> {
        self.manga_provider.local_library_stats()
    }

    fn refresh_path_input(&mut self) {
        if let Some(stats) = self.stats() {
            self.library_path = Input::default().with_value(stats.library_path.display().to_string());
        }
    }

    fn render_summary(&self, area: Rect, frame: &mut Frame<'_>) {
        let Some(stats) = self.stats() else {
            Paragraph::new("Local stats unavailable")
                .block(Block::bordered().title("Stats"))
                .render(area, frame.buffer_mut());
            return;
        };

        let items = [
            Line::from(vec!["Manga: ".into(), stats.manga_count.to_string().yellow()]),
            Line::from(vec!["Chapters: ".into(), stats.chapter_count.to_string().yellow()]),
            Line::from(vec!["Pages: ".into(), stats.page_count.to_string().yellow()]),
            Line::from(vec!["Library size: ".into(), human_bytes(stats.total_bytes).yellow()]),
        ];

        List::new(items)
            .block(Block::bordered().title("Local library"))
            .render(area, frame.buffer_mut());
    }

    fn render_path(&self, area: Rect, frame: &mut Frame<'_>) {
        let input_help = match self.state {
            StatsState::EditingPath => Line::from(vec![
                "Reload ".into(),
                "<Enter>".to_span().style(*INSTRUCTIONS_STYLE),
                " cancel ".into(),
                "<Esc>".to_span().style(*INSTRUCTIONS_STYLE),
            ]),
            _ => Line::from(vec![
                "Edit path ".into(),
                "<e>".to_span().style(*INSTRUCTIONS_STYLE),
                " reload ".into(),
                "<r>".to_span().style(*INSTRUCTIONS_STYLE),
            ]),
        };

        render_search_bar(self.is_typing(), input_help, &self.library_path, frame, area);
    }

    fn render_details(&self, area: Rect, frame: &mut Frame<'_>) {
        let Some(stats) = self.stats() else {
            Block::bordered().title("Formats").render(area, frame.buffer_mut());
            return;
        };

        let [format_area, note_area] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
        let format_lines = [
            Line::from(vec!["Image folders: ".into(), stats.image_folder_count.to_string().green()]),
            Line::from(vec!["CBZ/ZIP: ".into(), stats.cbz_count.to_string().green()]),
            Line::from(vec!["CBR/RAR: ".into(), stats.cbr_count.to_string().green()]),
            Line::from(vec!["EPUB: ".into(), stats.epub_count.to_string().green()]),
            Line::from(vec!["Archives: ".into(), stats.archive_count().to_string().green()]),
        ];

        List::new(format_lines)
            .block(Block::bordered().title("Formats"))
            .render(format_area, frame.buffer_mut());

        Paragraph::new(Line::from(vec!["Archives are indexed without full extraction. Pages are read lazily when opened.".into()]))
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title("Index"))
            .render(
                note_area.inner(Margin {
                    horizontal: 0,
                    vertical: 0,
                }),
                frame.buffer_mut(),
            );
    }

    fn render_status(&mut self, area: Rect, frame: &mut Frame<'_>) {
        match &self.state {
            StatsState::Reloading => {
                let loader = Throbber::default()
                    .label("Reloading local library")
                    .style(Style::default().fg(Color::Yellow))
                    .throbber_set(throbber_widgets_tui::BRAILLE_SIX)
                    .use_type(throbber_widgets_tui::WhichUse::Spin);
                ratatui::widgets::StatefulWidget::render(loader, area, frame.buffer_mut(), &mut self.loading_state);
            },
            StatsState::Reloaded => {
                Paragraph::new("Library reloaded")
                    .block(Block::bordered().title("Status"))
                    .render(area, frame.buffer_mut());
            },
            StatsState::Error(message) => {
                Paragraph::new(message.to_span().style(*ERROR_STYLE))
                    .wrap(Wrap { trim: true })
                    .block(Block::bordered().title("Status"))
                    .render(area, frame.buffer_mut());
            },
            _ => {
                Paragraph::new(Line::from(vec!["Open manga from Library/Search. Change folders here without restarting.".into()]))
                    .wrap(Wrap { trim: true })
                    .block(Block::bordered().title("Status"))
                    .render(area, frame.buffer_mut());
            },
        }
    }

    fn edit_path(&mut self) {
        if self.state != StatsState::Reloading {
            self.refresh_path_input();
            self.state = StatsState::EditingPath;
        }
    }

    fn stop_editing_path(&mut self) {
        self.refresh_path_input();
        self.state = StatsState::Normal;
    }

    fn reload_current_path(&mut self) {
        if let Some(stats) = self.stats() {
            self.reload_library(stats.library_path);
        }
    }

    fn reload_input_path(&mut self) {
        let value = self.library_path.value().trim();
        if value.is_empty() {
            self.state = StatsState::Error("Local library path cannot be empty".to_string());
            return;
        }

        self.reload_library(PathBuf::from(value));
    }

    fn reload_library(&mut self, path: PathBuf) {
        if self.state == StatsState::Reloading {
            return;
        }

        self.state = StatsState::Reloading;
        let provider = Arc::clone(&self.manga_provider);
        let local_tx = self.local_event_tx.clone();
        let global_tx = self.global_event_tx.clone();

        self.tasks.spawn(async move {
            let reload_result = tokio::task::spawn_blocking(move || {
                provider
                    .reload_local_library(path)
                    .and_then(|_| provider.local_library_stats().ok_or_else(|| "Local stats unavailable".into()))
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);

            if reload_result.is_ok()
                && let Some(tx) = global_tx
            {
                tx.send(Events::LocalLibraryReloaded).ok();
            }

            local_tx.send(StatsEvents::Reloaded(reload_result)).ok();
        });
    }

    fn handle_key_events(&mut self, key_event: KeyEvent) {
        match self.state {
            StatsState::EditingPath => match key_event.code {
                KeyCode::Enter => self.local_action_tx.send(StatsActions::ReloadInputPath).ok(),
                KeyCode::Esc => self.local_action_tx.send(StatsActions::StopEditingPath).ok(),
                _ => {
                    self.library_path.handle_event(&event::Event::Key(key_event));
                    None
                },
            },
            _ => match key_event.code {
                KeyCode::Char('e') => self.local_action_tx.send(StatsActions::EditPath).ok(),
                KeyCode::Char('r') => self.local_action_tx.send(StatsActions::ReloadCurrentPath).ok(),
                _ => None,
            },
        };
    }

    fn tick(&mut self) {
        if let Ok(event) = self.local_event_rx.try_recv() {
            match event {
                StatsEvents::Reloaded(Ok(stats)) => {
                    self.library_path = Input::default().with_value(stats.library_path.display().to_string());
                    self.state = StatsState::Reloaded;
                },
                StatsEvents::Reloaded(Err(message)) => {
                    self.state = StatsState::Error(message);
                },
            }
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;

    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }

    if unit == 0 { format!("{} {}", bytes, UNITS[unit]) } else { format!("{size:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::backend::manga_provider::local::LocalProvider;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("manga-tui-stats-{name}-{unique}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect::<Vec<_>>().join("")
    }

    #[test]
    fn formats_bytes() {
        assert_eq!("512 B", human_bytes(512));
        assert_eq!("2.0 KiB", human_bytes(2048));
    }

    #[test]
    fn renders_local_library_stats() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("render");
        fs::write(dir.path.join("page.jpg"), include_bytes!("../../../data_test/images/1.jpg"))?;
        let provider = Arc::new(LocalProvider::from_path(&dir.path)?);
        let mut stats = StatsPage::new(provider);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        terminal.draw(|frame| stats.render(frame.area(), frame))?;

        let output = buffer_text(terminal.backend().buffer());
        assert!(output.contains("Local library"));
        assert!(output.contains("Manga:"));
        assert!(output.contains(dir.path.to_str().unwrap()));
        Ok(())
    }
}
