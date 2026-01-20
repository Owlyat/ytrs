use anyhow::{Context, Result, bail};
use inquire::{Confirm, Select, Text as InquireText, validator::Validation};
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, poll, read},
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use rustypipe::{
    client::RustyPipe,
    model::{TrackItem, VideoItem, traits::YtEntity},
};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio::process::{Child, Command};
use yt_dlp::{Youtube, fetcher::deps::Libraries};

#[derive(Error, Debug)]
pub enum YtrsError {
    #[error("MPV not installed or not found in PATH")]
    MpvNotFound,
    #[error("YouTube query failed: {0}")]
    QueryError(String),
    #[error("Download failed: {0}")]
    DownloadError(String),
    #[error("Video selection failed")]
    SelectionError,
    #[error("External process error: {0}")]
    ProcessError(String),
    #[error("YouTube library error: {0}")]
    LibraryError(String),
    #[error("MPV playback error: {0}")]
    PlaybackError(String),
}

struct MpvController {
    process: Child,
}

impl MpvController {
    async fn new(audio_only: bool, url: &str) -> Result<Self> {
        let mut args = vec!["--no-terminal"];

        if audio_only {
            args.push("--no-video");
        }

        args.push(url);

        let process = Command::new("mpv")
            .args(&args)
            .spawn()
            .context("Failed to spawn MPV process")?;

        Ok(Self { process })
    }

    async fn terminate(&mut self) {
        let _ = self.process.kill().await;
        let _ = self.process.wait().await;
    }
}

impl Drop for MpvController {
    fn drop(&mut self) {
        if !cfg!(windows) {
            if let Some(pid) = self.process.id() {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
            }
        }
        if cfg!(windows) {
            if let Some(pid) = self.process.id() {
                let _ = std::process::Command::new("taskkill")
                    .args(["/f", "/pid", &pid.to_string()])
                    .output();
            } else {
                let _ = std::process::Command::new("taskkill")
                    .args(["/f", "/im", "mpv.exe"])
                    .output();
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    YTRSAction::default().run().await
}

const ACTIONS: &[&str] = &["Watch", "Listen", "YT-DLP", "Exit"];

#[derive(Default)]
pub enum YTRSAction {
    #[default]
    None,
    Ytdlp {
        yt_query: YtMusicQuery,
    },
    Watch {
        yt_query: YTQuery,
    },
    Listen {
        yt_query: YtMusicQuery,
        search_term: Option<String>,
    },
}

impl YTRSAction {
    pub async fn run(&mut self) -> Result<()> {
        loop {
            match Select::new("Select an action", ACTIONS.to_vec())
                .with_page_size(6)
                .with_help_message("Type to filter | Arrow keys to navigate")
                .prompt()
                .context("Failed to display action menu")
            {
                Ok("Watch") => {
                    self.watch().await?;
                }
                Ok("Listen") => self.listen().await?,
                Ok("YT-DLP") => self.yt_dlp().await?,
                Ok("Exit") => break Ok(()),
                Ok(_) => {}
                Err(e) => {
                    bail!("Action selection failed: {}", e);
                }
            }
        }
    }

    async fn watch(&mut self) -> Result<()> {
        loop {
            let query = InquireText::new("Query:")
                .with_help_message("Press Escape to cancel | Ctrl+C to exit")
                .with_validator(|input: &str| {
                    if input.trim().is_empty() {
                        Ok(Validation::Invalid("Query cannot be empty".into()))
                    } else if input.len() < 2 {
                        Ok(Validation::Invalid(
                            "Query too short (min 2 characters)".into(),
                        ))
                    } else {
                        Ok(Validation::Valid)
                    }
                })
                .prompt()
                .context("Failed to read query input")?;

            match YTQuery::from(query.as_str()).await {
                Ok(yt) => {
                    *self = Self::Watch { yt_query: yt };
                }
                Err(e) => {
                    bail!("{}", e);
                }
            }

            if let Self::Watch { yt_query } = self {
                let url = format!(
                    "https://www.youtube.com/watch?v={}",
                    yt_query.video.id.clone()
                );

                let output = Command::new("mpv")
                    .args(["--version"])
                    .output()
                    .await
                    .context("Failed to check MPV version")?;

                if !output.status.success() {
                    bail!("MPV is not installed. Please install MPV to watch videos.");
                }

                let mut mpv = MpvController::new(false, &url)
                    .await
                    .context("Failed to start MPV")?;
                self.watch_playback(&mut mpv).await?;
                drop(mpv);
            }
        }
    }

    async fn watch_playback(&mut self, mpv: &mut MpvController) -> Result<()> {
        let mut term = ratatui::init();
        let quit_style = Style::default()
            .bg(Color::Rgb(50, 50, 70))
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let key_style = Style::default()
            .bg(Color::Yellow)
            .fg(Color::Rgb(50, 50, 70))
            .add_modifier(Modifier::BOLD);

        loop {
            match mpv.process.try_wait() {
                Ok(Some(status)) => {
                    ratatui::restore();
                    if !status.success() {
                        bail!("MPV playback ended with error");
                    }
                    return Ok(());
                }
                Ok(None) => {
                    term.draw(|f| {
                        let area = f.area();
                        let popup_area = Rect::new(
                            area.x + (area.width as u16 / 4),
                            area.y + area.height as u16 - 6,
                            area.width as u16 / 2,
                            5,
                        );
                        let block = Block::default()
                            .title("⬛ Playback Control")
                            .title_alignment(Alignment::Center)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow))
                            .border_type(BorderType::Rounded)
                            .style(Style::default().bg(Color::Rgb(30, 30, 45)));
                        f.render_widget(block, popup_area);
                        let inner_area = popup_area.inner(ratatui::layout::Margin {
                            vertical: 1,
                            horizontal: 2,
                        });
                        let text = Paragraph::new(Text::from(Line::from(vec![
                            Span::styled("Press ", quit_style),
                            Span::styled(" [ q ] ", key_style),
                            Span::styled("to stop playback", quit_style),
                        ])))
                        .alignment(Alignment::Center);
                        f.render_widget(text, inner_area);
                    })
                    .expect("Terminal draw crashed");

                    if poll(Duration::from_millis(50))? {
                        if let Ok(Event::Key(KeyEvent { code, .. })) = read() {
                            if let KeyCode::Char(c) = code {
                                if c == 'q' || c == 'Q' {
                                    ratatui::restore();
                                    mpv.terminate().await;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    ratatui::restore();
                    bail!("Error waiting for MPV: {}", e);
                }
            }
        }
    }

    async fn listen(&mut self) -> Result<()> {
        loop {
            let (yt_query, search_term) = match YtMusicQuery::new_music_search(
                if let YTRSAction::Listen {
                    yt_query: _,
                    search_term,
                } = self
                {
                    search_term.clone()
                } else {
                    None
                },
            )
            .await
            {
                Ok(result) => result,
                Err(_) => break Ok(()),
            };

            *self = Self::Listen {
                yt_query,
                search_term: Some(search_term),
            };

            let url = format!("https://www.youtube.com/watch?v={}", self.get_id().unwrap());

            let output = Command::new("mpv")
                .args(["--version"])
                .output()
                .await
                .context("Failed to check MPV version")?;

            if !output.status.success() {
                bail!("MPV is not installed. Please install MPV to listen to music.");
            }

            let mut mpv = MpvController::new(true, &url)
                .await
                .context("Failed to start MPV")?;
            self.watch_playback(&mut mpv).await?;
            drop(mpv);
        }
    }

    async fn yt_dlp(&mut self) -> Result<()> {
        let output_dir = PathBuf::from("output");
        let libraries_dir = PathBuf::from("libs");

        match Select::new("Action", vec!["Download", "Update", "Install"])
            .with_page_size(6)
            .with_help_message("Type to filter | Arrow keys to navigate")
            .prompt()
            .context("Failed to display YT-DLP action menu")
        {
            Ok("Download") => {
                let (yt_query, _) = match YtMusicQuery::new_music_search(None).await {
                    Ok(result) => result,
                    Err(_) => return Ok(()),
                };
                *self = Self::Ytdlp { yt_query };
                let url = &format!("https://www.youtube.com/watch?v={}", self.get_id().unwrap());

                let ytdlp = libraries_dir.join("yt-dlp");
                let ffmpeg = libraries_dir.join("ffmpeg");

                let libraries = Libraries::new(ytdlp, ffmpeg);
                let fetcher = Youtube::new(libraries, output_dir)
                    .context("Failed to initialize YouTube downloader")?;

                match Select::new("Download", vec!["All", "Audio-Only", "Video-Only"])
                    .with_page_size(6)
                    .with_help_message("Type to filter | Arrow keys to navigate")
                    .prompt()
                    .context("Failed to display download options")
                {
                    Ok("All") => {
                        let extension =
                            Select::new("Format", vec!["mp4", "webm", "avi", "mov", "mkv", "flv"])
                                .with_page_size(8)
                                .prompt()
                                .context("Failed to select format")?;
                        match fetcher
                            .download_video_from_url(
                                url.clone(),
                                format!("{}.{}", self.get_name().unwrap(), extension),
                            )
                            .await
                        {
                            Ok(path) => {
                                println!("Video Downloaded at {}", path.to_string_lossy());
                            }
                            Err(e) => {
                                bail!("Error downloading video: {}", e);
                            }
                        }
                    }
                    Ok("Audio-Only") => {
                        let extension = Select::new(
                            "Extension",
                            vec![
                                "mp3", "wav", "flac", "aac", "alac", "ogg", "m4a", "opus", "vorbis",
                            ],
                        )
                        .with_page_size(8)
                        .prompt()
                        .context("Failed to select audio extension")?;
                        match fetcher
                            .download_video_from_url(
                                url.clone(),
                                format!("{}.{}", self.get_name().unwrap(), extension),
                            )
                            .await
                        {
                            Ok(path) => {
                                println!("Audio Downloaded at {}", path.to_string_lossy());
                            }
                            Err(e) => {
                                bail!("Error while downloading audio: {}", e);
                            }
                        }
                    }
                    Ok("Video-Only") => {
                        let extension = Select::new("Extension", vec!["mp4", "avi", "mov", "webm"])
                            .with_page_size(6)
                            .prompt()
                            .context("Failed to select video extension")?;
                        match fetcher
                            .download_video_stream_from_url(
                                url.clone(),
                                format!("{}.{}", self.get_name().unwrap(), extension),
                            )
                            .await
                        {
                            Ok(path) => {
                                println!("Video Downloaded at {}", path.to_string_lossy());
                            }
                            Err(e) => {
                                bail!("Error while downloading Video: {}", e);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        bail!("Download selection failed: {}", e);
                    }
                }
            }
            Ok("Update") => {
                let ytdlp = libraries_dir.join("yt-dlp");
                let ffmpeg = libraries_dir.join("ffmpeg");

                let libraries = Libraries::new(ytdlp, ffmpeg);
                let fetcher = Youtube::new(libraries, output_dir);

                if let Ok(ytdlp) = fetcher {
                    if let Err(e) = ytdlp.update_downloader().await {
                        bail!("Error while updating yt-dlp: {}", e);
                    }
                } else {
                    bail!("Failed to initialize downloader for update");
                }
            }
            Ok("Install") => {
                println!("Downloading Libs ...");

                let fetcher = Youtube::with_new_binaries(libraries_dir, output_dir).await;
                match fetcher {
                    Ok(_) => {
                        println!("Libs Installed Successfully");
                    }
                    Err(e) => {
                        bail!("Error while Installing libraries: {}", e);
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                bail!("Action selection failed: {}", e);
            }
        }
        Ok(())
    }

    fn get_id(&self) -> Result<String> {
        match self {
            YTRSAction::None => bail!("No video selected"),
            YTRSAction::Ytdlp { yt_query } => Ok(yt_query.video.id.clone()),
            YTRSAction::Watch { yt_query } => Ok(yt_query.video.id.clone()),
            YTRSAction::Listen {
                yt_query,
                search_term: _,
            } => Ok(yt_query.video.id.clone()),
        }
    }

    fn get_name(&self) -> Result<String> {
        match self {
            YTRSAction::None => bail!("No video selected"),
            YTRSAction::Ytdlp { yt_query } => Ok(yt_query.video.name.clone()),
            YTRSAction::Watch { yt_query } => Ok(yt_query.video.name.clone()),
            YTRSAction::Listen {
                yt_query,
                search_term: _,
            } => Ok(yt_query.video.name.clone()),
        }
    }
}

pub struct YtMusicQuery {
    video: TrackItem,
}

impl YtMusicQuery {
    async fn new_music_search(last_search_term: Option<String>) -> Result<(Self, String)> {
        let search_term = InquireText::new("Youtube Search:")
            .with_help_message("Press Escape to cancel | Ctrl+C to exit")
            .with_initial_value(&last_search_term.unwrap_or_default())
            .with_validator(|input: &str| {
                if input.trim().is_empty() {
                    Ok(Validation::Invalid("Search term cannot be empty".into()))
                } else if input.len() < 2 {
                    Ok(Validation::Invalid(
                        "Search term too short (min 2 characters)".into(),
                    ))
                } else {
                    Ok(Validation::Valid)
                }
            })
            .prompt()
            .context("Failed to read search input")?;

        let rp = RustyPipe::new();
        let found_videos = rp
            .query()
            .unauthenticated()
            .music_search_tracks(search_term.clone())
            .await
            .context("Failed to search YouTube Music")?;
        let mut found_videos_str: Vec<String> = found_videos
            .clone()
            .items
            .items
            .into_iter()
            .map(|x| x.name.to_string())
            .collect();
        found_videos_str.push("Exit".to_owned());
        let selected_vid_str = Select::new("Select Music", found_videos_str)
            .with_page_size(12)
            .prompt()
            .context("Failed to select music")?;
        if selected_vid_str == "Exit" {
            let confirm = Confirm::new("Exit application?")
                .with_default(true)
                .prompt()?;
            if confirm {
                bail!("User cancelled");
            }
        }
        if let Some(vid) = found_videos
            .items
            .items
            .into_iter()
            .find(|track| track.name() == selected_vid_str)
        {
            Ok((Self { video: vid }, search_term))
        } else {
            bail!("Selected video not found");
        }
    }
}

pub struct YTQuery {
    video: VideoItem,
}

impl YTQuery {
    pub async fn from(query: &str) -> Result<Self> {
        let found_videos = RustyPipe::new()
            .query()
            .unauthenticated()
            .search(query)
            .await
            .context("Failed to search YouTube")?;
        let mut videos: Vec<String> = found_videos
            .items
            .items
            .iter()
            .map(|v: &VideoItem| format!("➡️ {}", v.name))
            .collect();
        videos.push("Exit".to_owned());

        let video_name = Select::new("Select video to watch", videos)
            .with_page_size(12)
            .with_help_message("Type to filter | Arrow keys to navigate | Enter to select")
            .prompt()
            .context("Failed to select video")?;
        if video_name == "Exit" {
            let confirm = Confirm::new("Exit application?")
                .with_default(true)
                .prompt()?;
            if confirm {
                bail!("User cancelled");
            }
        }
        let selected_vid = found_videos
            .items
            .items
            .into_iter()
            .find(|v| video_name.contains(&v.name));
        if let Some(vid) = selected_vid {
            Ok(Self { video: vid })
        } else {
            bail!("Error: selected video not found");
        }
    }
}
