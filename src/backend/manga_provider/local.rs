use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use bytes::Bytes;
use manga_tui::SearchTerm;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use url::Url;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalLibraryStats {
    pub library_path: PathBuf,
    pub manga_count: usize,
    pub volume_count: usize,
    pub chapter_count: usize,
    pub page_count: usize,
    pub image_folder_count: usize,
    pub cbz_count: usize,
    pub cbr_count: usize,
    pub epub_count: usize,
    pub total_bytes: u64,
}

impl LocalLibraryStats {
    pub fn archive_count(&self) -> usize {
        self.cbz_count + self.cbr_count + self.epub_count
    }
}

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
    source: LocalPageSource,
}

#[derive(Clone, Debug)]
enum LocalPageSource {
    File(PathBuf),
    ZipEntry {
        archive_path: PathBuf,
        entry_name: String,
    },
    RarEntry {
        archive_path: PathBuf,
        entry_name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalChapterFormat {
    ImageFolder,
    Cbz,
    Cbr,
    Epub,
}

#[derive(Clone, Debug)]
struct LocalChapter {
    id: String,
    title: String,
    number: String,
    volume: Option<String>,
    source_path: PathBuf,
    format: LocalChapterFormat,
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
    library: Arc<RwLock<LocalLibrary>>,
}

#[derive(Clone, Debug)]
struct LocalLibrary {
    library_path: PathBuf,
    mangas: Vec<LocalManga>,
    manga_index: HashMap<String, usize>,
    page_sources: HashMap<String, LocalPageSource>,
    stats: LocalLibraryStats,
}

impl LocalProvider {
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            library: Arc::new(RwLock::new(Self::load_library(path.into())?)),
        })
    }

    fn load_library(path: PathBuf) -> Result<LocalLibrary, Box<dyn Error>> {
        let library_path = path.canonicalize()?;
        let mangas = scan_path(&library_path)?;

        if mangas.is_empty() {
            return Err(format!(
                "No readable local manga found in {}. Supported inputs: image folders, chapter folders, CBZ, CBR, ZIP, RAR, and EPUB.",
                library_path.display()
            )
            .into());
        }

        let (manga_index, page_sources, stats) = build_library_index(&library_path, &mangas);

        Ok(LocalLibrary {
            library_path,
            mangas,
            manga_index,
            page_sources,
            stats,
        })
    }

    pub fn reload_from_path(&self, path: impl Into<PathBuf>) -> Result<(), Box<dyn Error>> {
        let library = Self::load_library(path.into())?;
        *self.library.write().map_err(|_| "Local library lock poisoned")? = library;
        Ok(())
    }

    pub fn library_path(&self) -> PathBuf {
        self.library.read().expect("local library lock poisoned").library_path.clone()
    }

    pub fn library_stats(&self) -> LocalLibraryStats {
        self.library.read().expect("local library lock poisoned").stats.clone()
    }

    fn mangas(&self) -> Vec<LocalManga> {
        self.library.read().expect("local library lock poisoned").mangas.clone()
    }

    fn find_manga(&self, manga_id: &str) -> Result<LocalManga, Box<dyn Error>> {
        let library = self.library.read().map_err(|_| "Local library lock poisoned")?;
        library
            .manga_index
            .get(manga_id)
            .and_then(|index| library.mangas.get(*index))
            .cloned()
            .ok_or_else(|| format!("Local manga not found: {manga_id}").into())
    }

    fn find_chapter(&self, chapter_id: &str, manga_id: &str) -> Result<(LocalManga, LocalChapter), Box<dyn Error>> {
        let manga = self.find_manga(manga_id)?;
        let chapter = manga
            .chapters
            .iter()
            .find(|chapter| chapter.id == chapter_id)
            .cloned()
            .ok_or_else(|| format!("Local chapter not found: {chapter_id}"))?;

        Ok((manga, chapter))
    }

    fn chapter_to_read(&self, chapter: &LocalChapter, page_bookmarked: Option<u32>) -> ChapterToRead {
        ChapterToRead {
            id: chapter.id.clone(),
            title: chapter.title.clone(),
            number: chapter.number.parse().unwrap_or(1.0),
            volume_number: chapter.volume.clone(),
            num_page_bookmarked: page_bookmarked,
            language: Languages::English,
            pages_url: chapter.pages.iter().map(|page| page.url.clone()).collect(),
        }
    }

    fn list_of_chapters(manga: &LocalManga) -> ListOfChapters {
        let mut grouped_chapters: BTreeMap<String, Vec<super::ChapterReader>> = BTreeMap::new();
        for chapter in &manga.chapters {
            let volume = chapter.volume.clone().unwrap_or_else(|| "none".to_string());
            grouped_chapters.entry(volume.clone()).or_default().push(super::ChapterReader {
                id: chapter.id.clone(),
                number: chapter.number.clone(),
                volume,
            });
        }

        let mut volumes: Vec<Volumes> = grouped_chapters
            .into_iter()
            .map(|(volume, chapters)| Volumes {
                volume,
                chapters: SortedChapters::new(chapters),
            })
            .collect();
        volumes.sort_by(|a, b| chapter_number_cmp(&a.volume, &b.volume));

        ListOfChapters {
            volumes: SortedVolumes::new(volumes),
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
            volume_number: chapter.volume.clone(),
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
        let source = self
            .library
            .read()
            .map_err(|_| "Local library lock poisoned")?
            .page_sources
            .get(url)
            .cloned();

        if let Some(source) = source.as_ref() {
            return read_local_page_source(source);
        }

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
        Ok(Self::to_manga(&self.find_manga(manga_id)?))
    }
}

impl HomePageMangaProvider for LocalProvider {
    async fn get_popular_mangas(&self) -> Result<Vec<PopularManga>, Box<dyn Error>> {
        Ok(self
            .mangas()
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
            .mangas()
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
            .mangas()
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
        Ok((self.chapter_to_read(&chapter, None), Self::list_of_chapters(&manga)))
    }
}

impl SearchChapterById for LocalProvider {
    async fn search_chapter(&self, chapter_id: &str, manga_id: &str) -> Result<ChapterToRead, Box<dyn Error>> {
        let (_, chapter) = self.find_chapter(chapter_id, manga_id)?;
        Ok(self.chapter_to_read(&chapter, None))
    }
}

impl FetchChapterBookmarked for LocalProvider {
    async fn fetch_chapter_bookmarked(
        &self,
        chapter: ChapterBookmarked,
    ) -> Result<(ChapterToRead, ListOfChapters), Box<dyn Error>> {
        let (manga, local_chapter) = self.find_chapter(&chapter.id, &chapter.manga_id)?;
        Ok((self.chapter_to_read(&local_chapter, chapter.number_page_bookmarked), Self::list_of_chapters(&manga)))
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
                volume_number: chapter.volume.clone(),
                publication_date: None,
            })
            .collect())
    }
}

impl ReaderPageProvider for LocalProvider {}
impl MangaProvider for LocalProvider {
    fn local_library_stats(&self) -> Option<LocalLibraryStats> {
        Some(self.library_stats())
    }

    fn reload_local_library(&self, path: PathBuf) -> Result<(), Box<dyn Error>> {
        self.reload_from_path(path)
    }
}

fn build_library_index(
    library_path: &Path,
    mangas: &[LocalManga],
) -> (HashMap<String, usize>, HashMap<String, LocalPageSource>, LocalLibraryStats) {
    let mut manga_index = HashMap::with_capacity(mangas.len());
    let mut page_sources = HashMap::new();
    let mut counted_bytes = HashSet::new();
    let mut stats = LocalLibraryStats {
        library_path: library_path.to_path_buf(),
        manga_count: mangas.len(),
        ..Default::default()
    };

    for (manga_index_value, manga) in mangas.iter().enumerate() {
        manga_index.insert(manga.id.clone(), manga_index_value);
        let mut volumes = HashSet::new();

        for chapter in &manga.chapters {
            stats.chapter_count += 1;
            stats.page_count += chapter.pages.len();
            if let Some(volume) = chapter.volume.as_ref() {
                volumes.insert(volume.clone());
            }

            match chapter.format {
                LocalChapterFormat::ImageFolder => stats.image_folder_count += 1,
                LocalChapterFormat::Cbz => stats.cbz_count += 1,
                LocalChapterFormat::Cbr => stats.cbr_count += 1,
                LocalChapterFormat::Epub => stats.epub_count += 1,
            }

            match chapter.format {
                LocalChapterFormat::ImageFolder => {
                    for page in &chapter.pages {
                        if let LocalPageSource::File(path) = &page.source {
                            counted_bytes.insert(path.clone());
                        }
                    }
                },
                LocalChapterFormat::Cbz | LocalChapterFormat::Cbr | LocalChapterFormat::Epub => {
                    counted_bytes.insert(chapter.source_path.clone());
                },
            }

            for page in &chapter.pages {
                page_sources.insert(page.url.to_string(), page.source.clone());
            }
        }
        stats.volume_count += volumes.len();
    }

    stats.total_bytes = counted_bytes
        .into_iter()
        .filter_map(|path| fs::metadata(path).ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum();

    (manga_index, page_sources, stats)
}

fn scan_path(path: &Path) -> Result<Vec<LocalManga>, Box<dyn Error>> {
    if path.is_file() {
        return Ok(vec![manga_from_single_source(path)?]);
    }

    scan_directory(path)
}

fn scan_directory(path: &Path) -> Result<Vec<LocalManga>, Box<dyn Error>> {
    let direct_images = collect_images_shallow(path)?;
    let direct_archives = collect_archives_shallow(path)?;
    let chapter_dirs = collect_chapter_dirs(path)?;
    let volume_dirs = collect_volume_dirs(path)?;

    if !direct_images.is_empty() && direct_archives.is_empty() && chapter_dirs.is_empty() && volume_dirs.is_empty() {
        return Ok(vec![manga_from_image_folder(path, direct_images)?]);
    }

    if !direct_archives.is_empty() || !chapter_dirs.is_empty() || !volume_dirs.is_empty() {
        let manga_id = local_id(path);
        let mut chapters = vec![];

        for source in chapter_dirs.into_iter().chain(direct_archives.into_iter()) {
            let number = chapters.len() + 1;
            chapters.push(chapter_from_source(&source, &manga_id, number, None)?);
        }

        for volume_dir in volume_dirs {
            let volume = volume_number_from_path(&volume_dir);
            let sources = collect_chapter_sources_shallow(&volume_dir)?;
            for source in sources {
                let number = chapters.len() + 1;
                chapters.push(chapter_from_source(&source, &manga_id, number, volume.clone())?);
            }
        }
        chapters.sort_by(local_chapter_cmp);

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
    let chapter = chapter_from_source(path, &manga_id, 1, None)?;
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
    let chapter = chapter_from_images(path, &manga_id, 1, None, images)?;
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

fn chapter_from_source(path: &Path, manga_id: &str, number: usize, volume: Option<String>) -> Result<LocalChapter, Box<dyn Error>> {
    if path.is_dir() {
        return chapter_from_images(path, manga_id, number, volume, collect_images_recursive(path)?);
    }

    chapter_from_archive(path, manga_id, number, volume)
}

fn chapter_from_images(
    source_path: &Path,
    manga_id: &str,
    number: usize,
    default_volume: Option<String>,
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
            Ok(LocalPage {
                url,
                extension,
                source: LocalPageSource::File(path),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    let chapter_numbers = chapter_numbers_from_path(source_path, number, default_volume);

    Ok(LocalChapter {
        id: local_id(source_path),
        title: title_from_path(source_path),
        number: chapter_numbers.chapter,
        volume: chapter_numbers.volume,
        source_path: source_path.to_path_buf(),
        format: LocalChapterFormat::ImageFolder,
        pages,
    })
}

fn chapter_from_archive(
    path: &Path,
    manga_id: &str,
    number: usize,
    default_volume: Option<String>,
) -> Result<LocalChapter, Box<dyn Error>> {
    let chapter_id = local_id(path);
    let format = LocalChapterFormat::try_from_path(path)?;
    let entries = archive_image_entries(path, format)?;

    if entries.is_empty() {
        return Err(format!("No readable images found in {}", path.display()).into());
    }

    let pages = entries
        .into_iter()
        .enumerate()
        .map(|(index, entry_name)| {
            let extension = extension_from_name(&entry_name).unwrap_or_else(|| "jpg".to_string());
            let url = local_page_url(&chapter_id, index)?;
            let source = match format {
                LocalChapterFormat::Cbz | LocalChapterFormat::Epub => LocalPageSource::ZipEntry {
                    archive_path: path.to_path_buf(),
                    entry_name,
                },
                LocalChapterFormat::Cbr => LocalPageSource::RarEntry {
                    archive_path: path.to_path_buf(),
                    entry_name,
                },
                LocalChapterFormat::ImageFolder => unreachable!(),
            };

            Ok(LocalPage {
                url,
                extension,
                source,
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    let chapter_numbers = chapter_numbers_from_path(path, number, default_volume);

    Ok(LocalChapter {
        id: chapter_id,
        title: title_from_path(path),
        number: chapter_numbers.chapter,
        volume: chapter_numbers.volume,
        source_path: path.to_path_buf(),
        format,
        pages,
    })
}

impl LocalChapterFormat {
    fn try_from_path(path: &Path) -> Result<Self, Box<dyn Error>> {
        match extension(path).as_deref() {
            Some("cbz" | "zip") => Ok(Self::Cbz),
            Some("cbr" | "rar") => Ok(Self::Cbr),
            Some("epub") => Ok(Self::Epub),
            _ => Err(format!("Unsupported local archive: {}", path.display()).into()),
        }
    }
}

fn archive_image_entries(path: &Path, format: LocalChapterFormat) -> Result<Vec<String>, Box<dyn Error>> {
    match format {
        LocalChapterFormat::Cbz | LocalChapterFormat::Epub => zip_image_entries(path),
        LocalChapterFormat::Cbr => rar_image_entries(path),
        LocalChapterFormat::ImageFolder => Ok(vec![]),
    }
}

fn zip_image_entries(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entries = vec![];

    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        if file.is_dir() || !is_supported_image_name(file.name()) {
            continue;
        }

        entries.push(file.name().to_string());
    }

    entries.sort_by(|a, b| natural_cmp(a, b));
    Ok(entries)
}

fn rar_image_entries(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let archive = rars::ArchiveReader::read_path(path)?;
    let mut entries: Vec<String> = archive
        .members()
        .filter(|member| !member.meta.is_directory)
        .map(|member| member.meta.name_lossy())
        .filter(|name| is_supported_image_name(name))
        .collect();

    entries.sort_by(|a, b| natural_cmp(a, b));
    Ok(entries)
}

fn local_page_url(chapter_id: &str, page_index: usize) -> Result<Url, Box<dyn Error>> {
    Ok(Url::parse(&format!("local://page/{chapter_id}/{page_index}"))?)
}

fn read_local_page_source(source: &LocalPageSource) -> Result<Bytes, Box<dyn Error>> {
    match source {
        LocalPageSource::File(path) => Ok(Bytes::from(fs::read(path)?)),
        LocalPageSource::ZipEntry {
            archive_path,
            entry_name,
        } => read_zip_entry(archive_path, entry_name).map(Bytes::from),
        LocalPageSource::RarEntry {
            archive_path,
            entry_name,
        } => read_rar_entry(archive_path, entry_name).map(Bytes::from),
    }
}

fn read_zip_entry(path: &Path, entry_name: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive.by_name(entry_name)?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    io::copy(&mut entry, &mut bytes)?;
    Ok(bytes)
}

fn read_rar_entry(path: &Path, entry_name: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let output_path = archive_entry_cache_path(path, entry_name)?;

    if output_path.exists() {
        return Ok(fs::read(output_path)?);
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let archive = rars::ArchiveReader::read_path(path)?;
    archive.extract_to(None, |meta| {
        if meta.name_lossy() != entry_name {
            return Ok(Box::new(io::sink()));
        }

        Ok(Box::new(File::create(&output_path)?))
    })?;

    Ok(fs::read(output_path)?)
}

fn local_cache_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let data_dir = APP_DATA_DIR.as_ref().ok_or("manga-tui data directory is not available")?;
    let data_dir = if data_dir.is_absolute() { data_dir.clone() } else { std::env::current_dir()?.join(data_dir) };
    Ok(data_dir.join(LOCAL_CACHE_DIR).join(local_id(path)))
}

fn archive_entry_cache_path(path: &Path, entry_name: &str) -> Result<PathBuf, Box<dyn Error>> {
    safe_archive_path(&local_cache_path(path)?, entry_name)
        .ok_or_else(|| format!("Could not build safe cache path for archive entry {entry_name}").into())
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
        if path.is_dir() && !looks_like_volume_path(&path) && !collect_images_shallow(&path)?.is_empty() {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

fn collect_volume_dirs(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut dirs = vec![];
    for entry in sorted_dir_entries(path)? {
        let path = entry.path();
        if path.is_dir() && looks_like_volume_path(&path) && !collect_chapter_sources_shallow(&path)?.is_empty() {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

fn collect_chapter_sources_shallow(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut sources = vec![];
    for entry in sorted_dir_entries(path)? {
        let path = entry.path();
        if path.is_file() && is_supported_archive(&path) {
            sources.push(path);
        } else if path.is_dir() && !collect_images_shallow(&path)?.is_empty() {
            sources.push(path);
        }
    }
    sources.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    Ok(sources)
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
    entries.retain(|entry| entry.file_name().to_str().map(|name| !name.starts_with('.')).unwrap_or(true));
    entries.sort_by(|a, b| natural_cmp(&a.file_name().to_string_lossy(), &b.file_name().to_string_lossy()));
    Ok(entries)
}

fn is_supported_archive(path: &Path) -> bool {
    matches!(extension(path).as_deref(), Some("cbz" | "cbr" | "epub" | "zip" | "rar"))
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

fn extension_from_name(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalChapterNumbers {
    volume: Option<String>,
    chapter: String,
}

fn chapter_numbers_from_path(path: &Path, fallback: usize, default_volume: Option<String>) -> LocalChapterNumbers {
    let title = title_from_path(path);
    let volume = volume_number_from_title(&title).or(default_volume);
    let chapter = chapter_number_from_title(&title)
        .or_else(|| number_after_volume(&title, volume.as_deref()))
        .unwrap_or_else(|| fallback.to_string());

    LocalChapterNumbers { volume, chapter }
}

fn volume_number_from_path(path: &Path) -> Option<String> {
    volume_number_from_title(&title_from_path(path))
}

fn volume_number_from_title(title: &str) -> Option<String> {
    number_after_keywords(title, &["volume", "vol", "v"])
}

fn chapter_number_from_title(title: &str) -> Option<String> {
    number_after_keywords(title, &["chapter", "chap", "ch", "c"])
}

fn number_after_volume(title: &str, volume: Option<&str>) -> Option<String> {
    let numbers = all_numbers(title);
    match volume {
        Some(volume) => numbers.into_iter().find(|number| number != volume),
        None => numbers.into_iter().next(),
    }
}

fn looks_like_volume_path(path: &Path) -> bool {
    let title = title_from_path(path).to_ascii_lowercase();
    ["volume", "vol", "vol.", "v"]
        .iter()
        .any(|keyword| title.split_whitespace().any(|part| part == *keyword))
        || title.strip_prefix("vol").is_some_and(starts_with_number_marker)
        || title.starts_with('v') && title.chars().nth(1).is_some_and(|char| char.is_ascii_digit())
}

fn starts_with_number_marker(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character == '.' || character.is_ascii_digit())
}

fn first_number(value: &str) -> Option<String> {
    let mut number = String::new();
    let mut started = false;

    for character in value.chars() {
        if character.is_ascii_digit() || (started && character == '.') {
            number.push(character);
            started = true;
        } else if started {
            break;
        }
    }

    (!number.is_empty()).then_some(number)
}

fn number_after_keywords(value: &str, keywords: &[&str]) -> Option<String> {
    let normalized = normalize_number_tokens(value);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();

    for (index, token) in tokens.iter().enumerate() {
        let token = token.trim_matches('.');
        if keywords.contains(&token)
            && let Some(number) = tokens.get(index + 1).and_then(|token| first_number(token))
        {
            return Some(number);
        }
    }

    for keyword in keywords {
        let compact_prefix = format!("{keyword}.");
        for token in &tokens {
            let token = token.trim_matches(['[', ']', '(', ')']);
            if let Some(rest) = token.strip_prefix(keyword).or_else(|| token.strip_prefix(&compact_prefix))
                && let Some(number) = first_number(rest)
            {
                return Some(number);
            }
        }
    }

    None
}

fn normalize_number_tokens(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|character| match character {
            '_' | '-' | '[' | ']' | '(' | ')' => ' ',
            _ => character,
        })
        .collect()
}

fn all_numbers(value: &str) -> Vec<String> {
    let mut numbers = vec![];
    let mut current = String::new();

    for character in value.chars() {
        if character.is_ascii_digit() || (!current.is_empty() && character == '.') {
            current.push(character);
        } else if !current.is_empty() {
            numbers.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        numbers.push(current);
    }

    numbers
}

fn local_chapter_cmp(a: &LocalChapter, b: &LocalChapter) -> Ordering {
    chapter_number_cmp(a.volume.as_deref().unwrap_or("0"), b.volume.as_deref().unwrap_or("0"))
        .then_with(|| chapter_number_cmp(&a.number, &b.number))
        .then_with(|| natural_cmp(&a.title, &b.title))
}

fn chapter_number_cmp(a: &str, b: &str) -> Ordering {
    a.parse::<f64>().unwrap_or(0.0).total_cmp(&b.parse::<f64>().unwrap_or(0.0))
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

    fn write_epub(path: &Path) -> Result<(), Box<dyn Error>> {
        let file = File::create(path)?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("META-INF/container.xml", options)?;
        zip.write_all(b"<container />")?;
        zip.start_file("OEBPS/Images/10.jpg", options)?;
        zip.write_all(&png_bytes()?)?;
        zip.start_file("OEBPS/Images/2.jpg", options)?;
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
        let mangas = provider.mangas();
        let manga = &mangas[0];

        assert_eq!(1, mangas.len());
        assert_eq!("One Shot", manga.title);
        assert_eq!(1, manga.chapters.len());
        assert_eq!(2, manga.chapters[0].pages.len());
        assert!(manga.chapters[0].pages[0].url.path().ends_with("2.png"));
        assert_eq!(2, provider.library_stats().page_count);
        assert_eq!(1, provider.library_stats().image_folder_count);

        Ok(())
    }

    #[test]
    fn scans_folder_of_chapter_folders_as_one_manga() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("chapter-folders");
        let manga_dir = dir.path.join("Series");

        write_png(&manga_dir.join("Chapter 1").join("1.png"))?;
        write_png(&manga_dir.join("Chapter 2").join("1.png"))?;

        let provider = LocalProvider::from_path(&manga_dir)?;
        let mangas = provider.mangas();
        let manga = &mangas[0];

        assert_eq!(1, mangas.len());
        assert_eq!("Series", manga.title);
        assert_eq!(2, manga.chapters.len());
        assert_eq!("Chapter 1", manga.chapters[0].title);
        assert_eq!("Chapter 2", manga.chapters[1].title);

        Ok(())
    }

    #[test]
    fn scans_volume_folders_as_one_manga_with_volume_numbers() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("volume-folders");
        let manga_dir = dir.path.join("Series");

        write_png(&manga_dir.join("Vol 2").join("Chapter 10").join("1.png"))?;
        write_png(&manga_dir.join("Vol 1").join("Chapter 1").join("1.png"))?;

        let provider = LocalProvider::from_path(&manga_dir)?;
        let manga = &provider.mangas()[0];

        assert_eq!(1, provider.mangas().len());
        assert_eq!(2, manga.chapters.len());
        assert_eq!(Some("1".to_string()), manga.chapters[0].volume);
        assert_eq!("1", manga.chapters[0].number);
        assert_eq!(Some("2".to_string()), manga.chapters[1].volume);
        assert_eq!("10", manga.chapters[1].number);
        assert_eq!(2, provider.library_stats().volume_count);

        let list = LocalProvider::list_of_chapters(manga);
        assert_eq!(2, list.volumes.as_slice().len());
        assert_eq!("1", list.volumes.as_slice()[0].volume);
        assert_eq!("2", list.volumes.as_slice()[1].volume);

        Ok(())
    }

    #[test]
    fn cover_image_does_not_turn_manga_folder_into_image_chapter() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("cover-with-chapters");
        let manga_dir = dir.path.join("Series");

        write_png(&manga_dir.join("cover.jpg"))?;
        write_png(&manga_dir.join("Chapter 1").join("1.png"))?;
        write_png(&manga_dir.join("Chapter 2").join("1.png"))?;

        let provider = LocalProvider::from_path(&manga_dir)?;
        let manga = &provider.mangas()[0];

        assert_eq!(2, manga.chapters.len());
        assert!(manga.cover_img_url.ends_with("cover.jpg"));

        Ok(())
    }

    #[test]
    fn parses_volume_and_chapter_numbers_from_names() {
        assert_eq!(Some("2".to_string()), volume_number_from_title("Vol. 2 Chapter 10"));
        assert_eq!(Some("10".to_string()), chapter_number_from_title("Vol. 2 Chapter 10"));

        let parsed = chapter_numbers_from_path(Path::new("Volume 03 - 012.cbz"), 1, None);

        assert_eq!(Some("03".to_string()), parsed.volume);
        assert_eq!("012", parsed.chapter);
        assert!(!looks_like_volume_path(Path::new("Volcano Arc")));
    }

    #[test]
    fn scans_folder_of_manga_folders_as_library() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("library");

        write_png(&dir.path.join("Series A").join("Chapter 1").join("1.png"))?;
        write_png(&dir.path.join("Series B").join("Chapter 1").join("1.png"))?;

        let provider = LocalProvider::from_path(&dir.path)?;
        let mangas = provider.mangas();

        assert_eq!(2, mangas.len());
        assert_eq!("Series A", mangas[0].title);
        assert_eq!("Series B", mangas[1].title);
        assert_eq!(1, mangas[0].chapters.len());
        assert_eq!(1, mangas[1].chapters.len());

        Ok(())
    }

    #[test]
    fn scans_single_cbz_as_one_manga() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("cbz");
        let cbz = dir.path.join("Archive.cbz");

        write_cbz(&cbz)?;

        let provider = LocalProvider::from_path(&cbz)?;
        let mangas = provider.mangas();
        let manga = &mangas[0];

        assert_eq!(1, mangas.len());
        assert_eq!("Archive", manga.title);
        assert_eq!(1, manga.chapters.len());
        assert_eq!(2, manga.chapters[0].pages.len());
        assert_eq!(1, provider.library_stats().cbz_count);
        assert!(!local_cache_path(&cbz)?.exists());
        assert!(matches!(
            &manga.chapters[0].pages[0].source,
            LocalPageSource::ZipEntry { entry_name, .. } if entry_name == "2.png"
        ));

        Ok(())
    }

    #[test]
    fn scans_single_epub_as_one_manga() -> Result<(), Box<dyn Error>> {
        let dir = TestDir::new("epub");
        let epub = dir.path.join("Volume.epub");

        write_epub(&epub)?;

        let provider = LocalProvider::from_path(&epub)?;
        let mangas = provider.mangas();
        let manga = &mangas[0];

        assert_eq!(1, mangas.len());
        assert_eq!("Volume", manga.title);
        assert_eq!(1, manga.chapters.len());
        assert_eq!(2, manga.chapters[0].pages.len());
        assert_eq!(1, provider.library_stats().epub_count);
        assert!(matches!(
            &manga.chapters[0].pages[0].source,
            LocalPageSource::ZipEntry { entry_name, .. } if entry_name == "OEBPS/Images/2.jpg"
        ));

        Ok(())
    }

    #[test]
    fn reload_from_path_replaces_library_index() -> Result<(), Box<dyn Error>> {
        let first = TestDir::new("reload-first");
        let second = TestDir::new("reload-second");
        let first_manga = first.path.join("First");
        let second_manga = second.path.join("Second");

        write_png(&first_manga.join("1.png"))?;
        write_png(&second_manga.join("1.png"))?;

        let provider = LocalProvider::from_path(&first_manga)?;
        assert_eq!("First", provider.mangas()[0].title);

        provider.reload_from_path(&second_manga)?;

        let mangas = provider.mangas();
        assert_eq!(1, mangas.len());
        assert_eq!("Second", mangas[0].title);
        assert_eq!(second_manga.canonicalize()?, provider.library_stats().library_path);

        Ok(())
    }

    #[test]
    #[ignore = "requires MANGA_TUI_TEST_LIBRARY_DIR to point to a local manga library"]
    fn scans_configured_local_library() -> Result<(), Box<dyn Error>> {
        let Some(path) = std::env::var_os("MANGA_TUI_TEST_LIBRARY_DIR").map(PathBuf::from) else {
            return Ok(());
        };

        let provider = LocalProvider::from_path(path)?;
        let stats = provider.library_stats();

        assert!(stats.manga_count > 0);
        assert!(stats.chapter_count > 0);
        assert!(stats.page_count > 0);

        Ok(())
    }

    #[test]
    fn safe_archive_path_ignores_parent_components() {
        let path = safe_archive_path(Path::new("/tmp/cache"), "../chapter/1.jpg").unwrap();

        assert_eq!(Path::new("/tmp/cache/chapter/1.jpg"), path);
    }
}
