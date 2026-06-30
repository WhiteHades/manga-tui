use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use manga_tui::SearchTerm;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use reqwest::Url;

use super::{
    Chapter, ChapterFilters, ChapterOrderBy, ChapterPageUrl, ChapterToRead, DecodeBytesToImage, FeedPageProvider,
    FetchChapterBookmarked, FiltersHandler, FiltersWidget, GetChapterPages, GetChaptersResponse, GetMangasResponse, GetRawImage,
    GoToReadChapter, HomePageMangaProvider, Languages, LatestChapter, ListOfChapters, Manga, MangaPageProvider, MangaProvider,
    MangaProviders, MangaStatus, Pagination, PopularManga, ProviderIdentity, ReaderPageProvider, RecentlyAddedManga,
    SearchChapterById, SearchManga, SearchMangaById, SearchMangaPanel, SearchPageProvider, SortedChapters, SortedVolumes, Volumes,
};
use crate::backend::APP_DATA_DIR;
use crate::backend::database::ChapterBookmarked;
use crate::backend::tui::Events;
use crate::config::ImageQuality;
use crate::view::widgets::StatefulWidgetFrame;

const LOCAL_CACHE_DIR: &str = "localCache";

#[derive(Clone, Debug, Default)]
pub struct LocalFiltersProvider {
    is_open: bool,
}

impl LocalFiltersProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

impl super::EventHandler for LocalFiltersProvider {
    fn handle_events(&mut self, events: Events) {
        if let Events::Key(key_event) = events {
            match key_event.code {
                crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('f') => self.toggle(),
                _ => {},
            }
        }
    }
}

impl FiltersHandler for LocalFiltersProvider {
    type InnerState = ();

    fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    fn is_open(&self) -> bool {
        self.is_open
    }

    fn is_typing(&self) -> bool {
        false
    }

    fn get_state(&self) -> &Self::InnerState {
        &()
    }
}

#[derive(Clone, Debug, Default)]
pub struct LocalFilterWidget;

impl LocalFilterWidget {
    pub fn new() -> Self {
        Self
    }
}

impl StatefulWidgetFrame for LocalFilterWidget {
    type State = LocalFiltersProvider;

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _state: &mut Self::State) {
        let paragraph = Paragraph::new("Local library search has no provider filters. Press <Esc> or <f> to close.")
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title("Local filters"));

        Widget::render(paragraph, area, frame.buffer_mut());
    }
}

impl FiltersWidget for LocalFilterWidget {
    type FilterState = LocalFiltersProvider;
}

#[derive(Clone, Debug)]
struct LocalPage {
    url: Url,
    extension: String,
}

#[derive(Clone, Debug)]
struct LocalChapter {
    id: String,
    title: String,
    number: String,
    source_path: PathBuf,
    pages: Vec<LocalPage>,
}

#[derive(Clone, Debug)]
struct LocalManga {
    id: String,
    title: String,
    source_path: PathBuf,
    cover_img_url: String,
    chapters: Vec<LocalChapter>,
}

#[derive(Clone, Debug)]
pub struct LocalProvider {
    library_path: PathBuf,
    mangas: Arc<Vec<LocalManga>>,
}

impl LocalProvider {
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, Box<dyn Error>> {
        let library_path = path.into().canonicalize()?;
        let mangas = scan_path(&library_path)?;

        if mangas.is_empty() {
            return Err(format!(
                "No readable local manga found in {}. Supported inputs: image folders, chapter folders, CBZ, and CBR.",
                library_path.display()
            )
            .into());
        }

        Ok(Self {
            library_path,
            mangas: Arc::new(mangas),
        })
    }

    fn find_manga(&self, manga_id: &str) -> Result<&LocalManga, Box<dyn Error>> {
        self.mangas
            .iter()
            .find(|manga| manga.id == manga_id)
            .ok_or_else(|| format!("Local manga not found: {manga_id}").into())
    }

    fn find_chapter(&self, chapter_id: &str, manga_id: &str) -> Result<(&LocalManga, &LocalChapter), Box<dyn Error>> {
        let manga = self.find_manga(manga_id)?;
        let chapter = manga
            .chapters
            .iter()
            .find(|chapter| chapter.id == chapter_id)
            .ok_or_else(|| format!("Local chapter not found: {chapter_id}"))?;

        Ok((manga, chapter))
    }

    fn chapter_to_read(&self, chapter: &LocalChapter, page_bookmarked: Option<u32>) -> ChapterToRead {
        ChapterToRead {
            id: chapter.id.clone(),
            title: chapter.title.clone(),
            number: chapter.number.parse().unwrap_or(1.0),
            volume_number: Some("1".to_string()),
            num_page_bookmarked: page_bookmarked,
            language: Languages::English,
            pages_url: chapter.pages.iter().map(|page| page.url.clone()).collect(),
        }
    }

    fn list_of_chapters(manga: &LocalManga) -> ListOfChapters {
        ListOfChapters {
            volumes: SortedVolumes::new(vec![Volumes {
                volume: "1".to_string(),
                chapters: SortedChapters::new(
                    manga
                        .chapters
                        .iter()
                        .map(|chapter| super::ChapterReader {
                            id: chapter.id.clone(),
                            number: chapter.number.clone(),
                            volume: "1".to_string(),
                        })
                        .collect(),
                ),
            }]),
        }
    }

    fn to_manga(manga: &LocalManga) -> Manga {
        Manga {
            id: manga.id.clone(),
            id_safe_for_download: manga.id.clone(),
            title: manga.title.clone(),
            genres: vec![],
            description: format!("Local manga from {}", manga.source_path.display()),
            status: MangaStatus::Completed,
            cover_img_url: manga.cover_img_url.clone(),
            languages: vec![Languages::English],
            rating: "local".to_string(),
            artist: None,
            author: None,
        }
    }

    fn to_search_manga(manga: &LocalManga) -> SearchManga {
        SearchManga {
            id: manga.id.clone(),
            title: manga.title.clone(),
            genres: vec![],
            description: Some(format!("{} local chapters", manga.chapters.len())),
            status: Some(MangaStatus::Completed),
            cover_img_url: manga.cover_img_url.clone(),
            languages: vec![Languages::English],
            artist: None,
            author: None,
        }
    }

    fn to_chapter(manga_id: &str, chapter: &LocalChapter) -> Chapter {
        Chapter {
            id: chapter.id.clone(),
            id_safe_for_download: chapter.id.clone(),
            manga_id: manga_id.to_string(),
            title: chapter.title.clone(),
            language: Languages::English,
            chapter_number: chapter.number.clone(),
            volume_number: Some("1".to_string()),
            scanlator: Some("Local".to_string()),
            publication_date: None,
        }
    }
}

impl ProviderIdentity for LocalProvider {
    fn name(&self) -> MangaProviders {
        MangaProviders::Local
    }
}

impl GetRawImage for LocalProvider {
    async fn get_raw_image(&self, url: &str) -> Result<Bytes, Box<dyn Error>> {
        let url = Url::parse(url)?;
        let path = url
            .to_file_path()
            .map_err(|_| format!("Local provider can only read file URLs, got {url}"))?;

        Ok(Bytes::from(fs::read(path)?))
    }
}

impl DecodeBytesToImage for LocalProvider {}
impl SearchMangaPanel for LocalProvider {}

impl SearchMangaById for LocalProvider {
    async fn get_manga_by_id(&self, manga_id: &str) -> Result<Manga, Box<dyn Error>> {
        Ok(Self::to_manga(self.find_manga(manga_id)?))
    }
}

impl HomePageMangaProvider for LocalProvider {
    async fn get_popular_mangas(&self) -> Result<Vec<PopularManga>, Box<dyn Error>> {
        Ok(self
            .mangas
            .iter()
            .map(|manga| PopularManga {
                id: manga.id.clone(),
                title: manga.title.clone(),
                genres: vec![],
                description: format!("{} local chapters", manga.chapters.len()),
                status: Some(MangaStatus::Completed),
                cover_img_url: manga.cover_img_url.clone(),
            })
            .collect())
    }

    async fn get_recently_added_mangas(&self) -> Result<Vec<RecentlyAddedManga>, Box<dyn Error>> {
        Ok(self
            .mangas
            .iter()
            .map(|manga| RecentlyAddedManga {
                id: manga.id.clone(),
                title: manga.title.clone(),
                description: format!("Local manga from {}", manga.source_path.display()),
                cover_img_url: manga.cover_img_url.clone(),
            })
            .collect())
    }
}

impl SearchPageProvider for LocalProvider {
    type FiltersHandler = LocalFiltersProvider;
    type InnerState = ();
    type Widget = LocalFilterWidget;

    async fn search_mangas(
        &self,
        search_term: Option<SearchTerm>,
        _filters: Self::InnerState,
        pagination: Pagination,
    ) -> Result<GetMangasResponse, Box<dyn Error>> {
        let mut mangas: Vec<SearchManga> = self
            .mangas
            .iter()
            .filter(|manga| {
                search_term
                    .as_ref()
                    .map(|term| manga.title.to_lowercase().contains(term.get()))
                    .unwrap_or(true)
            })
            .map(Self::to_search_manga)
            .collect();

        mangas.sort_by(|a, b| natural_cmp(&a.title, &b.title));

        let total_mangas = mangas.len() as u32;
        let from = pagination.index_to_slice_from();
        let to = pagination.to_index(mangas.len());
        let mangas = mangas.get(from..to).unwrap_or(&[]).to_vec();

        Ok(GetMangasResponse {
            mangas,
            total_mangas,
            next_page: pagination.current_page * pagination.items_per_page < total_mangas,
        })
    }
}

impl MangaPageProvider for LocalProvider {
    async fn get_chapters(
        &self,
        manga_id: &str,
        filters: ChapterFilters,
        pagination: Pagination,
    ) -> Result<GetChaptersResponse, Box<dyn Error>> {
        let manga = self.find_manga(manga_id)?;
        let mut chapters: Vec<Chapter> = manga.chapters.iter().map(|chapter| Self::to_chapter(manga_id, chapter)).collect();

        if filters.order == ChapterOrderBy::Descending {
            chapters.reverse();
        }

        let total_chapters = chapters.len() as u32;
        let from = pagination.index_to_slice_from();
        let to = pagination.to_index(chapters.len());
        let chapters = chapters.get(from..to).unwrap_or(&[]).to_vec();

        Ok(GetChaptersResponse {
            chapters,
            total_chapters,
        })
    }

    async fn get_all_chapters(&self, manga_id: &str, _language: Languages) -> Result<Vec<Chapter>, Box<dyn Error>> {
        let manga = self.find_manga(manga_id)?;
        Ok(manga.chapters.iter().map(|chapter| Self::to_chapter(manga_id, chapter)).collect())
    }
}

impl GetChapterPages for LocalProvider {
    async fn get_chapter_pages_url_with_extension(
        &self,
        chapter_id: &str,
        manga_id: &str,
        _image_quality: ImageQuality,
    ) -> Result<Vec<ChapterPageUrl>, Box<dyn Error>> {
        let (_, chapter) = self.find_chapter(chapter_id, manga_id)?;
        Ok(chapter
            .pages
            .iter()
            .map(|page| ChapterPageUrl {
                url: page.url.clone(),
                extension: page.extension.clone(),
            })
            .collect())
    }
}

impl GoToReadChapter for LocalProvider {
    async fn read_chapter(&self, chapter_id: &str, manga_id: &str) -> Result<(ChapterToRead, ListOfChapters), Box<dyn Error>> {
        let (manga, chapter) = self.find_chapter(chapter_id, manga_id)?;
        Ok((self.chapter_to_read(chapter, None), Self::list_of_chapters(manga)))
    }
}

impl SearchChapterById for LocalProvider {
    async fn search_chapter(&self, chapter_id: &str, manga_id: &str) -> Result<ChapterToRead, Box<dyn Error>> {
        let (_, chapter) = self.find_chapter(chapter_id, manga_id)?;
        Ok(self.chapter_to_read(chapter, None))
    }
}

impl FetchChapterBookmarked for LocalProvider {
    async fn fetch_chapter_bookmarked(
        &self,
        chapter: ChapterBookmarked,
    ) -> Result<(ChapterToRead, ListOfChapters), Box<dyn Error>> {
        let (manga, local_chapter) = self.find_chapter(&chapter.id, &chapter.manga_id)?;
        Ok((self.chapter_to_read(local_chapter, chapter.number_page_bookmarked), Self::list_of_chapters(manga)))
    }
}

impl FeedPageProvider for LocalProvider {
    async fn get_latest_chapters(&self, manga_id: &str) -> Result<Vec<LatestChapter>, Box<dyn Error>> {
        let manga = self.find_manga(manga_id)?;
        Ok(manga
            .chapters
            .iter()
            .rev()
            .take(5)
            .map(|chapter| LatestChapter {
                id: chapter.id.clone(),
                manga_id: manga.id.clone(),
                title: chapter.title.clone(),
                language: Languages::English,
                chapter_number: chapter.number.clone(),
                volume_number: Some("1".to_string()),
                publication_date: None,
            })
            .collect())
    }
}

impl ReaderPageProvider for LocalProvider {}
impl MangaProvider for LocalProvider {}

fn scan_path(path: &Path) -> Result<Vec<LocalManga>, Box<dyn Error>> {
    if path.is_file() {
        return Ok(vec![manga_from_single_source(path)?]);
    }

    scan_directory(path)
}

fn scan_directory(path: &Path) -> Result<Vec<LocalManga>, Box<dyn Error>> {
    let direct_images = collect_images_shallow(path)?;
    if !direct_images.is_empty() {
        return Ok(vec![manga_from_image_folder(path, direct_images)?]);
    }

    let direct_archives = collect_archives_shallow(path)?;
    let chapter_dirs = collect_chapter_dirs(path)?;

    if !direct_archives.is_empty() || !chapter_dirs.is_empty() {
        let manga_id = local_id(path);
        let mut chapters = vec![];

        for source in chapter_dirs.into_iter().chain(direct_archives.into_iter()) {
            let number = chapters.len() + 1;
            chapters.push(chapter_from_source(&source, &manga_id, number)?);
        }

        let cover_img_url = find_cover_url(path)?
            .or_else(|| {
                chapters
                    .first()
                    .and_then(|chapter| chapter.pages.first())
                    .map(|page| page.url.to_string())
            })
            .unwrap_or_default();

        return Ok(vec![LocalManga {
            id: manga_id,
            title: title_from_path(path),
            source_path: path.to_path_buf(),
            cover_img_url,
            chapters,
        }]);
    }

    let mut mangas = vec![];
    for entry in sorted_dir_entries(path)? {
        if entry.path().is_dir() {
            mangas.extend(scan_directory(&entry.path())?);
        } else if is_supported_archive(&entry.path()) {
            mangas.push(manga_from_single_source(&entry.path())?);
        }
    }

    Ok(mangas)
}

fn manga_from_single_source(path: &Path) -> Result<LocalManga, Box<dyn Error>> {
    if path.is_dir() {
        let images = collect_images_recursive(path)?;
        return manga_from_image_folder(path, images);
    }

    if !is_supported_archive(path) {
        return Err(format!("Unsupported local manga file: {}", path.display()).into());
    }

    let manga_id = local_id(path);
    let chapter = chapter_from_source(path, &manga_id, 1)?;
    let cover_img_url = find_cover_url(path)?
        .or_else(|| chapter.pages.first().map(|page| page.url.to_string()))
        .unwrap_or_default();

    Ok(LocalManga {
        id: manga_id,
        title: title_from_path(path),
        source_path: path.to_path_buf(),
        cover_img_url,
        chapters: vec![chapter],
    })
}

fn manga_from_image_folder(path: &Path, images: Vec<PathBuf>) -> Result<LocalManga, Box<dyn Error>> {
    let manga_id = local_id(path);
    let chapter = chapter_from_images(path, &manga_id, 1, images)?;
    let cover_img_url = find_cover_url(path)?
        .or_else(|| chapter.pages.first().map(|page| page.url.to_string()))
        .unwrap_or_default();

    Ok(LocalManga {
        id: manga_id,
        title: title_from_path(path),
        source_path: path.to_path_buf(),
        cover_img_url,
        chapters: vec![chapter],
    })
}

fn chapter_from_source(path: &Path, manga_id: &str, number: usize) -> Result<LocalChapter, Box<dyn Error>> {
    if path.is_dir() {
        return chapter_from_images(path, manga_id, number, collect_images_recursive(path)?);
    }

    let extracted_path = extract_archive_to_cache(path)?;
    chapter_from_images(path, manga_id, number, collect_images_recursive(&extracted_path)?)
}

fn chapter_from_images(
    source_path: &Path,
    manga_id: &str,
    number: usize,
    images: Vec<PathBuf>,
) -> Result<LocalChapter, Box<dyn Error>> {
    if images.is_empty() {
        return Err(format!("No readable images found in {}", source_path.display()).into());
    }

    let pages = images
        .into_iter()
        .map(|path| {
            let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("jpg").to_string();
            let url = Url::from_file_path(&path).map_err(|_| format!("Could not build file URL for {}", path.display()))?;
            Ok(LocalPage { url, extension })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    Ok(LocalChapter {
        id: local_id(source_path),
        title: title_from_path(source_path),
        number: number.to_string(),
        source_path: source_path.to_path_buf(),
        pages,
    })
}

fn extract_archive_to_cache(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let destination = local_cache_path(path)?;
    if !collect_images_recursive(&destination).unwrap_or_default().is_empty() {
        return Ok(destination);
    }

    fs::create_dir_all(&destination)?;

    match extension(path).as_deref() {
        Some("cbz") => extract_cbz(path, &destination)?,
        Some("cbr") => extract_cbr(path, &destination)?,
        _ => return Err(format!("Unsupported local archive: {}", path.display()).into()),
    }

    Ok(destination)
}

fn extract_cbz(path: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.is_dir() || !is_supported_image_name(file.name()) {
            continue;
        }

        if let Some(output_path) = safe_archive_path(destination, file.name()) {
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::create(output_path)?;
            io::copy(&mut file, &mut output)?;
        }
    }

    Ok(())
}

fn extract_cbr(path: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let archive = rars::ArchiveReader::read_path(path)?;
    archive.extract_to(None, |meta| {
        let name = meta.name_lossy();
        if meta.is_directory || !is_supported_image_name(&name) {
            return Ok(Box::new(io::sink()));
        }

        let Some(output_path) = safe_archive_path(destination, &name) else {
            return Ok(Box::new(io::sink()));
        };

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }

        Ok(Box::new(File::create(output_path)?))
    })?;

    Ok(())
}

fn local_cache_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let data_dir = APP_DATA_DIR.as_ref().ok_or("manga-tui data directory is not available")?;
    let data_dir = if data_dir.is_absolute() { data_dir.clone() } else { std::env::current_dir()?.join(data_dir) };
    Ok(data_dir.join(LOCAL_CACHE_DIR).join(local_id(path)))
}

fn safe_archive_path(destination: &Path, name: &str) -> Option<PathBuf> {
    let mut output = destination.to_path_buf();
    let mut has_component = false;

    for component in Path::new(name).components() {
        if let Component::Normal(part) = component {
            output.push(part);
            has_component = true;
        }
    }

    has_component.then_some(output)
}

fn find_cover_url(path: &Path) -> Result<Option<String>, Box<dyn Error>> {
    if !path.is_dir() {
        return Ok(None);
    }

    let cover = sorted_dir_entries(path)?.into_iter().map(|entry| entry.path()).find(|path| {
        path.is_file()
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.eq_ignore_ascii_case("cover"))
                .unwrap_or(false)
            && is_supported_image_path(path)
    });

    match cover {
        Some(path) => Url::from_file_path(&path)
            .map(|url| Some(url.to_string()))
            .map_err(|_| format!("Could not build file URL for {}", path.display()).into()),
        None => Ok(None),
    }
}

fn collect_chapter_dirs(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut dirs = vec![];
    for entry in sorted_dir_entries(path)? {
        let path = entry.path();
        if path.is_dir() && !collect_images_shallow(&path)?.is_empty() {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

fn collect_archives_shallow(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    Ok(sorted_dir_entries(path)?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported_archive(path))
        .collect())
}

fn collect_images_shallow(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    Ok(sorted_dir_entries(path)?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported_image_path(path))
        .collect())
}

fn collect_images_recursive(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let mut images = vec![];
    let mut dirs = vec![path.to_path_buf()];

    while let Some(dir) = dirs.pop() {
        for entry in sorted_dir_entries(&dir)? {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if is_supported_image_path(&path) {
                images.push(path);
            }
        }
    }

    images.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    Ok(images)
}

fn sorted_dir_entries(path: &Path) -> Result<Vec<fs::DirEntry>, Box<dyn Error>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|a, b| natural_cmp(&a.file_name().to_string_lossy(), &b.file_name().to_string_lossy()));
    Ok(entries)
}

fn is_supported_archive(path: &Path) -> bool {
    matches!(extension(path).as_deref(), Some("cbz" | "cbr"))
}

fn is_supported_image_path(path: &Path) -> bool {
    extension(path)
        .map(|extension| matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "avif"))
        .unwrap_or(false)
}

fn is_supported_image_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "avif")
        })
        .unwrap_or(false)
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn title_from_path(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("Local Manga")
        .replace(['_', '-'], " ")
}

fn local_id(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("local-{:x}", hasher.finish())
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();

    loop {
        match (left.peek(), right.peek()) {
            (Some(l), Some(r)) if l.is_ascii_digit() && r.is_ascii_digit() => {
                let left_num = take_number(&mut left);
                let right_num = take_number(&mut right);
                let ordering = left_num.cmp(&right_num);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            },
            (Some(_), Some(_)) => {
                let l = left.next().unwrap().to_ascii_lowercase();
                let r = right.next().unwrap().to_ascii_lowercase();
                let ordering = l.cmp(&r);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            },
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut value = String::new();
    while chars.peek().is_some_and(|char| char.is_ascii_digit()) {
        value.push(chars.next().unwrap());
    }
    value.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    use image::{ImageFormat, Rgb, RgbImage};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("manga-tui-{name}-{timestamp}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).ok();
        }
    }

    fn png_bytes() -> Result<Vec<u8>, Box<dyn Error>> {
        let image = RgbImage::from_pixel(4, 6, Rgb([10, 20, 30]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png)?;
        Ok(bytes.into_inner())
    }

    fn write_png(path: &Path) -> Result<(), Box<dyn Error>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, png_bytes()?)?;
        Ok(())
    }

    fn write_cbz(path: &Path) -> Result<(), Box<dyn Error>> {
        let file = File::create(path)?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("10.png", options)?;
        zip.write_all(&png_bytes()?)?;
        zip.start_file("2.png", options)?;
        zip.write_all(&png_bytes()?)?;
        zip.finish()?;
        Ok(())
    }

    #[test]
    fn natural_sort_orders_numbered_files() {
        let mut files = ["10.jpg", "2.jpg", "1.jpg"];
        files.sort_by(|a, b| natural_cmp(a, b));

        assert_eq!(["1.jpg", "2.jpg", "10.jpg"], files);
    }

    #[test]
    fn scans_image_folder_as_single_manga_with_one_chapter() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("image-folder");
        let manga_dir = dir.path.join("One Shot");

        write_png(&manga_dir.join("10.png"))?;
        write_png(&manga_dir.join("2.png"))?;

        let provider = LocalProvider::from_path(&manga_dir)?;
        let manga = &provider.mangas[0];

        assert_eq!(1, provider.mangas.len());
        assert_eq!("One Shot", manga.title);
        assert_eq!(1, manga.chapters.len());
        assert_eq!(2, manga.chapters[0].pages.len());
        assert!(manga.chapters[0].pages[0].url.path().ends_with("2.png"));

        Ok(())
    }

    #[test]
    fn scans_folder_of_chapter_folders_as_one_manga() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("chapter-folders");
        let manga_dir = dir.path.join("Series");

        write_png(&manga_dir.join("Chapter 1").join("1.png"))?;
        write_png(&manga_dir.join("Chapter 2").join("1.png"))?;

        let provider = LocalProvider::from_path(&manga_dir)?;
        let manga = &provider.mangas[0];

        assert_eq!(1, provider.mangas.len());
        assert_eq!("Series", manga.title);
        assert_eq!(2, manga.chapters.len());
        assert_eq!("Chapter 1", manga.chapters[0].title);
        assert_eq!("Chapter 2", manga.chapters[1].title);

        Ok(())
    }

    #[test]
    fn scans_folder_of_manga_folders_as_library() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("library");

        write_png(&dir.path.join("Series A").join("Chapter 1").join("1.png"))?;
        write_png(&dir.path.join("Series B").join("Chapter 1").join("1.png"))?;

        let provider = LocalProvider::from_path(&dir.path)?;

        assert_eq!(2, provider.mangas.len());
        assert_eq!("Series A", provider.mangas[0].title);
        assert_eq!("Series B", provider.mangas[1].title);
        assert_eq!(1, provider.mangas[0].chapters.len());
        assert_eq!(1, provider.mangas[1].chapters.len());

        Ok(())
    }

    #[test]
    fn scans_single_cbz_as_one_manga() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("cbz");
        let cbz = dir.path.join("Archive.cbz");

        write_cbz(&cbz)?;

        let provider = LocalProvider::from_path(&cbz)?;
        let manga = &provider.mangas[0];

        assert_eq!(1, provider.mangas.len());
        assert_eq!("Archive", manga.title);
        assert_eq!(1, manga.chapters.len());
        assert_eq!(2, manga.chapters[0].pages.len());
        assert!(manga.chapters[0].pages[0].url.path().ends_with("2.png"));

        Ok(())
    }

    #[test]
    fn safe_archive_path_ignores_parent_components() {
        let path = safe_archive_path(Path::new("/tmp/cache"), "../chapter/1.jpg").unwrap();

        assert_eq!(Path::new("/tmp/cache/chapter/1.jpg"), path);
    }
}
