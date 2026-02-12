mod app;
mod cli;
mod mpv;
mod utility;

use anyhow::Result;
use app::*;
use clap::Parser;
use strum::IntoEnumIterator;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();
    let cloned = args.clone();
    let mut app: Option<YoutubeRs> = None;
    match &args.command {
        Some(cli::AppActionCli::Download { query, url }) => {
            if let Some(query) = query {
                app = Some(
                    YoutubeRs::builder()
                        .api(None, true)
                        .prompt_download()
                        .prompt_format()
                        .query(query)
                        .build(cloned),
                );
            } else if let Some(url) = url {
                app = Some(
                    YoutubeRs::builder()
                        .prompt_download()
                        .prompt_format()
                        .url(url.clone())
                        .build(cloned),
                );
            }
        }
        Some(cli::AppActionCli::Player { file, url }) => {
            if let Some(file) = file {
                app = Some(
                    YoutubeRs::builder()
                        .player()
                        .file(file.to_path_buf())
                        .build(cloned),
                );
            } else if let Some(url) = url {
                app = Some(
                    YoutubeRs::builder()
                        .api(None, true)
                        .player()
                        .url(url.clone())
                        .build(cloned),
                );
            }
        }
        Some(cli::AppActionCli::Transcript {
            query,
            summarize,
            url,
        }) => {
            if let Some(query) = query {
                let mut builder = YoutubeRs::builder();
                builder.transcript().query(query);
                if let Some(b) = summarize {
                    builder.do_summarize(b.clone());
                }
                app = Some(builder.build(cloned));
            } else if let Some(url) = url {
                let mut builder = YoutubeRs::builder();
                builder.transcript().url(url);
                if let Some(b) = summarize {
                    builder.do_summarize(b.clone());
                }
                app = Some(builder.build(cloned));
            }
        }
        None => {}
    }
    if let Some(current_app) = &mut app {
        current_app.process().await?;
        return Ok(());
    }
    let mut res = inquire::Select::new("Select Action", AppAction::iter().collect()).prompt()?;
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
        AppAction::Quit => return Ok(()),
    }
    app = Some(
        YoutubeRs::builder()
            .api(None, true)
            .action(Some(res), None)
            .build(args.clone()),
    );
    if let Some(app) = &mut app {
        app.process().await?;
    }
    Ok(())
}
