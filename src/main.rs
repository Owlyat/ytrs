mod app;
mod utility;

use anyhow::Result;
use app::*;
use strum::IntoEnumIterator;

#[tokio::main]
async fn main() -> Result<()> {
    let mut api = None;
    loop {
        let mut res =
            inquire::Select::new("Select Action", AppAction::iter().collect()).prompt()?;
        match res {
            AppAction::Download { format: _ } => {
                let fmt =
                    inquire::Select::new("Select Audio or Video", Format::variants()).prompt()?;
                res = AppAction::Download {
                    format: Format::from(fmt),
                };
            }
            AppAction::Transcript => {}
            AppAction::Player { format: _ } => {
                let fmt =
                    inquire::Select::new("Select Audio or Video", Format::variants()).prompt()?;
                match fmt.as_str() {
                    "Video" => {
                        res = AppAction::Player {
                            format: Format::Video {
                                format: Default::default(),
                            },
                        }
                    }
                    "Audio" => {
                        res = AppAction::Player {
                            format: Format::Audio {
                                format: Default::default(),
                            },
                        }
                    }
                    _ => {}
                }
            }
            AppAction::Quit => break,
        }
        if api.is_none() {
            api = Some(inquire::Select::new("Select API", YoutubeAPI::iter().collect()).prompt()?);
        }
        let mut app = YoutubeRs {
            api: api.clone().unwrap_or_default(),
            action: res,
            mpv_installed: YoutubeRs::check_mpv().unwrap_or_default(),
            last_search: None,
        };
        app.process().await?;
    }
    Ok(())
}
