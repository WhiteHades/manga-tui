use std::error::Error;
use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand, crate_version};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::backend::APP_DATA_DIR;
use crate::backend::manga_provider::Languages;
use crate::config::get_config_directory_path;
use crate::global::PREFERRED_LANGUAGE;

#[derive(Subcommand, Clone)]
pub enum Commands {
    Lang {
        #[arg(short, long)]
        print: bool,
        #[arg(short, long)]
        set: Option<String>,
    },
}

#[derive(Parser, Clone)]
#[command(version = crate_version!())]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,
    #[arg(short, long)]
    pub data_dir: bool,
    #[arg(short, long)]
    pub config_dir: bool,
    /// Read a local manga library, image folder, CBZ, CBR, or EPUB.
    #[arg(short = 'l', long = "local", value_name = "PATH")]
    pub local: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Credentials {
    pub access_token: String,
    pub client_id: String,
}

impl CliArgs {
    pub fn new() -> Self {
        Self {
            config_dir: false,
            command: None,
            data_dir: false,
            local: None,
        }
    }

    pub fn with_command(mut self, command: Commands) -> Self {
        self.command = Some(command);
        self
    }

    pub fn print_available_languages() {
        println!("The available languages are:");
        Languages::iter().filter(|lang| *lang != Languages::Unkown).for_each(|lang| {
            println!("{} {} | iso code : {}", lang.as_emoji(), lang.as_human_readable().to_lowercase(), lang.as_iso_code())
        });
    }

    /// This method should only return `Ok(())` it the app should keep running, otherwise `exit`
    pub async fn proccess_args(self) -> Result<(), Box<dyn Error>> {
        if self.data_dir {
            let app_dir = APP_DATA_DIR.as_ref().unwrap();
            println!("{}", app_dir.to_str().unwrap());
            exit(0)
        }

        if self.config_dir {
            println!("{}", get_config_directory_path().display());
            exit(0)
        }

        match &self.command {
            Some(command) => match command {
                Commands::Lang { print, set } => {
                    if *print {
                        Self::print_available_languages();
                        exit(0)
                    }

                    match set {
                        Some(lang) => {
                            let try_lang = Languages::try_from_iso_code(lang.as_str());

                            if try_lang.is_none() {
                                println!(
                                    "`{}` is not a valid ISO language code, run `{} lang --print` to list available languages and their ISO codes",
                                    lang,
                                    env!("CARGO_BIN_NAME")
                                );

                                exit(0)
                            }

                            PREFERRED_LANGUAGE.set(try_lang.unwrap()).unwrap();
                        },
                        None => {
                            PREFERRED_LANGUAGE.set(Languages::default()).unwrap();
                        },
                    }
                    Ok(())
                },
            },
            None => {
                PREFERRED_LANGUAGE.set(Languages::default()).unwrap();
                Ok(())
            },
        }
    }
}
