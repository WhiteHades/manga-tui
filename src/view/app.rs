use std::sync::Arc;

use ::crossterm::event::KeyCode;
use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Tabs, Widget};
use ratatui_image::picker::Picker;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use self::home::Home;
use self::manga::MangaPage;
use self::reader::MangaReader;
use self::search::{InputMode, SearchPage};
use self::stats::StatsPage;
use super::widgets::Component;
use crate::backend::manga_provider::{ChapterToRead, ListOfChapters, Manga, MangaProvider, Pagination};
use crate::backend::tracker::MangaTracker;
use crate::backend::tui::{Action, Events};
use crate::config::MangaTuiConfig;
use crate::global::INSTRUCTIONS_STYLE;
use crate::view::pages::*;
use crate::view::widgets::ErrorModal;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum AppState {
    Runnning,
    Done,
}

#[derive(PartialEq, Eq, Debug, Clone, Default)]
pub struct MangaToRead {
    pub title: String,
    pub manga_id: String,
    pub list: ListOfChapters,
}

/// The application which owns every `page` or section in manga-tui
/// its job mainly consists of displaying the selected page, passing the `manga_provider` to each
/// page handling when to quit and switching between pages based on user inputs
pub struct App<T, S>
where
    T: MangaProvider,
    S: MangaTracker,
{
    pub global_action_tx: UnboundedSender<Action>,
    pub global_action_rx: UnboundedReceiver<Action>,
    pub global_event_tx: UnboundedSender<Events>,
    pub global_event_rx: UnboundedReceiver<Events>,
    pub state: AppState,
    pub current_tab: SelectedPage,
    pub manga_page: Option<MangaPage<T, S>>,
    pub manga_reader_page: Option<MangaReader<T, S>>,
    pub search_page: SearchPage<T, S>,
    pub home_page: Home<T>,
    pub stats_page: StatsPage<T>,
    manga_provider: Arc<T>,
    manga_tracker: Option<S>,
    error_message: Option<String>,
    // The picker is what decides how big a image needs to be rendered depending on the user's
    // terminal font size and the graphics it supports
    // if the terminal doesn't support any graphics protocol the picker is `None`
    picker: Option<Picker>,
    pending_goto_prefix: bool,
}

impl<T, S> Component for App<T, S>
where
    T: MangaProvider,
    S: MangaTracker,
{
    type Actions = Action;

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        if self.manga_reader_page.is_some() && self.current_tab == SelectedPage::ReaderTab {
            self.manga_reader_page.as_mut().unwrap().render(area, frame);
        } else {
            let main_layout = Layout::vertical([Constraint::Percentage(6), Constraint::Percentage(94)]);

            let [top_tabs_area, page_area] = main_layout.areas(area);

            self.render_top_tabs(top_tabs_area, frame.buffer_mut());

            self.render_pages(page_area, frame);
        }

        if let Some(message) = self.error_message.as_ref().cloned() {
            self.render_modal_error_message(area, frame.buffer_mut(), &message);
        }
    }

    fn handle_events(&mut self, events: Events) {
        match events {
            Events::Key(key_event) => {
                self.handle_key_events(key_event);
            },
            Events::GoToMangaPage(manga) => self.go_to_manga_page(manga),
            Events::ReadChapter(chapter_response, manga_to_read) => {
                self.go_to_read_chapter(chapter_response, manga_to_read, self.manga_tracker.clone())
            },
            Events::GoSearchPage => {
                self.go_search_page();
            },
            Events::GoToHome => self.go_to_home(),
            Events::GoStatsPage => self.go_stats_page(),
            Events::LocalLibraryReloaded => self.local_library_reloaded(),

            Events::Error(message) => self.display_error_message(message),

            Events::GoBackMangaPage => {
                if self.current_tab == SelectedPage::ReaderTab && self.manga_reader_page.is_some() {
                    self.manga_reader_page.as_mut().unwrap().clean_up();
                    self.current_tab = SelectedPage::MangaTab;
                }
            },
            _ => {},
        }
    }

    fn update(&mut self, action: Action) {
        match action {
            Action::Quit => {
                self.state = AppState::Done;
            },
        }
    }

    fn clean_up(&mut self) {}
}

impl<T, S> App<T, S>
where
    T: MangaProvider,
    S: MangaTracker,
{
    pub fn new(
        api_client: T,
        manga_tracker: Option<S>,
        picker: Option<Picker>,
        filters_state: T::FiltersHandler,
        filter_widget: T::Widget,
    ) -> Self {
        let (global_action_tx, global_action_rx) = unbounded_channel::<Action>();
        let (global_event_tx, global_event_rx) = unbounded_channel::<Events>();

        global_event_tx.send(Events::GoToHome).ok();

        let provider = Arc::new(api_client);

        App {
            picker: picker.clone(),
            current_tab: SelectedPage::default(),
            search_page: SearchPage::new(
                picker.clone(),
                Arc::clone(&provider),
                manga_tracker.clone(),
                filters_state,
                filter_widget,
                Pagination::from_first_page(16),
            )
            .with_global_sender(global_event_tx.clone()),
            stats_page: StatsPage::new(Arc::clone(&provider)).with_global_sender(global_event_tx.clone()),
            home_page: Home::new(picker, Arc::clone(&provider)).with_global_sender(global_event_tx.clone()),
            manga_page: None,
            manga_reader_page: None,
            global_action_tx,
            global_action_rx,
            global_event_tx,
            global_event_rx,
            manga_tracker,
            state: AppState::Runnning,
            error_message: None,
            manga_provider: Arc::clone(&provider),
            pending_goto_prefix: false,
        }
    }

    pub fn render_top_tabs(&self, area: Rect, buf: &mut Buffer) {
        let mut titles: Vec<&str> = vec!["Library <gh>", "Search <gs>", "Stats <gt>"];

        let tabs_block = Block::default().borders(Borders::BOTTOM);

        let index_current_tab = match self.current_tab {
            SelectedPage::Home => 0,
            SelectedPage::Search => 1,
            SelectedPage::Stats => 2,
            SelectedPage::MangaTab => {
                titles.push(" 📖 Manga page");
                3
            },
            _ => 0,
        };

        Tabs::new(titles)
            .block(tabs_block)
            .highlight_style(*INSTRUCTIONS_STYLE)
            .select(index_current_tab)
            .padding("", "")
            .divider(" | ")
            .render(area, buf);
    }

    fn render_modal_error_message(&self, area: Rect, buf: &mut Buffer, message: &str) {
        ErrorModal::new(message).render(area, buf);
    }

    fn display_error_message(&mut self, error_message: String) {
        self.error_message = Some(error_message);
    }

    fn close_error_mesagge(&mut self) {
        self.error_message = None;
    }

    fn is_displaying_error_message(&self) -> bool {
        self.error_message.is_some()
    }

    pub fn render_pages(&mut self, area: Rect, frame: &mut Frame<'_>) {
        match self.current_tab {
            SelectedPage::Search => self.render_search_page(area, frame),
            SelectedPage::MangaTab => self.render_manga_page(area, frame),
            SelectedPage::Home => self.render_home_page(area, frame),
            SelectedPage::Stats => self.render_stats_page(area, frame),
            // Reader tab should be on full screen
            SelectedPage::ReaderTab => {},
        }
    }

    fn render_stats_page(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.stats_page.render(area, frame);
    }

    pub fn render_search_page(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.search_page.render(area, frame);
    }

    pub fn render_manga_page(&mut self, area: Rect, frame: &mut Frame<'_>) {
        if let Some(page) = self.manga_page.as_mut() {
            page.render(area, frame);
        }
    }

    pub fn render_home_page(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.home_page.render(area, frame);
    }

    /// This method ensures a chapter is bookmarked on quit as well
    /// only if auto_bookmark = true
    fn auto_bookmark_on_quit(&mut self) {
        if let Some(reader_page) = self.manga_reader_page.as_mut()
            && reader_page.auto_bookmark
        {
            reader_page.bookmark_current_chapter();
        }
        if let Some(reader_page) = self.manga_reader_page.as_mut() {
            reader_page.finish_reading_session();
        }
    }

    fn quit(&mut self) {
        self.auto_bookmark_on_quit();
        self.global_action_tx.send(Action::Quit).ok();
    }

    fn key_events_error_message(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Char('q') | KeyCode::Esc => self.close_error_mesagge(),
            KeyCode::Char('c') if key_event.modifiers == KeyModifiers::CONTROL => self.quit(),
            _ => {},
        }
    }

    fn handle_key_events(&mut self, key_event: KeyEvent) -> bool {
        if self.manga_page.as_ref().is_some_and(|page| page.is_downloading_all_chapters()) {
            return false;
        }

        if key_event.code == KeyCode::Char('c') && key_event.modifiers == KeyModifiers::CONTROL {
            self.quit();
            return true;
        }

        if self.current_tab != SelectedPage::ReaderTab
            && self.search_page.input_mode != InputMode::Typing
            && !self.search_page.is_typing_filter()
            && !self.stats_page.is_typing()
        {
            if self.pending_goto_prefix {
                self.pending_goto_prefix = false;

                match key_event.code {
                    KeyCode::Char('h') => {
                        if self.current_tab != SelectedPage::ReaderTab {
                            self.global_event_tx.send(Events::GoToHome).ok();
                        }
                        return true;
                    },
                    KeyCode::Char('s') => {
                        if self.current_tab != SelectedPage::ReaderTab {
                            self.global_event_tx.send(Events::GoSearchPage).ok();
                        }
                        return true;
                    },
                    KeyCode::Char('t') => {
                        if self.current_tab != SelectedPage::ReaderTab {
                            self.global_event_tx.send(Events::GoStatsPage).ok();
                        }
                        return true;
                    },
                    KeyCode::Esc => return true,
                    _ => {},
                }
            }

            match key_event.code {
                KeyCode::Char('q') => {
                    self.quit();
                    return true;
                },
                KeyCode::Char('g') => {
                    self.pending_goto_prefix = true;
                    return true;
                },

                _ => {},
            }
        }

        false
    }

    fn go_search_page(&mut self) {
        if self.manga_page.is_some() {
            self.manga_page.as_mut().unwrap().clean_up();
            self.manga_page = None;
        }
        self.stats_page.clean_up();
        self.current_tab = SelectedPage::Search;
    }

    fn go_to_manga_page(&mut self, manga: Manga) {
        if self.manga_reader_page.is_some() {
            self.manga_reader_page.as_mut().unwrap().clean_up();
            self.manga_reader_page = None;
        }

        self.stats_page.clean_up();

        self.current_tab = SelectedPage::MangaTab;

        let config = MangaTuiConfig::get();

        let manga_page = MangaPage::new(manga, self.picker.clone(), Arc::clone(&self.manga_provider))
            .with_global_sender(self.global_event_tx.clone())
            .auto_bookmark(config.auto_bookmark)
            .with_manga_tracker(self.manga_tracker.clone());

        self.manga_page = Some(manga_page);
    }

    fn go_to_read_chapter(&mut self, chapter_to_read: ChapterToRead, manga_to_read: MangaToRead, manga_tracker: Option<S>) {
        let Some(picker) = self.picker.as_ref().cloned() else {
            self.display_error_message(
                "This terminal does not support inline images, so the reader cannot open chapters.".to_string(),
            );
            return;
        };

        self.home_page.clean_up();
        self.stats_page.clean_up();
        self.current_tab = SelectedPage::ReaderTab;

        let mut manga_reader = MangaReader::new(chapter_to_read, manga_to_read.manga_id, picker, Arc::clone(&self.manga_provider))
            .with_global_sender(self.global_event_tx.clone())
            .with_list_of_chapters(manga_to_read.list)
            .with_manga_title(manga_to_read.title)
            .with_manga_tracker(manga_tracker);

        let config = MangaTuiConfig::get();

        if config.auto_bookmark {
            manga_reader.set_auto_bookmark();
        }

        manga_reader.init_fetching_pages();

        self.manga_reader_page = Some(manga_reader);
    }

    fn go_to_home(&mut self) {
        if self.manga_page.is_some() {
            self.manga_page.as_mut().unwrap().clean_up();
            self.manga_page = None;
        }

        self.stats_page.clean_up();

        if self.home_page.require_search() {
            self.home_page.init_search();
        }

        self.current_tab = SelectedPage::Home;
    }

    fn go_stats_page(&mut self) {
        if self.manga_page.is_some() {
            self.manga_page.as_mut().unwrap().clean_up();
            self.manga_page = None;
        }
        self.current_tab = SelectedPage::Stats;
    }

    fn local_library_reloaded(&mut self) {
        self.home_page.clean_up();
        self.search_page.clean_up();

        if let Some(manga_page) = self.manga_page.as_mut() {
            manga_page.clean_up();
        }
        self.manga_page = None;

        if let Some(reader_page) = self.manga_reader_page.as_mut() {
            reader_page.clean_up();
        }
        self.manga_reader_page = None;
        self.current_tab = SelectedPage::Stats;
    }

    pub async fn listen_to_event(&mut self) {
        if let Some(event) = self.global_event_rx.recv().await {
            // If the app is displaying an error message then the user can only close the app or
            // the error message popup
            if self.is_displaying_error_message()
                && let Events::Key(key_event) = event.clone()
            {
                self.key_events_error_message(key_event);
                return;
            }

            if let Events::Key(key_event) = event.clone()
                && self.handle_key_events(key_event)
            {
                return;
            }

            self.handle_events(event.clone());

            match self.current_tab {
                SelectedPage::Search => {
                    self.search_page.handle_events(event);
                },
                SelectedPage::MangaTab => {
                    self.manga_page.as_mut().unwrap().handle_events(event);
                },
                SelectedPage::ReaderTab => {
                    self.manga_reader_page.as_mut().unwrap().handle_events(event);
                },
                SelectedPage::Home => {
                    self.home_page.handle_events(event);
                },
                SelectedPage::Stats => {
                    self.stats_page.handle_events(event);
                },
            };
        }
    }

    pub fn update_based_on_action(&mut self) {
        if let Ok(app_action) = self.global_action_rx.try_recv() {
            self.update(app_action);
        }

        match self.current_tab {
            SelectedPage::Search => {
                if let Ok(search_page_action) = self.search_page.local_action_rx.try_recv() {
                    self.search_page.update(search_page_action);
                }
            },
            SelectedPage::MangaTab => {
                if let Some(manga_page) = self.manga_page.as_mut()
                    && let Ok(action) = manga_page.local_action_rx.try_recv()
                {
                    manga_page.update(action);
                }
            },
            SelectedPage::ReaderTab => {
                if let Some(reader_page) = self.manga_reader_page.as_mut()
                    && let Ok(reader_action) = reader_page.local_action_rx.try_recv()
                {
                    reader_page.update(reader_action);
                }
            },
            SelectedPage::Home => {
                if let Ok(home_action) = self.home_page.local_action_rx.try_recv() {
                    self.home_page.update(home_action);
                }
            },
            SelectedPage::Stats => {
                if let Ok(stats_action) = self.stats_page.local_action_rx.try_recv() {
                    self.stats_page.update(stats_action);
                }
            },
        };
    }

    #[cfg(test)]
    fn with_manga_page(mut self) -> Self {
        self.manga_page =
            Some(MangaPage::new(crate::backend::manga_provider::Manga::default(), None, Arc::clone(&self.manga_provider)));

        self
    }
}

#[cfg(test)]
mod tests {

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::backend::manga_provider::mock::{MockFilterState, MockFiltersHandler, MockMangaPageProvider, MockWidgetFilter};
    use crate::backend::manga_provider::{Languages, SortedVolumes, Volumes};
    use crate::backend::tracker::MangaTracker;
    use crate::global::test_utils::TrackerTest;
    use crate::view::widgets::press_key;

    fn tick<T: MangaProvider, S: MangaTracker>(app: &mut App<T, S>) {
        let max_amoun_ticks = 10;
        let mut count = 0;

        loop {
            if let Ok(event) = app.global_event_rx.try_recv() {
                app.handle_events(event);
            }

            if count > max_amoun_ticks {
                break;
            }
            count += 1;
        }
    }

    #[test]
    fn goes_to_home_page() {
        let mut app: App<MockMangaPageProvider, TrackerTest> =
            App::new(MockMangaPageProvider::new(), None, None, MockFiltersHandler::new(MockFilterState {}), MockWidgetFilter {});

        let first_event = app.global_event_rx.blocking_recv().expect("no event was sent");

        assert_eq!(Events::GoToHome, first_event);
        assert_eq!(app.current_tab, SelectedPage::Home);
    }

    #[test]
    fn can_go_to_search_page_by_pressing_gs() {
        let mut app: App<MockMangaPageProvider, TrackerTest> =
            App::new(MockMangaPageProvider::new(), None, None, MockFiltersHandler::new(MockFilterState {}), MockWidgetFilter {});

        press_key(&mut app, KeyCode::Char('g'));
        press_key(&mut app, KeyCode::Char('s'));

        tick(&mut app);

        assert_eq!(app.current_tab, SelectedPage::Search);
    }

    #[test]
    fn can_go_to_home_by_pressing_gh() {
        let mut app: App<MockMangaPageProvider, TrackerTest> =
            App::new(MockMangaPageProvider::new(), None, None, MockFiltersHandler::new(MockFilterState {}), MockWidgetFilter {});

        app.go_search_page();

        press_key(&mut app, KeyCode::Char('g'));
        press_key(&mut app, KeyCode::Char('h'));

        tick(&mut app);

        assert_eq!(app.current_tab, SelectedPage::Home);
    }

    #[test]
    fn can_go_to_stats_by_pressing_gt() {
        let mut app: App<MockMangaPageProvider, TrackerTest> =
            App::new(MockMangaPageProvider::new(), None, None, MockFiltersHandler::new(MockFilterState {}), MockWidgetFilter {});

        press_key(&mut app, KeyCode::Char('g'));
        press_key(&mut app, KeyCode::Char('t'));

        tick(&mut app);

        assert_eq!(app.current_tab, SelectedPage::Stats);
    }

    #[test]
    fn doesnt_listen_to_key_events_if_it_is_downloading_all_chapters() {
        let mut app: App<MockMangaPageProvider, TrackerTest> =
            App::new(MockMangaPageProvider::new(), None, None, MockFiltersHandler::new(MockFilterState {}), MockWidgetFilter {})
                .with_manga_page();

        app.manga_page.as_mut().unwrap().start_downloading_all_chapters();

        press_key(&mut app, KeyCode::Char('g'));
        press_key(&mut app, KeyCode::Char('f'));
        press_key(&mut app, KeyCode::Char('g'));
        press_key(&mut app, KeyCode::Char('s'));
        press_key(&mut app, KeyCode::F(2));

        tick(&mut app);

        assert_eq!(app.current_tab, SelectedPage::Home)
    }

    #[test]
    fn reader_page_is_initialized_corectly() {
        let mut app: App<MockMangaPageProvider, TrackerTest> = App::new(
            MockMangaPageProvider::new(),
            None,
            Some(Picker::halfblocks()),
            MockFiltersHandler::new(MockFilterState {}),
            MockWidgetFilter {},
        )
        .with_manga_page();

        let chapter_to_read = ChapterToRead {
            id: "some_id".to_string(),
            title: "some_title".to_string(),
            number: 1.0,
            volume_number: Some("1".to_string()),
            num_page_bookmarked: None,
            language: Languages::default(),
            pages_url: vec!["local://page/test/0".parse().unwrap()],
        };

        let list_of_chapter: ListOfChapters = ListOfChapters {
            volumes: SortedVolumes::new(vec![Volumes {
                volume: "1".to_string(),
                ..Default::default()
            }]),
        };

        let manga_tracker = TrackerTest::new();

        app.go_to_read_chapter(
            chapter_to_read,
            MangaToRead {
                title: "some_title".to_string(),
                manga_id: "some_manga_id".to_string(),
                list: list_of_chapter.clone(),
            },
            Some(manga_tracker),
        );

        let reader_page = app.manga_reader_page.unwrap();

        assert!(reader_page.global_event_tx.is_some());
        assert_eq!(reader_page.list_of_chapters, list_of_chapter);
        assert_eq!(SelectedPage::ReaderTab, app.current_tab);
        assert!(reader_page.manga_tracker.is_some());
    }
}
