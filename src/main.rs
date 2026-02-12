mod app;
mod cli;
mod mpv;
mod utility;

static ARGS: LazyLock<cli::Cli> = LazyLock::new(|| cli::Cli::parse());

use std::sync::LazyLock;

use anyhow::Result;
use app::*;
use clap::Parser;
use strum::IntoEnumIterator;

#[tokio::main]
async fn main() -> Result<()> {
    let mut api = None;
    let mut app: Option<YoutubeRs> = None;
    loop {
        if let Some(current_app) = &mut app
            && current_app.action.is_player()
        {
            current_app.process().await?;
            continue;
        }
        let mut res =
            inquire::Select::new("Select Action", AppAction::iter().collect()).prompt()?;
        match res {
            AppAction::Download { format: _ } => {
                let fmt = FormatInquire::select("Select Audio or Video").prompt()?;
                let mut format = Format::from(fmt);
                match &mut format {
                    Format::Audio { format } => {
                        *format = AudioFormat::select("Select Audio Format").prompt()?
                    }
                    Format::Video { format } => {
                        *format = VideoFormat::select("Select Video Format").prompt()?
                    }
                }
                res = AppAction::Download { format };
            }
            AppAction::Transcript => {}
            AppAction::Player { format: _ } => {
                let fmt = FormatInquire::select("Select Audio or Video").prompt()?;
                res = AppAction::Player { format: fmt.into() }
            }
            AppAction::Quit => break,
        }
        if api.is_none() {
            api = Some(YoutubeAPI::select("Select API").prompt().unwrap());
        }

        app = Some(YoutubeRs {
            api: api.clone().unwrap_or_default(),
            action: res,
            mpv_installed: YoutubeRs::check_mpv().unwrap_or_default(),
            last_search: None,
        });
        if let Some(app) = &mut app {
            app.process().await?;
        }
    }
    Ok(())
}
