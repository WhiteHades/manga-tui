# manga-tui

`manga-tui` is an offline terminal manga library and reader. It reads local folders and archives directly from disk, keeps history/bookmarks locally, and does not use online manga providers.

## Features

- Local-first library view for folders, image folders, CBZ/ZIP, CBR/RAR, and EPUB.
- Lazy archive indexing: archives are scanned for page names without full extraction, and pages are read when opened.
- In-app Stats page with library path, manga/chapter/page totals, format counts, and library reload.
- Vim-style navigation across Library, Search, Stats, Manga, and Reader screens.
- Inline image reader with stable page/status panels and centered page rendering.
- Local downloads to CBZ, EPUB, PDF, or raw images.

## Install

From this checkout:

```sh
cargo install --path . --force --locked
```

Then run:

```sh
manga-tui
```

By default the app opens:

```text
/home/user/Videos/mangas
```

Override the library path with either:

```sh
manga-tui --local /path/to/manga-library
MANGA_TUI_LIBRARY_DIR=/path/to/manga-library manga-tui
```

## Local Library Layouts

Supported inputs:

- A folder of images: one manga, one chapter.
- A single `.cbz`, `.zip`, `.cbr`, `.rar`, or `.epub`: one manga, one chapter.
- A folder of chapter folders or chapter archives: one manga.
- A folder of manga folders: one library.

Cover detection:

- `cover.jpg`, `cover.png`, `cover.webp`, and other supported image extensions are used when present.
- If no cover file exists, the first page of the first chapter is used.

## In-App Library Selection

Open the Stats tab with:

```text
gt
```

Then:

- `e`: edit the local library path.
- `Enter`: reload the typed path.
- `Esc`: cancel path editing.
- `r`: reload the current path.

Reloading swaps the shared local index and clears stale Library/Search/Manga/Reader state.

## Key Bindings

| Area | Keys |
| --- | --- |
| App | `gh` Library, `gs` Search, `gt` Stats, `q` quit |
| Lists | `j`/`k` move down/up, `Enter`/`l` open |
| Search | `/` type, `Enter` search, `Esc` stop typing |
| Pagination | `Ctrl-d` next page, `Ctrl-u` previous page |
| Manga page | `Enter`/`l` read, `B` read bookmarked, `m` bookmark, `d` download chapter, `a` download all |
| Reader | `j`/`l` next page, `k`/`h` previous page, `n` next chapter, `N` previous chapter, `q`/`Backspace` exit |
| Confirm dialogs | `y` confirm, `n`/`q`/`Esc` cancel |

## Image Rendering

Use a terminal with inline image support for the reader. If the terminal does not support inline images, the app still opens and can browse/search/download local manga, but the reader will show an inline-image support error when opening a chapter.

## Configuration

Print the config directory:

```sh
manga-tui --config-dir
```

Default config:

```toml
download_type = "cbz"
image_quality = "low"
amount_pages = 5
auto_bookmark = true
```

Print the data directory:

```sh
manga-tui --data-dir
```

Data directories:

- `history`: SQLite reading history/bookmarks.
- `mangaDownloads`: downloaded chapters.
- `errorLogs`: error log files.

Override the data directory with:

```sh
export MANGA_TUI_DATA_DIR="/path/to/manga-tui-data"
```

## Development

Useful verification commands:

```sh
cargo fmt
cargo check
cargo test --all-targets
MANGA_TUI_TEST_LIBRARY_DIR=/home/user/Videos/mangas cargo test scans_configured_local_library -- --ignored --nocapture
```
