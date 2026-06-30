#![allow(unused)]
#![allow(dead_code)]
#![allow(deprecated)]
#![allow(clippy::single_match)]

use std::io::stdout;
use std::path::PathBuf;
use std::process::exit;

use backend::manga_provider::local::{LocalFilterWidget, LocalFiltersProvider, LocalProvider};
use backend::tracker::DisabledTracker;
use clap::Parser;
use crossterm::ExecutableCommand;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use directories::UserDirs;
use log::LevelFilter;
use logger::{ILogger, Logger};

use self::backend::build_data_dir;
use self::backend::database::Database;
use self::backend::migration::update_database_with_migrations;
use self::backend::tui::run_app;
use self::cli::CliArgs;

mod backend;
mod cli;
mod common;
mod config;
mod global;
mod logger;
mod utils;
mod view;

const DEFAULT_LOCAL_LIBRARY_RELATIVE_PATH: &str = "Videos/mangas";

fn local_library_path(cli_args: &CliArgs) -> PathBuf {
    cli_args
        .local
        .clone()
        .or_else(|| std::env::var_os("MANGA_TUI_LIBRARY_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            UserDirs::new()
                .map(|dirs| dirs.home_dir().join(DEFAULT_LOCAL_LIBRARY_RELATIVE_PATH))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_LOCAL_LIBRARY_RELATIVE_PATH))
        })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 7)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let logger = Logger;

    pretty_env_logger::formatted_builder()
        .format_module_path(false)
        .filter_level(LevelFilter::Info)
        .init();

    match build_data_dir(&logger) {
        Ok(_) => {},
        Err(e) => {
            logger.error(
            format!(
            "Data directory could not be created, this is where your manga history and manga downloads is stored
             \n this could be for many reasons such as the application not having enough permissions
            \n Try setting the environment variable `MANGA_TUI_DATA_DIR` to some path pointing to a directory, example: /home/user/somedirectory 
            \n Error details : {e}"
        ).into()
            );
            exit(1)
        },
    }

    let cli_args = CliArgs::parse();

    let local_path = local_library_path(&cli_args);

    cli_args.proccess_args().await?;

    let mut connection = Database::get_connection()?;
    let database = Database::new(&connection);

    database.setup()?;
    update_database_with_migrations(&mut connection, &logger)?;

    drop(connection);

    color_eyre::install()?;
    stdout().execute(EnableMouseCapture)?;

    if !local_path.exists() {
        logger.error(
            format!(
                "Local manga library not found: {}\nSet MANGA_TUI_LIBRARY_DIR or run `manga-tui --local <path>`.",
                local_path.display()
            )
            .into(),
        );
        exit(1)
    }

    logger.inform(format!("Using local manga from {}", local_path.display()));
    run_app(
        ratatui::init(),
        LocalProvider::from_path(local_path)?,
        Option::<DisabledTracker>::None,
        LocalFiltersProvider::new(),
        LocalFilterWidget::new(),
    )
    .await?;

    ratatui::restore();
    stdout().execute(DisableMouseCapture)?;

    Ok(())
}
