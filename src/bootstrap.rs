use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Result, bail};
use tracing::{debug, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;

use crate::app::{AudioFormat, Format, VideoFormat, YoutubeRs};
use crate::cli;

/// File logging setup.
///
/// Debug builds log to `./ytrs.log`; release builds log to
/// `~/.config/ytrs/ytrs.log` (see [`crate::config::ytrs_config_dir`]).
///
/// Returns the guard that must be kept alive for logs to be flushed.
pub fn init_tracing() -> WorkerGuard {
    let (dir, filename) = log_location();
    // Ensure the directory exists (a no-op for `.`).
    let _ = std::fs::create_dir_all(&dir);
    let file_appender = tracing_appender::rolling::never(dir, filename);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    fmt().with_max_level(tracing::Level::DEBUG)
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .init();
    guard
}

/// Read one trimmed line from stdin. `None` on EOF.
fn read_line() -> Option<String> {
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }
    Some(buf.trim().to_string())
}

/// Plain-stdin replacement for the old inquire text prompt.
pub fn prompt_text(title: &str) -> Result<String> {
    loop {
        print!("{title}: ");
        io::stdout().flush()?;
        match read_line() {
            Some(input) if input.trim().is_empty() => {
                println!("Input cannot be empty.");
            }
            Some(input) if input.len() < 2 => {
                println!("Input too short (min 2 characters).");
            }
            Some(input) => return Ok(input),
            None => bail!("User cancelled"),
        }
    }
}

/// Plain-stdin numbered list. Returns `None` when the user picks 0 (cancel).
pub fn prompt_select(title: &str, items: &[String]) -> Result<Option<usize>> {
    if items.is_empty() {
        bail!("Nothing to select");
    }
    println!("{title}:");
    for (i, item) in items.iter().enumerate() {
        println!("  [{}] {item}", i + 1);
    }
    println!("  [0] Cancel");
    loop {
        print!("Choice: ");
        io::stdout().flush()?;
        match read_line() {
            None => bail!("User cancelled"),
            Some(input) => match input.parse::<usize>() {
                Ok(0) => return Ok(None),
                Ok(n) if n >= 1 && n <= items.len() => return Ok(Some(n - 1)),
                _ => println!("Enter a number between 0 and {}", items.len()),
            },
        }
    }
}

/// Plain-stdin yes/no prompt.
pub fn prompt_confirm(title: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    loop {
        print!("{title} [{hint}]: ");
        io::stdout().flush()?;
        match read_line() {
            None => bail!("User cancelled"),
            Some(input) if input.is_empty() => return Ok(default_yes),
            Some(input) => match input.to_lowercase().as_str() {
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => println!("Answer y or n."),
            },
        }
    }
}

/// Where the log file goes, per build profile.
#[cfg(debug_assertions)]
fn log_location() -> (std::path::PathBuf, &'static str) {
    (std::path::PathBuf::from("."), "ytrs.log")
}

/// Where the log file goes, per build profile.
#[cfg(not(debug_assertions))]
fn log_location() -> (std::path::PathBuf, &'static str) {
    (crate::config::ytrs_config_dir(), "ytrs.log")
}

/// Wiring between the parsed CLI args and [`YoutubeRs`].
///
/// Kept out of `main.rs` so the entry point only handles top-level dispatch.
pub fn build_app_from_cli(args: &cli::Cli) -> Option<YoutubeRs> {
    let cloned = args.clone();
    match &args.command {
        Some(cli::AppActionCli::Download { query, url, format, codec }) => {
            debug!(?query, ?url, ?format, ?codec, "building download action");
            Some(build_download(query, url, format.unwrap_or_default(), codec, cloned))
        }
        Some(cli::AppActionCli::Player {
            file,
            url,
            api,
            midi,
            embed,
            vo,
            audio_device,
            no_art,
            img_protocol,
        }) => {
            debug!(?file, ?url, ?api, midi, embed, vo, ?audio_device, "building player action");
            Some(build_player(
                file,
                url,
                api,
                *midi,
                *embed,
                vo.clone(),
                audio_device.clone(),
                *no_art,
                img_protocol.unwrap_or_default(),
                cloned,
            ))
        }
        Some(cli::AppActionCli::Transcript {
            query,
            summarize,
            url,
        }) => {
            debug!(?query, ?summarize, ?url, "building transcript action");
            Some(build_transcript(query, summarize, url, cloned))
        }
        Some(cli::AppActionCli::Update) => {
            info!("yt-dlp update requested");
            update_yt_dlp(cloned)
        }
        None => None,
    }
}

pub(crate) fn update_yt_dlp(args: cli::Cli) -> Option<YoutubeRs> {
    let (tx, rx) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        YoutubeRs::update_yt_dlp(&args).await.unwrap();
        let _ = tx.send(());
    });
    println!("Updating yt-dlp");
    while rx.try_recv().is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("Finished");

    None
}

fn download_format(
    format: cli::DownloadFormat,
    codec: &Option<String>,
) -> Result<Format> {
    match format {
        cli::DownloadFormat::Audio => {
            let codec = codec.clone().unwrap_or_else(|| "mp3".to_string());
            match codec.to_lowercase().as_str() {
                "mp3" => Ok(Format::Audio {
                    format: AudioFormat::MP3,
                }),
                "wav" => Ok(Format::Audio {
                    format: AudioFormat::WAV,
                }),
                _ => bail!("Unknown audio codec '{codec}' (mp3|wav)"),
            }
        }
        cli::DownloadFormat::Video => {
            let codec = codec.clone().unwrap_or_else(|| "mp4".to_string());
            match codec.to_lowercase().as_str() {
                "mp4" => Ok(Format::Video {
                    format: VideoFormat::MP4,
                }),
                "avi" => Ok(Format::Video {
                    format: VideoFormat::AVI,
                }),
                "mov" => Ok(Format::Video {
                    format: VideoFormat::MOV,
                }),
                _ => bail!("Unknown video codec '{codec}' (mp4|avi|mov)"),
            }
        }
    }
}

fn build_download(
    query: &Option<String>,
    url: &Option<String>,
    format: cli::DownloadFormat,
    codec: &Option<String>,
    args: cli::Cli,
) -> YoutubeRs {
    let fmt = match download_format(format, codec) {
        Ok(fmt) => fmt,
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(2);
        }
    };
    let mut builder = YoutubeRs::builder();
    builder.download(fmt);
    if let Some(query) = query {
        builder.api(Some(false)).query(query).build(args)
    } else if let Some(url) = url {
        builder.url(url.clone()).build(args)
    } else {
        // Search term is asked at runtime, no pre-TUI prompt here.
        builder.api(Some(false)).build(args)
    }
}

fn build_player(
    file: &Option<std::path::PathBuf>,
    url: &Option<String>,
    api: &Option<cli::PlayerAPI>,
    midi: bool,
    embed: bool,
    vo: Option<String>,
    audio_device: Option<String>,
    no_art: bool,
    img_protocol: cli::ImgProtocol,
    args: cli::Cli,
) -> YoutubeRs {
    let mut builder = YoutubeRs::builder();
    builder
        .midi(midi)
        .embed(embed)
        .vo(vo)
        .audio_device(audio_device)
        .no_art(no_art)
        .img_protocol(img_protocol);
    if let Some(file) = file {
        debug!(?file, "player: building from file");
        builder.player().file(file.to_path_buf()).build(args)
    } else if let Some(url) = url {
        debug!(?url, ?api, "player: building from url");
        let is_music = match api {
            Some(cli::PlayerAPI::Video) => Some(false),
            Some(cli::PlayerAPI::Music) => Some(true),
            None => crate::app::url_is_music(url),
        };
        // Default format follows the source: music -> audio, video -> video.
        let format = match is_music {
            Some(true) => Format::Audio {
                format: AudioFormat::MP3,
            },
            _ => Format::Video {
                format: VideoFormat::MP4,
            },
        };
        builder
            .player_with_format(format)
            .api(is_music)
            .url(url.clone())
            .build(args)
    } else {
        debug!("player: building empty player hub");
        builder.audio_player().build(args)
    }
}

fn build_transcript(
    query: &Option<String>,
    summarize: &Option<bool>,
    url: &Option<String>,
    args: cli::Cli,
) -> YoutubeRs {
    let mut builder = YoutubeRs::builder();
    builder.transcript();
    if let Some(query) = query {
        builder.query(query);
    } else if let Some(url) = url {
        builder.url(url);
    }
    if let Some(b) = summarize {
        builder.do_summarize(*b);
    }
    builder.build(args)
}

/// No subcommand: open the player hub directly (search from the TUI).
pub async fn run_interactive(args: cli::Cli) -> Result<()> {
    info!("no subcommand, opening player hub");
    let mut app = YoutubeRs::builder().audio_player().build(args);
    app.run().await
}
