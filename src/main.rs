use anyhow::{Context, Result, bail};
use image::DynamicImage;
use inquire::{Confirm, Select, Text as InquireText, validator::Validation};
use mpv_ipc::{MpvIpc, MpvSpawnOptions};
use ratatui::{
    crossterm::{
        event::{Event, KeyCode, KeyEventKind, poll, read},
        style::Stylize,
    },
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
};
use ratatui_image::{Image, picker};
use rustypipe::{
    client::RustyPipe,
    model::{TrackItem, VideoItem},
};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;
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

struct MpvPlayer {
    mpv_ipc: MpvIpc,
}

impl MpvPlayer {
    async fn new(audio_only: bool, url: &str) -> Result<Self> {
        let opts = MpvSpawnOptions::default();

        let mut mpv = MpvIpc::spawn(&opts, audio_only)
            .await
            .context("Failed to spawn mpv process")?;

        // Load the media
        mpv.send_command(json!(["loadfile", url]))
            .await
            .context("Failed to load media")?;

        Ok(Self { mpv_ipc: mpv })
    }

    async fn quit(&mut self) {
        self.mpv_ipc.quit().await;
    }
}

const LOADING_FRAMES: &[&str] = &["-", "\\", "|", "/"];

#[tokio::main]
async fn main() -> Result<()> {
    YTRSAction::default().run().await
}

const ACTIONS: &[&str] = &["Watch", "Listen", "YT-DLP", "Exit"];

pub enum Query {
    YtQuery(YTQuery),
    YtMusicQuery(YtMusicQuery),
}

#[derive(Default)]
pub enum YTRSAction {
    #[default]
    None,
    Ytdlp {
        yt_query: Query,
    },
    Watch {
        yt_query: YTQuery,
    },
    Listen {
        yt_query: YtMusicQuery,
        search_term: Option<String>,
    },
}

static mut MPV_CHECKED: bool = false;
static mut MPV_INSTALLED: bool = false;

fn check_mpv_installed() -> bool {
    unsafe {
        if MPV_CHECKED {
            return MPV_INSTALLED;
        }
        MPV_CHECKED = true;
    }
    let output = std::process::Command::new("mpv")
        .args(["--version"])
        .output();
    match output {
        Ok(output) => {
            let installed = output.status.success();
            unsafe {
                MPV_INSTALLED = installed;
            }
            installed
        }
        Err(_) => {
            unsafe {
                MPV_INSTALLED = false;
            }
            false
        }
    }
}

fn check_libs_installed() -> bool {
    let libs_dir = PathBuf::from("libs");

    // Check for yt-dlp
    let ytdlp_exists = libs_dir.join("yt-dlp.exe").exists() || libs_dir.join("yt-dlp").exists();

    // Check for ffmpeg (in any subdirectory)
    let mut ffmpeg_exists = false;
    if libs_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&libs_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let bin_path = entry.path().join("bin").join("ffmpeg.exe");
                    if bin_path.exists() {
                        ffmpeg_exists = true;
                        break;
                    }
                }
            }
        }
    }

    ytdlp_exists && ffmpeg_exists
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

            println!("Searching for: {}", query);
            match YTQuery::from(query.as_str()).await {
                Ok(yt) => {
                    *self = Self::Watch { yt_query: yt };
                }
                Err(e) => {
                    eprintln!("Search failed: {}. Please try again.", e);
                    continue;
                }
            }

            if let Self::Watch { yt_query } = self {
                let url = format!(
                    "https://www.youtube.com/watch?v={}",
                    yt_query.video.id.clone()
                );

                if !check_mpv_installed() {
                    bail!(
                        "MPV is not installed or not in PATH.\n   Please install MPV to watch videos.\n   Windows: winget install mpv\n   macOS: brew install mpv\n   Linux: sudo apt install mpv"
                    );
                }

                let mut mpv = MpvPlayer::new(false, &url)
                    .await
                    .context("Failed to start MPV")?;
                self.watch_playback(&mut mpv, &mut None).await?;
            }
        }
    }

    async fn watch_playback(
        &mut self,
        mpv: &mut MpvPlayer,
        thumbnail: &mut Option<DynamicImage>,
    ) -> Result<()> {
        let mut term = ratatui::init();
        let quit_style = Style::default()
            .bg(Color::Rgb(50, 50, 70))
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let key_style = Style::default()
            .bg(Color::Yellow)
            .fg(Color::Rgb(50, 50, 70))
            .add_modifier(Modifier::BOLD);
        let status_style = Style::default()
            .bg(Color::Rgb(40, 40, 60))
            .fg(Color::LightCyan);

        let mut loading_frame = 0;
        let mut ipc_status = "Connecting to mpv...".to_string();
        let mut playback_time = 0.0_f64;
        let mut duration = 0.0_f64;

        // Observe playback-time and duration for progress display
        let time_rx = mpv.mpv_ipc.observe_prop::<f64>("playback-time", 0.0).await;
        let duration_rx = mpv.mpv_ipc.observe_prop::<f64>("duration", 0.0).await;

        loop {
            // Check if mpv is still running
            if !mpv.mpv_ipc.running().await {
                ratatui::restore();
                println!("Playback finished.");
                return Ok(());
            } else {
                let title = self.get_name().unwrap_or_default();

                // Update playback time from observer
                if let Ok(has_changed) = time_rx.has_changed() {
                    if has_changed {
                        playback_time = *time_rx.borrow();
                    }
                }

                // Update duration from observer
                if let Ok(has_changed) = duration_rx.has_changed() {
                    if has_changed {
                        duration = *duration_rx.borrow();
                        if duration > 0.0 {
                            ipc_status = "Playing!".to_string();
                        } else {
                            ipc_status = "Loading...".to_string();
                        }
                    }
                }

                // Check if media is ready (duration > 0 means we know the length)
                let media_ready = duration > 0.0;

                term.draw(|f| {
                    let area = f.area();

                    if !media_ready {
                        let popup_area = ratatui::layout::Rect::new(
                            area.x + (area.width as u16 / 4),
                            area.y + area.height as u16 / 2 - 4,
                            area.width as u16 / 2,
                            8,
                        );
                        let block = Block::default()
                            .title("[Starting MPV]")
                            .title_alignment(Alignment::Center)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan))
                            .border_type(BorderType::Rounded)
                            .style(Style::default().bg(Color::Rgb(30, 30, 45)));
                        f.render_widget(block, popup_area);

                        let inner_area = popup_area.inner(ratatui::layout::Margin {
                            vertical: 1,
                            horizontal: 2,
                        });

                        let loading_line = ratatui::layout::Rect::new(
                            inner_area.x,
                            inner_area.y + 2,
                            inner_area.width,
                            1,
                        );
                        let loading_text =
                            Paragraph::new(format!("{} Loading...", LOADING_FRAMES[loading_frame]))
                                .style(status_style.add_modifier(Modifier::BOLD))
                                .alignment(Alignment::Center);
                        f.render_widget(loading_text, loading_line);
                    } else {
                        // Draw the thumbnail on top, popup below
                        if let Some(thumbnail) = thumbnail {
                            let layout = Layout::vertical(Constraint::from_percentages([70, 30]))
                                .split(f.area());
                            let top = layout[0];
                            let bottom = layout[1];

                            let top_layout =
                                Layout::horizontal(Constraint::from_percentages([10, 80, 10]))
                                    .split(top);

                            let popup_area = ratatui::layout::Rect::new(
                                bottom.x + (bottom.width as u16 / 4),
                                bottom.y + 1,
                                bottom.width as u16 / 2,
                                bottom.height as u16 - 2,
                            );
                            let image_area = Rect {
                                x: popup_area.x + (popup_area.x / 5),
                                y: 0,
                                width: popup_area.width,
                                height: top_layout[1].height,
                            };
                            if let Ok(picker) = picker::Picker::from_query_stdio() {
                                if let Ok(protocol) = picker.new_protocol(
                                    thumbnail.clone(),
                                    image_area,
                                    ratatui_image::Resize::Scale(None),
                                ) {
                                    f.render_widget(Image::new(&protocol), image_area);
                                }
                            }
                            let block = Block::default()
                                .title("Now Playing")
                                .title_alignment(Alignment::Center)
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(Color::Cyan))
                                .border_type(BorderType::Rounded)
                                .style(Style::default().bg(Color::Rgb(30, 30, 45)));
                            f.render_widget(block, popup_area);

                            let inner_area = popup_area.inner(ratatui::layout::Margin {
                                vertical: 1,
                                horizontal: 2,
                            });

                            let title_area =
                                Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
                            let truncated_title = if title.len() > inner_area.width as usize {
                                format!("{}...", &title[..inner_area.width as usize - 3])
                            } else {
                                title.clone()
                            };
                            let title_text = Paragraph::new(truncated_title)
                                .style(status_style.add_modifier(Modifier::BOLD))
                                .alignment(Alignment::Center);
                            f.render_widget(title_text, title_area);

                            // Format times as MM:SS
                            let current_minutes = (playback_time as u64) / 60;
                            let current_seconds = (playback_time as u64) % 60;
                            let total_minutes = (duration as u64) / 60;
                            let total_seconds = (duration as u64) % 60;
                            let time_str = format!(
                                "{:02}:{:02} / {:02}:{:02}",
                                current_minutes, current_seconds, total_minutes, total_seconds
                            );

                            // Display time
                            let time_area =
                                Rect::new(inner_area.x, inner_area.y + 1, inner_area.width, 1);
                            let time_text = Paragraph::new(time_str)
                                .style(Style::default().fg(Color::LightCyan))
                                .alignment(Alignment::Center);
                            f.render_widget(time_text, time_area);

                            // Draw progress gauge
                            let gauge_area =
                                Rect::new(inner_area.x, inner_area.y + 2, inner_area.width, 1);
                            let progress = if duration > 0.0 {
                                (playback_time / duration).min(1.0).max(0.0)
                            } else {
                                0.0
                            };
                            let gauge_width = inner_area.width.saturating_sub(2);
                            let filled_width = (gauge_width as f64 * progress) as u16;
                            let _empty_width = gauge_width.saturating_sub(filled_width);

                            let gauge = ratatui::widgets::Gauge::default()
                                .ratio(progress as f64)
                                .gauge_style(Style::default().fg(Color::Yellow))
                                .style(Style::default().fg(Color::DarkGray))
                                .label("");
                            f.render_widget(gauge, gauge_area);

                            let quit_area =
                                Rect::new(inner_area.x, inner_area.y + 5, inner_area.width, 1);
                            let quit_text = Paragraph::new(ratatui::text::Text::from(
                                ratatui::text::Line::from(vec![
                                    Span::styled("Press ", quit_style),
                                    Span::styled(" [ q ] ", key_style),
                                    Span::styled("to stop", quit_style),
                                ]),
                            ))
                            .alignment(Alignment::Center);
                            f.render_widget(quit_text, quit_area);
                        } else {
                            let popup_area = ratatui::layout::Rect::new(
                                area.x + (area.width as u16 / 4),
                                area.y + area.height as u16 / 2 - 4,
                                area.width as u16 / 2,
                                8,
                            );
                            let block = Block::default()
                                .title("Now Playing")
                                .title_alignment(Alignment::Center)
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(Color::Cyan))
                                .border_type(BorderType::Rounded)
                                .style(Style::default().bg(Color::Rgb(30, 30, 45)));
                            f.render_widget(block, popup_area);

                            let inner_area = popup_area.inner(ratatui::layout::Margin {
                                vertical: 1,
                                horizontal: 2,
                            });

                            let title_area =
                                Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
                            let truncated_title = if title.len() > inner_area.width as usize {
                                format!("{}...", &title[..inner_area.width as usize - 3])
                            } else {
                                title.clone()
                            };
                            let title_text = Paragraph::new(truncated_title)
                                .style(status_style.add_modifier(Modifier::BOLD))
                                .alignment(Alignment::Center);
                            f.render_widget(title_text, title_area);

                            let current_minutes = (playback_time as u64) / 60;
                            let current_seconds = (playback_time as u64) % 60;
                            let total_minutes = (duration as u64) / 60;
                            let total_seconds = (duration as u64) % 60;
                            let time_str = format!(
                                "{:02}:{:02} / {:02}:{:02}",
                                current_minutes, current_seconds, total_minutes, total_seconds
                            );

                            let time_area =
                                Rect::new(inner_area.x, inner_area.y + 1, inner_area.width, 1);
                            let time_text = Paragraph::new(time_str)
                                .style(Style::default().fg(Color::LightCyan))
                                .alignment(Alignment::Center);
                            f.render_widget(time_text, time_area);

                            let gauge_area =
                                Rect::new(inner_area.x, inner_area.y + 2, inner_area.width, 1);
                            let progress = if duration > 0.0 {
                                (playback_time / duration).min(1.0).max(0.0)
                            } else {
                                0.0
                            };
                            let gauge_width = inner_area.width.saturating_sub(2);
                            let filled_width = (gauge_width as f64 * progress) as u16;
                            let _empty_width = gauge_width.saturating_sub(filled_width);

                            let gauge = ratatui::widgets::Gauge::default()
                                .ratio(progress as f64)
                                .gauge_style(Style::default().fg(Color::Yellow))
                                .style(Style::default().fg(Color::DarkGray))
                                .label("");
                            f.render_widget(gauge, gauge_area);

                            let quit_area =
                                Rect::new(inner_area.x, inner_area.y + 5, inner_area.width, 1);
                            let quit_text = Paragraph::new(ratatui::text::Text::from(
                                ratatui::text::Line::from(vec![
                                    Span::styled("Press ", quit_style),
                                    Span::styled(" [ q ] ", key_style),
                                    Span::styled("to stop", quit_style),
                                ]),
                            ))
                            .alignment(Alignment::Center);
                            f.render_widget(quit_text, quit_area);
                        }
                    }
                })
                .expect("Terminal draw crashed");

                loading_frame = (loading_frame + 1) % LOADING_FRAMES.len();

                if poll(Duration::from_millis(100))? {
                    if let Ok(Event::Key(key_event)) = read() {
                        match key_event.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                ratatui::restore();
                                mpv.quit().await;
                                println!("Playback stopped by user.");
                                return Ok(());
                            }
                            KeyCode::Left => {
                                if key_event.kind == KeyEventKind::Press {
                                    let _ = mpv
                                        .mpv_ipc
                                        .send_command(json!(["seek", "-5", "relative"]))
                                        .await;
                                }
                            }
                            KeyCode::Right => {
                                if key_event.kind == KeyEventKind::Press {
                                    let _ = mpv
                                        .mpv_ipc
                                        .send_command(json!(["seek", "5", "relative"]))
                                        .await;
                                }
                            }
                            _ => {}
                        }
                    }
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

            let video_id = self.get_id().unwrap();
            let url = format!("https://www.youtube.com/watch?v={video_id}");
            let thumbnail_url = format!("https://img.youtube.com/vi/{video_id}/hqdefault.jpg");
            let thumbnail_bytes = reqwest::Client::new()
                .get(&thumbnail_url)
                .send()
                .await?
                .bytes()
                .await?;
            let thumbnail = image::load_from_memory(&thumbnail_bytes)?;

            if !check_mpv_installed() {
                bail!(
                    "MPV is not installed or not in PATH.\n   Please install MPV to listen to music.\n   Windows: winget install mpv\n   macOS: brew install mpv\n   Linux: sudo apt install mpv"
                );
            }

            let mut mpv = MpvPlayer::new(true, &url)
                .await
                .context("Failed to start MPV")?;
            self.watch_playback(&mut mpv, &mut Some(thumbnail)).await?;
        }
    }

    async fn yt_dlp(&mut self) -> Result<()> {
        let output_dir = PathBuf::from("output");
        let libraries_dir = PathBuf::from("libs");
        let libs_installed = check_libs_installed();

        // Build action options based on whether libs are installed
        let actions = if libs_installed {
            vec!["Download", "Update"]
        } else {
            vec!["Download", "Update", "Install"]
        };

        match Select::new("Action", actions)
            .with_page_size(6)
            .with_help_message("Type to filter | Arrow keys to navigate")
            .prompt()
            .context("Failed to display YT-DLP action menu")
        {
            Ok("Download") => {
                // For now, use YouTube Music search - can be extended for YouTube later
                let yt_choice = vec!["Youtube", "Youtube Music"];
                let res = match inquire::Select::new("Download from", yt_choice).prompt() {
                    Ok(c) => c,
                    Err(e) => panic!("{}", e.to_string()),
                };
                match res {
                    "Youtube" => {
                        self.ytdlp_download(false, &output_dir, &libraries_dir)
                            .await?;
                    }
                    "Youtube Music" => {
                        self.ytdlp_download(true, &output_dir, &libraries_dir)
                            .await?;
                    }
                    _ => panic!("Should not happen"),
                }
            }
            Ok("Update") => {
                let ytdlp = libraries_dir.join("yt-dlp");
                let ffmpeg = libraries_dir.join("ffmpeg");

                let libraries = Libraries::new(ytdlp, ffmpeg);
                let fetcher = Youtube::new(libraries, output_dir);

                if let Ok(ytdlp) = fetcher {
                    println!("Checking for yt-dlp updates...");
                    if let Err(e) = ytdlp.update_downloader().await {
                        bail!(
                            "Update failed: {}.\n   You may already have the latest version or no internet connection.",
                            e
                        );
                    }
                    println!("yt-dlp updated successfully!");
                } else {
                    bail!(
                        "Failed to initialize downloader for update.\n   Make sure yt-dlp and ffmpeg are in the 'libs' directory."
                    );
                }
            }
            Ok("Install") => {
                println!("Downloading yt-dlp and ffmpeg binaries...");
                println!("   This may take a few minutes depending on your connection...");

                std::fs::create_dir_all(&libraries_dir)
                    .context("Failed to create libraries directory")?;
                std::fs::create_dir_all(&output_dir)
                    .context("Failed to create output directory")?;

                let fetcher =
                    Youtube::with_new_binaries(libraries_dir.clone(), output_dir.clone()).await;
                match fetcher {
                    Ok(_) => {
                        // Verify binaries actually exist
                        let ytdlp_exists = libraries_dir.join("yt-dlp.exe").exists()
                            || libraries_dir.join("yt-dlp").exists();
                        let ffmpeg_exists = libraries_dir.join("ffmpeg.exe").exists()
                            || libraries_dir.join("ffmpeg").exists()
                            || libraries_dir.join("ffmpeg-release").exists();

                        if ytdlp_exists && ffmpeg_exists {
                            println!("Libraries installed successfully!");
                            println!(
                                "   yt-dlp and ffmpeg are now available in the 'libs' directory."
                            );
                        } else {
                            println!(
                                "Warning: Installation completed but some binaries may be missing."
                            );
                            if !ytdlp_exists {
                                println!("   Missing: yt-dlp");
                            }
                            if !ffmpeg_exists {
                                println!("   Missing: ffmpeg");
                            }
                        }
                    }
                    Err(e) => {
                        // Check if binaries exist despite error (partial success)
                        let ytdlp_exists = libraries_dir.join("yt-dlp.exe").exists()
                            || libraries_dir.join("yt-dlp").exists();
                        let ffmpeg_exists = libraries_dir.join("ffmpeg.exe").exists()
                            || libraries_dir.join("ffmpeg").exists()
                            || libraries_dir.join("ffmpeg-release").exists();

                        if ytdlp_exists && ffmpeg_exists {
                            println!("Libraries installed successfully!");
                            println!(
                                "   yt-dlp and ffmpeg are now available in the 'libs' directory."
                            );
                        } else {
                            bail!(
                                "Installation failed: {}.\n   Please check your internet connection and try again.",
                                e
                            );
                        }
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

    fn find_ffmpeg_path(libraries_dir: &PathBuf) -> Result<PathBuf> {
        if libraries_dir.join("ffmpeg.exe").exists() {
            return Ok(libraries_dir.join("ffmpeg.exe"));
        }
        if libraries_dir.join("ffmpeg").exists() {
            return Ok(libraries_dir.join("ffmpeg"));
        }
        if libraries_dir
            .join("ffmpeg-release")
            .join("ffmpeg.exe")
            .exists()
        {
            return Ok(libraries_dir.join("ffmpeg-release").join("ffmpeg.exe"));
        }
        let mut ffmpeg_path = None;
        if let Ok(entries) = std::fs::read_dir(libraries_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let bin_path = entry.path().join("bin").join("ffmpeg.exe");
                    if bin_path.exists() {
                        ffmpeg_path = Some(bin_path);
                        break;
                    }

                    if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                        for sub_entry in sub_entries.flatten() {
                            if sub_entry.path().is_dir() {
                                let nested_bin = sub_entry.path().join("bin").join("ffmpeg.exe");
                                if nested_bin.exists() {
                                    ffmpeg_path = Some(nested_bin);
                                    break;
                                }
                            }
                        }
                    }
                    if ffmpeg_path.is_some() {
                        break;
                    }
                }
            }
        }

        match ffmpeg_path {
            Some(p) => Ok(p),
            None => bail!("ffmpeg not found in libs directory. Searched: libs/ and subdirectories"),
        }
    }

    async fn convert_m4a_to_mp3(
        input_path: &PathBuf,
        output_path: &PathBuf,
        ffmpeg_path: &PathBuf,
    ) -> Result<()> {
        eprintln!("Converting to MP3 with VBR quality...");

        let output = Command::new(ffmpeg_path)
            .args(&[
                "-i",
                input_path.to_string_lossy().as_ref(),
                "-c:v",
                "copy",
                "-c:a",
                "libmp3lame",
                "-q:a",
                "4",
                output_path.to_string_lossy().as_ref(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .context("Failed to execute ffmpeg for conversion")?;

        if output.status.success() {
            std::fs::remove_file(input_path)?;
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("FFmpeg conversion failed: {}", stderr);
        }
    }

    async fn ytdlp_download(
        &mut self,
        yt_music: bool,
        output_dir: &PathBuf,
        libraries_dir: &PathBuf,
    ) -> Result<(), anyhow::Error> {
        if yt_music {
            let (yt_music_query, _) = YtMusicQuery::new_music_search(None).await?;
            *self = Self::Ytdlp {
                yt_query: Query::YtMusicQuery(yt_music_query),
            };
        } else {
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

            println!("Searching for: {}", query);
            match YTQuery::from(query.as_str()).await {
                Ok(yt) => {
                    *self = Self::Ytdlp {
                        yt_query: Query::YtQuery(yt),
                    };
                }
                Err(e) => {
                    panic!("Search failed: {}.", e);
                }
            }
        }
        let url = format!("https://www.youtube.com/watch?v={}", self.get_id().unwrap());

        let ytdlp = if libraries_dir.join("yt-dlp.exe").exists() {
            libraries_dir.join("yt-dlp.exe")
        } else if libraries_dir.join("yt-dlp").exists() {
            libraries_dir.join("yt-dlp")
        } else {
            bail!("yt-dlp not found in libs directory");
        };

        let ffmpeg = Self::find_ffmpeg_path(libraries_dir)?;

        let libraries = Libraries::new(ytdlp, ffmpeg);
        let fetcher = Youtube::new(libraries, output_dir.clone())
            .context("Failed to initialize YouTube downloader.\n   Make sure yt-dlp and ffmpeg are in the 'libs' directory.")?;
        Ok(
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
                    println!("Downloading video as {}...", extension);
                    match fetcher
                        .download_video_from_url(
                            url.clone(),
                            format!(
                                "{}.{}",
                                self.get_name().unwrap().replace(" ", "_"),
                                extension
                            ),
                        )
                        .await
                    {
                        Ok(path) => {
                            println!("Video downloaded successfully!");
                            println!("📁 Location: {}", path.to_string_lossy());
                        }
                        Err(e) => {
                            bail!(
                                "Download failed: {}.\n   Please check your internet connection and try again.",
                                e
                            );
                        }
                    }
                }
                Ok("Audio-Only") => {
                    let extension = Select::new("Extension", vec!["mp3", "m4a"])
                        .with_page_size(8)
                        .prompt()
                        .context("Failed to select audio extension")?;

                    println!("Downloading audio as {}...", extension);

                    let ffmpeg_convert_path = Self::find_ffmpeg_path(libraries_dir)?;

                    let m4a_name = format!("{}.m4a", self.get_name().unwrap());

                    match fetcher
                        .download_audio_stream_from_url(url.clone(), m4a_name.clone())
                        .await
                    {
                        Ok(path) => {
                            if extension == "mp3" {
                                let mp3_path = path.with_extension("mp3");
                                match Self::convert_m4a_to_mp3(
                                    &path,
                                    &mp3_path,
                                    &ffmpeg_convert_path,
                                )
                                .await
                                {
                                    Ok(_) => {
                                        println!(
                                            "Audio downloaded and converted to MP3 successfully!"
                                        );
                                        println!("📁 Location: {}", mp3_path.to_string_lossy());
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "Warning: MP3 conversion failed: {}. Keeping m4a file.",
                                            e
                                        );
                                        println!("Audio downloaded successfully (m4a)!");
                                        println!("📁 Location: {}", path.to_string_lossy());
                                    }
                                }
                            } else {
                                let final_path = path;
                                println!("Audio downloaded successfully!");
                                println!("📁 Location: {}", final_path.to_string_lossy());
                            }
                        }
                        Err(e) => {
                            bail!("Audio download failed: {}. Try m4a format.", e);
                        }
                    }
                }
                Ok("Video-Only") => {
                    let extension = Select::new("Extension", vec!["mp4", "avi", "mov", "webm"])
                        .with_page_size(6)
                        .prompt()
                        .context("Failed to select video extension")?;
                    println!("Downloading video-only stream as {}...", extension);
                    match fetcher
                        .download_video_stream_from_url(
                            url.clone(),
                            format!("{}.{}", self.get_name().unwrap(), extension),
                        )
                        .await
                    {
                        Ok(path) => {
                            println!("Video stream downloaded successfully!");
                            println!("📁 Location: {}", path.to_string_lossy());
                        }
                        Err(e) => {
                            bail!(
                                "Video stream download failed: {}.\n   Please check your internet connection and try again.",
                                e
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    bail!("Download selection failed: {}", e);
                }
            },
        )
    }

    fn get_id(&self) -> Result<String> {
        match self {
            YTRSAction::None => bail!("No video selected. Please search and select a video first."),
            YTRSAction::Ytdlp { yt_query } => Ok(match yt_query {
                Query::YtQuery(ytquery) => ytquery.video.id.clone(),
                Query::YtMusicQuery(yt_music_query) => yt_music_query.video.id.clone(),
            }),
            YTRSAction::Watch { yt_query } => Ok(yt_query.video.id.clone()),
            YTRSAction::Listen {
                yt_query,
                search_term: _,
            } => Ok(yt_query.video.id.clone()),
        }
    }

    fn get_name(&self) -> Result<String> {
        match self {
            YTRSAction::None => bail!("No video selected. Please search and select a video first."),
            YTRSAction::Ytdlp { yt_query } => Ok(match yt_query {
                Query::YtQuery(ytquery) => ytquery.video.name.clone(),
                Query::YtMusicQuery(yt_music_query) => yt_music_query.video.name.clone(),
            }),
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

        println!("Searching YouTube Music for: {}", search_term);

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
            .map(|track| TrackInfo::from(&track).to_string())
            .collect();
        found_videos_str.push("Exit".to_owned());
        let selected_vid_str = Select::new("Select Music", found_videos_str)
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
            .find(|track| TrackInfo::from(track).to_string() == selected_vid_str)
        {
            Ok((Self { video: vid }, search_term))
        } else {
            bail!("Selected music not found. Please try again.");
        }
    }
}

pub struct TrackInfo {
    artists: String,
    track_name: String,
    id: String,
    duration: Option<u32>,
    view_count: Option<u64>,
}

impl From<&TrackItem> for TrackInfo {
    fn from(value: &TrackItem) -> Self {
        Self {
            artists: value.artists.iter().map(|a| a.name.clone()).collect(),
            track_name: value.name.clone(),
            id: value.id.clone(),
            duration: value.duration.clone(),
            view_count: value.view_count.clone(),
        }
    }
}

impl std::fmt::Display for TrackInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Track name: '{}'{}{}\nArtist(s): [{}]",
            self.track_name.clone().green(),
            match self.duration {
                Some(d) => {
                    let hours = d / 3600;
                    let minutes = (d % 3600) / 60;
                    let secs = d % 60;
                    format!(
                        " [{}{}{}]",
                        if hours > 0 {
                            format!("{hours:02}:").green()
                        } else {
                            "".to_owned().green()
                        },
                        if minutes > 0 {
                            format!("{minutes:02}:").green()
                        } else {
                            "".to_string().green()
                        },
                        format!("{secs:02}").green()
                    )
                }
                None => {
                    "".to_string()
                }
            },
            match self.view_count {
                Some(views) => format!(" Views: {}", views).dark_blue(),
                None => "".to_owned().dark_blue(),
            },
            self.artists.clone().blue()
        )
    }
}

pub struct YTQuery {
    video: VideoItem,
}

impl YTQuery {
    pub async fn from(query: &str) -> Result<Self> {
        println!("Searching YouTube for: {}", query);

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
            .map(|v: &VideoItem| format!("-> {}", v.name))
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
            bail!("Selected video not found. Please try again.");
        }
    }
}
