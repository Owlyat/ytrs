use crate::mpv::{MpvIpc, MpvSpawnOptions};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{Timelike, Utc};
use image::DynamicImage;
use inquire::{Confirm, Select, Text as InquireText, validator::Validation};
use inquire_derive::Selectable;
use ollama_rs::Ollama;
use ollama_rs::generation::completion::request::GenerationRequest;
use ratatui::layout::Flex;
use ratatui::prelude::*;
use ratatui::style::Stylize;
use ratatui::widgets::{Gauge, List, ListItem, ListState};
use ratatui::{
    crossterm::event::{KeyCode, read},
    layout::{Constraint, Layout},
    widgets::{Block, Paragraph},
};
use ratatui_image::{StatefulImage, picker};
use rustypipe::{
    client::RustyPipe,
    model::{TrackItem, VideoItem},
};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use strum::IntoEnumIterator;
use thiserror::Error;
use yt_dlp::Youtube;
use yt_dlp::client::Libraries;
use yt_dlp::model::VideoCodecPreference;
use yt_dlp::model::caption::Subtitle;

use crate::utility::format_time;

pub struct YoutubeRs {
    pub api: YoutubeAPI,
    pub action: AppAction,
    pub mpv_installed: bool,
    pub last_search: Option<String>,
}

#[derive(strum::Display, strum::EnumIter, Clone, Copy)]
pub enum AppAction {
    Download { format: Format },
    Transcript,
    Player { format: Format },
    Quit,
}

#[derive(strum::Display, strum::EnumIter, Default, Clone, Selectable, Debug, Copy)]
pub enum YoutubeAPI {
    Music,
    #[default]
    Video,
}
#[derive(Copy, Debug, Selectable, strum::Display, Clone)]
pub enum FormatInquire {
    Audio,
    Video,
}

#[derive(strum::Display, strum::EnumIter, Clone, PartialEq, Copy)]
pub enum Format {
    Audio { format: AudioFormat },
    Video { format: VideoFormat },
}

#[derive(Clone, strum::Display, strum::EnumIter, Default, PartialEq, Copy, Debug, Selectable)]
pub enum AudioFormat {
    #[default]
    MP3,
    WAV,
}

#[derive(Clone, strum::Display, strum::EnumIter, Default, PartialEq, Copy, Selectable, Debug)]
pub enum VideoFormat {
    #[default]
    MP4,
    AVI,
    MOV,
}

pub struct VideoInfo {
    channel: Option<String>,
    name: String,
    _view_count: Option<u64>,
    duration: Option<u32>,
}

pub struct TrackInfo {
    artists: String,
    track_name: String,
    _id: String,
    duration: Option<u32>,
    view_count: Option<u64>,
}

#[derive(Clone)]
pub enum YoutubeResponse {
    Video(VideoItem),
    Track(TrackItem),
}

#[derive(Error, Debug)]
pub enum YtrsError {
    #[error("MPV not installed or not found in PATH")]
    MpvNotFound,
    #[error("Quit successfully")]
    Quit,
}

impl YoutubeResponse {
    pub fn get_id(&self) -> String {
        match self {
            YoutubeResponse::Video(video_item) => video_item.id.clone(),
            YoutubeResponse::Track(track_item) => track_item.id.clone(),
        }
    }
    pub fn get_name(&self) -> String {
        match self {
            YoutubeResponse::Video(video_item) => video_item.name.clone(),
            YoutubeResponse::Track(track_item) => track_item.name.clone(),
        }
    }
    pub fn get_duration(&self) -> u32 {
        match self {
            YoutubeResponse::Video(video_item) => video_item.duration.unwrap_or_default(),
            YoutubeResponse::Track(track_item) => track_item.duration.unwrap_or_default(),
        }
    }
}

impl YoutubeRs {
    pub async fn process(&mut self) -> Result<()> {
        match self.action.clone() {
            AppAction::Download { format } => {
                if !self.check_libraries() {
                    Self::install_lib().await?;
                }
                let (video_id, video_name) = match self.api {
                    YoutubeAPI::Music => {
                        let (track, search) = Self::query_ytmusic(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        (track.id.clone(), track.name.clone())
                    }
                    YoutubeAPI::Video => {
                        let (video, search) = Self::query_ytvideo(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        (video.id.clone(), video.name.clone())
                    }
                };
                let url = format!("https://www.youtube.com/watch?v={video_id}");
                match format {
                    Format::Audio { format } => {
                        self.download_audio(&url, &video_name, format).await?;
                    }
                    Format::Video { format } => {
                        self.download_video(&url, &video_name, format).await?;
                    }
                }
            }
            AppAction::Transcript => {
                if !self.check_libraries() {
                    Self::install_lib().await?;
                }
                let video_id = match self.api {
                    YoutubeAPI::Music => {
                        let (track, search) = Self::query_ytmusic(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        track.id.clone()
                    }
                    YoutubeAPI::Video => {
                        let (video, search) = Self::query_ytvideo(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        video.id.clone()
                    }
                };
                self.download_transcript(&video_id).await?;
            }
            AppAction::Player { format } => {
                if !self.mpv_installed {
                    self.mpv_installed = Self::check_mpv()?;
                }
                let mut response = match self.api {
                    YoutubeAPI::Music => {
                        let res = Self::query_ytmusic(self.last_search.clone()).await?;
                        self.last_search = Some(res.1);
                        YoutubeResponse::Track(res.0)
                    }
                    YoutubeAPI::Video => {
                        let res = Self::query_ytvideo(self.last_search.clone()).await?;
                        self.last_search = Some(res.1);
                        YoutubeResponse::Video(res.0)
                    }
                };
                match format {
                    Format::Audio { .. } => {
                        let mut opt_thumbnail =
                            Self::fetch_yt_thumbnail(&response.get_id()).await.ok();
                        self.player(&mut response, &mut opt_thumbnail, true).await;
                    }
                    Format::Video { .. } => {
                        let mut opt_thumbnail =
                            Self::fetch_yt_thumbnail(&response.get_id()).await.ok();
                        self.player(&mut response, &mut opt_thumbnail, false).await;
                    }
                }
            }
            AppAction::Quit => return Err(YtrsError::Quit.into()),
        }
        Ok(())
    }
    async fn player(
        &mut self,
        response: &mut YoutubeResponse,
        opt_thumbnail: &mut Option<DynamicImage>,
        audio_only: bool,
    ) {
        let opts = MpvSpawnOptions::default();
        let mut mpv = MpvIpc::spawn(&opts, audio_only)
            .await
            .context("Failed to spawn mpv process")
            .expect("Could not spawn MPV");
        mpv.send_command(json!(["loadfile", Self::get_video_url(&response.get_id())]))
            .await
            .context("Failed to load media")
            .expect("Could not send command to MPV");

        let mut term = ratatui::init();
        let time_rx = mpv.observe_prop::<f64>("playback-time", 0.0).await;
        let mut playback_time = 0.0;
        let mut vid_started = false;
        let loader = ["/", "|", "\\", "-"];
        let mut loader_idx = 0;
        let mut pause_state = false;
        let mut open_popup = false;
        let mut videos_list: Vec<(String, YoutubeResponse)> = Vec::new();
        let mut selected_list_item = ListState::default();
        let mut popup_query = String::new();
        let mut img = if let Some(dyn_thumbnail) = &opt_thumbnail
            && let Ok(picker) = picker::Picker::from_query_stdio()
        {
            let protocol = picker.new_resize_protocol(dyn_thumbnail.clone());
            Some(protocol)
        } else {
            None
        };

        loop {
            if !mpv.running().await {
                break;
            }
            if let Ok(has_changed) = time_rx.has_changed()
                && has_changed
            {
                // Mpv found the video
                playback_time = *time_rx.borrow();
                if playback_time == 0.0 && !vid_started {
                    vid_started = true;
                }
            }

            let _ = term.draw(|f| {
                if vid_started {
                    let layout = Layout::vertical(Constraint::from_percentages([60, 40]))
                        .flex(Flex::SpaceEvenly)
                        .split(f.area());
                    // Top
                    if let Some(protocol) = &mut img {
                        let img_layout = layout[0];
                        f.render_stateful_widget(
                            StatefulImage::default(),
                            img_layout
                                .centered(Constraint::Percentage(25), Constraint::Percentage(75)),
                            protocol,
                        );
                    }

                    // Bottom
                    let info_layout = layout[1];
                    let info_layout = info_layout.centered_horizontally(Constraint::Percentage(50));
                    if open_popup {
                        let areas = Layout::vertical([Constraint::Length(3), Constraint::Fill(3)])
                            .split(info_layout);
                        Paragraph::new(format!("YTSearch: {popup_query}"))
                            .block(
                                Block::bordered()
                                    .title_top("Search")
                                    .title_alignment(HorizontalAlignment::Center)
                                    .yellow()
                                    .on_blue(),
                            )
                            .render(areas[0], f.buffer_mut());
                        let list = List::new(
                            videos_list
                                .iter()
                                .map(|v| ListItem::from(v.0.clone()))
                                .collect::<Vec<ListItem>>(),
                        )
                        .block(
                            Block::bordered()
                                .title_bottom(
                                    format!("[▼▲ Select Entry | (Esc) Player | (Enter) Search/Play Entry | Tab Change Api: {}]",self.api),
                                )
                                .style(Style::default().yellow().on_blue()),
                        )
                        .highlight_symbol(">")
                        .highlight_style(Style::default().red().on_cyan())
                        .direction(ratatui::widgets::ListDirection::TopToBottom);
                        f.render_stateful_widget(list, areas[1], &mut selected_list_item);
                    } else {
                        Block::bordered()
                            .style(Style::default().on_blue().yellow())
                            .title_top(format!(
                                "{} - {}:{}",
                                response.get_name(),
                                format_time(playback_time as u32),
                                format_time(response.get_duration()),
                            ))
                            .title_alignment(HorizontalAlignment::Center)
                            .title_bottom("['q' Quit | ▲▼ Volume(+/-) | ◀▶ Seek | 'o' YtSearch]")
                            .title_alignment(HorizontalAlignment::Center)
                            .render(info_layout, f.buffer_mut());
                        let gauge_layout = info_layout
                            .inner(Margin {
                                horizontal: 1,
                                vertical: 1,
                            })
                            .centered_vertically(Constraint::Percentage(50));
                        Gauge::default()
                            .block(Block::bordered().style(Style::default().yellow().on_blue()))
                            .ratio(playback_time / response.get_duration() as f64)
                            .render(gauge_layout, f.buffer_mut());
                    }
                } else {
                    if Utc::now().second().is_multiple_of(2) {
                        loader_idx += 1 % loader.len();
                    }
                    Block::bordered()
                        .title(format!("[Loading MPV {}]", loader[loader_idx]))
                        .render(f.area(), f.buffer_mut());
                }
            });
            let event_happened = ratatui::crossterm::event::poll(Duration::from_millis(50)).ok();
            if let Some(has_happened) = event_happened
                && has_happened
            {
                let event = read().unwrap();
                if open_popup {
                    if event.is_key_press()
                        && let KeyCode::Char(ch) = event.as_key_event().unwrap().code
                    {
                        popup_query.push(ch);
                    }
                    if event.is_key_press()
                        && event.as_key_event().unwrap().code == KeyCode::Backspace
                    {
                        popup_query.pop();
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Tab {
                        self.api = match self.api {
                            YoutubeAPI::Music => YoutubeAPI::Video,
                            YoutubeAPI::Video => YoutubeAPI::Music,
                        };
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Up {
                        selected_list_item.select_previous();
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Down {
                        selected_list_item.select_next();
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Esc {
                        open_popup = !open_popup;
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Enter
                    {
                        if let Some(selected) = selected_list_item.selected() {
                            if let Some(vid) = videos_list.get(selected).map(|v| v.1.clone()) {
                                popup_query.clear();
                                mpv.send_command(json!([
                                    "loadfile",
                                    Self::get_video_url(&vid.get_id())
                                ]))
                                .await
                                .context("Failed to load media")
                                .expect("Could not send command to MPV");
                                if let Ok(thumbnail) = Self::fetch_yt_thumbnail(&vid.get_id()).await
                                {
                                    img = if let Ok(picker) = picker::Picker::from_query_stdio() {
                                        let protocol =
                                            picker.new_resize_protocol(thumbnail.clone());
                                        Some(protocol)
                                    } else {
                                        None
                                    };
                                } else {
                                    img = None;
                                }
                                *response = vid;
                                videos_list.clear();
                            }
                        } else {
                            match self.api {
                                YoutubeAPI::Music => {
                                    let rp = RustyPipe::new();
                                    let found_videos = rp
                                        .query()
                                        .unauthenticated()
                                        .music_search_tracks(popup_query.clone())
                                        .await
                                        .context("Failed to search YouTube Music")
                                        .expect("Failed to fetch youtube with rustypipe");
                                    YoutubeRs::cleanup_rustypipe_cache();
                                    videos_list = found_videos
                                        .clone()
                                        .items
                                        .items
                                        .into_iter()
                                        .map(|track| {
                                            (TrackInfo::from(&track).to_string(), track.into())
                                        })
                                        .collect();
                                }
                                YoutubeAPI::Video => {
                                    let found_videos = RustyPipe::new()
                                        .query()
                                        .unauthenticated()
                                        .search(popup_query.clone())
                                        .await
                                        .context("Failed to search YouTube")
                                        .unwrap();
                                    YoutubeRs::cleanup_rustypipe_cache();
                                    videos_list = found_videos
                                        .items
                                        .items
                                        .iter()
                                        .map(|v| (VideoInfo::from(v).to_string(), v.into()))
                                        .collect();
                                }
                            }
                        }
                    }
                } else {
                    if event.is_key_press()
                        && event.as_key_event().unwrap().code == KeyCode::Char('q')
                    {
                        break;
                    }
                    if event.is_key_press()
                        && event.as_key_event().unwrap().code == KeyCode::Char('y')
                    {
                        let current_url = Self::get_video_url(&response.get_id());
                        let _ = Self::clipboard(&current_url);
                    }
                    if event.is_key_press()
                        && event.as_key_event().unwrap().code == KeyCode::Char(' ')
                    {
                        pause_state = !pause_state;
                        let _ = mpv.set_prop("pause", pause_state).await;
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Right
                    {
                        let _ = mpv.send_command(json!(["seek", "5", "relative"])).await;
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Left {
                        let _ = mpv.send_command(json!(["seek", "-5", "relative"])).await;
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Up {
                        let _ = mpv.send_command(json!(["add", "volume", "5"])).await;
                    }
                    if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Down {
                        let _ = mpv.send_command(json!(["add", "volume", "-5"])).await;
                    }
                    if event.is_key_press()
                        && event.as_key_event().unwrap().code == KeyCode::Char('o')
                    {
                        open_popup = !open_popup;
                    }
                }
            }
        }
        mpv.quit().await;
        ratatui::restore();
    }

    fn clipboard(text: &str) -> Result<()> {
        terminal_clipboard::set_string(text)
            .map_err(|e| anyhow::anyhow!("Clipboard error: {:?}", e))?;
        Ok(())
    }
    fn get_video_url(video_id: &String) -> String {
        format!("https://www.youtube.com/watch?v={video_id}")
    }
    fn cleanup_rustypipe_cache() {
        std::fs::remove_file("./rustypipe_cache.json").expect("Could not clean cache");
    }

    async fn fetch_yt_thumbnail(video_id: &str) -> Result<DynamicImage> {
        let thumbnail_url = if Self::ytdlp_exist() {
            Self::get_fetcher()
                .await?
                .fetch_video_infos(String::from(video_id))
                .await?
                .thumbnail
        } else {
            format!("https://img.youtube.com/vi/{video_id}/hqdefault.jpg")
        };
        let thumbnail_bytes = reqwest::Client::new()
            .get(&thumbnail_url)
            .send()
            .await?
            .bytes()
            .await?;
        Ok(image::load_from_memory(&thumbnail_bytes)?)
    }

    async fn download_audio(&self, url: &str, video_name: &str, format: AudioFormat) -> Result<()> {
        let fetcher = Self::get_fetcher().await?;
        let safe_name =
            video_name.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "_");
        let downloaded = fetcher
            .download_audio_stream_with_quality(
                url.to_string(),
                format!("{safe_name}.{}", format.to_string().to_lowercase()),
                yt_dlp::model::AudioQuality::Best,
                yt_dlp::model::AudioCodecPreference::Custom(format.to_string()),
            )
            .await?;
        println!("Audio downloaded at '{downloaded:?}'");
        Ok(())
    }

    async fn download_video(&self, url: &str, video_name: &str, format: VideoFormat) -> Result<()> {
        let fetcher = Self::get_fetcher().await?;
        let safe_name =
            video_name.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "_");
        let downloaded = fetcher
            .download_video_with_quality(
                url.to_string(),
                format!("{safe_name}.{}", format.to_string().to_lowercase()),
                yt_dlp::model::VideoQuality::Best,
                VideoCodecPreference::Custom(format.to_string()),
                yt_dlp::model::AudioQuality::Best,
                yt_dlp::model::AudioCodecPreference::MP3,
            )
            .await?;
        println!("Video Downloaded at '{downloaded:?}'");
        Ok(())
    }

    async fn download_transcript(&self, video_id: &str) -> Result<()> {
        let fetcher = Self::get_fetcher().await?;

        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let video = fetcher.fetch_video_infos(url).await?;

        let languages = fetcher.list_subtitle_languages(&video);
        if languages.is_empty() {
            println!("Finding Generated Captions");
            let cap: Vec<(String, &Vec<yt_dlp::model::caption::AutomaticCaption>)> = video
                .automatic_captions
                .iter()
                .map(|v| (v.0.clone(), v.1))
                .collect();
            if cap.is_empty() {
                println!("No Generated Caption found");
                if !video.description.is_empty() {
                    println!("{}: \n{}", "Video Description".green(), video.description);
                }
                return Ok(());
            }
            let lang = match inquire::Select::new(
                "Generated Lang",
                cap.iter().map(|(lang, _)| lang.clone()).collect(),
            )
            .prompt()
            {
                Ok(l) => l,
                Err(e) => match e {
                    inquire::InquireError::OperationCanceled => Err(anyhow!(YtrsError::Quit))?,
                    _ => Err(e)?,
                },
            };
            for (l, cap) in cap {
                if lang == l {
                    let res: Vec<Subtitle> = cap
                        .iter()
                        .map(|v| Subtitle::from_automatic_caption(v, l.clone()))
                        .collect();
                    let res_to_dl = match inquire::Select::new("Caption", res).prompt() {
                        Ok(res) => res,
                        Err(e) => match e {
                            inquire::InquireError::OperationCanceled => {
                                Err(anyhow!(YtrsError::Quit))?
                            }
                            _ => Err(e)?,
                        },
                    };
                    let response = reqwest::Client::new()
                        .get(res_to_dl.url.clone())
                        .send()
                        .await?
                        .text()
                        .await?;
                    let mut f = OpenOptions::new().write(true).create(true).open(format!(
                        "output/subtitle_{l}.{}",
                        res_to_dl.file_extension()
                    ))?;
                    f.write_all(response.as_bytes())?;
                    println!(
                        "AutoGenerated Captions downloaded at 'output/subtitle_{l}.{}'",
                        res_to_dl.file_extension()
                    );
                    let res = match inquire::Confirm::new("Summarize with ai ?")
                        .with_starting_input("N")
                        .prompt()
                    {
                        Ok(b) => b,
                        Err(e) => match e {
                            inquire::InquireError::OperationCanceled => {
                                Err(anyhow!(YtrsError::Quit))?
                            }
                            _ => Err(e)?,
                        },
                    };
                    if res {
                        use tokio::io::{self, AsyncWriteExt};
                        use tokio_stream::StreamExt;

                        let ollama = Ollama::default();
                        let models = ollama.list_local_models().await?;
                        let model = match inquire::Select::new(
                            "Which LLM to use:",
                            models.iter().map(|llm| llm.name.clone()).collect(),
                        )
                        .prompt()
                        {
                            Ok(v) => v,
                            Err(e) => match e {
                                inquire::InquireError::OperationCanceled => {
                                    Err(anyhow!(YtrsError::Quit))?
                                }
                                _ => Err(e)?,
                            },
                        };
                        println!("Generating response ...\n");
                        let mut stream = ollama.generate_stream(GenerationRequest::new(
                            model,
                            format!("Summarize this content in '{l}' in a few bullet points: \n```{}```", response),
                        )).await?;
                        let mut stdout = io::stdout();
                        while let Some(res) = stream.next().await {
                            let responses = res?;
                            for resp in responses {
                                stdout.write_all(resp.response.as_bytes()).await?;
                                stdout.flush().await?;
                            }
                        }
                        println!("\n");
                    }
                }
            }
            return Ok(());
        }
        println!("Finding Subtitles");

        let selected_lang = match inquire::Select::new("Lang", languages).prompt() {
            Ok(v) => v,
            Err(e) => match e {
                inquire::InquireError::OperationCanceled => Err(anyhow!(YtrsError::Quit))?,
                _ => Err(e)?,
            },
        };
        // Download English subtitles
        let subtitle_path = fetcher
            .download_subtitle(
                &video,
                selected_lang.clone(),
                format!("subtitle_{selected_lang}.srt"),
            )
            .await?;
        println!("Subtitle downloaded to: {:?}", subtitle_path);

        Ok(())
    }

    fn yt_prompt(opt_search: Option<String>) -> Result<String> {
        InquireText::new("Youtube Search:")
            .with_help_message("Press Escape to cancel | Ctrl+C to exit")
            .with_initial_value(&opt_search.unwrap_or_default())
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
            .context("Failed to read search input")
    }

    async fn query_ytmusic(opt_search: Option<String>) -> Result<(TrackItem, String)> {
        let search_term = Self::yt_prompt(opt_search)?;
        let rp = RustyPipe::new();
        let found_videos = rp
            .query()
            .unauthenticated()
            .music_search_tracks(search_term.clone())
            .await
            .context("Failed to search YouTube Music")?;
        Self::cleanup_rustypipe_cache();
        let mut found_videos_str: Vec<String> = found_videos
            .clone()
            .items
            .items
            .into_iter()
            .map(|track| TrackInfo::from(&track).colored())
            .collect();
        found_videos_str.push("Exit".red().to_string());
        let selected_vid_str = Select::new("Select Music", found_videos_str)
            .prompt()
            .context("Failed to select music")?;
        if selected_vid_str == "Exit".red().to_string().as_str() {
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
            .find(|track| TrackInfo::from(track).colored() == selected_vid_str)
        {
            Ok((vid, search_term))
        } else {
            bail!("Selected music not found. Please try again.");
        }
    }
    async fn query_ytvideo(opt_search: Option<String>) -> Result<(VideoItem, String)> {
        let search_term = Self::yt_prompt(opt_search)?;
        let found_videos = RustyPipe::new()
            .query()
            .unauthenticated()
            .search(search_term.clone())
            .await
            .context("Failed to search YouTube")?;
        Self::cleanup_rustypipe_cache();
        let mut videos: Vec<String> = found_videos
            .items
            .items
            .iter()
            .map(|v: &VideoItem| VideoInfo::from(v).colored())
            .collect();
        videos.push("Exit".red().to_string());

        let video_entry = Select::new("Select video to watch", videos)
            .with_help_message("Type to filter | Arrow keys to navigate | Enter to select")
            .prompt()
            .context("Failed to select video")?;
        if video_entry == "Exit".red().to_string().as_str() {
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
            .find(|v| VideoInfo::from(v).colored() == video_entry);
        if let Some(vid) = selected_vid {
            Ok((vid, search_term))
        } else {
            bail!("Selected video not found. Please try again.");
        }
    }
    pub fn check_mpv() -> Result<bool> {
        let output = std::process::Command::new("mpv")
            .args(["--version"])
            .output();
        match output {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Err(YtrsError::MpvNotFound.into()),
        }
    }
    fn ytdlp_exist() -> bool {
        Self::get_libs().youtube.exists()
    }
    fn ffmpeg_check() -> bool {
        Self::get_libs().ffmpeg.exists()
    }
    fn check_libraries(&mut self) -> bool {
        Self::ytdlp_exist() && Self::ffmpeg_check()
    }

    async fn install_lib() -> Result<()> {
        println!("Installing Libraries");
        let (exec_dir, output_dir) = Self::get_libs_path();
        let _ = Youtube::with_new_binaries(exec_dir, output_dir).await?;
        Ok(())
    }
    fn get_libs_path() -> (PathBuf, PathBuf) {
        let exec_dir = PathBuf::from("libs");
        let output_dir = PathBuf::from("output");
        (exec_dir, output_dir)
    }
    fn get_libs() -> Libraries {
        let (libs, _) = Self::get_libs_path();
        let youtube = libs.join("yt-dlp");
        let ffmpeg = libs.join("ffmpeg");
        Libraries::new(youtube, ffmpeg)
    }
    async fn get_fetcher() -> Result<Youtube> {
        let (_, out) = Self::get_libs_path();
        let libs = Self::get_libs();
        Youtube::new(libs, out)
            .await
            .context("Failed to retrieve Youtube Fetcher")
    }
}

impl AppAction {
    pub fn is_player(&self) -> bool {
        match self {
            AppAction::Player { .. } => true,
            _ => false,
        }
    }
}

impl From<&VideoItem> for VideoInfo {
    fn from(value: &VideoItem) -> Self {
        Self {
            channel: value.channel.clone().map(|i| i.name),
            name: value.name.clone(),
            _view_count: value.view_count,
            duration: value.duration,
        }
    }
}
impl VideoInfo {
    pub fn colored(&self) -> String {
        format!(
            "Video name: [{}]{}{}",
            self.name.to_string().green(),
            if let Some(d) = self.duration {
                format!(" {}", format_time(d))
            } else {
                "".to_string()
            },
            if let Some(chan) = &self.channel {
                format!("\n\tBy: {}", chan).blue()
            } else {
                "".to_string().blue()
            }
        )
    }
}
impl std::fmt::Display for VideoInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Video name: [{}]{}{}",
            self.name.to_string(),
            if let Some(d) = self.duration {
                format!(" {}", format_time(d))
            } else {
                "".to_string()
            },
            if let Some(chan) = &self.channel {
                format!("\n\tBy: {}", chan)
            } else {
                "".to_string()
            }
        )
    }
}
impl From<&TrackItem> for TrackInfo {
    fn from(value: &TrackItem) -> Self {
        Self {
            artists: value.artists.iter().map(|a| a.name.clone()).collect(),
            track_name: value.name.clone(),
            _id: value.id.clone(),
            duration: value.duration,
            view_count: value.view_count,
        }
    }
}
impl From<String> for AudioFormat {
    fn from(value: String) -> Self {
        Self::iter()
            .map(|v| (v.clone(), v.to_string()))
            .find(|(_, v_str)| v_str == &value)
            .iter()
            .next()
            .unwrap()
            .0
            .clone()
    }
}
impl From<String> for VideoFormat {
    fn from(value: String) -> Self {
        Self::iter()
            .map(|v| (v.clone(), v.to_string()))
            .find(|(_, v_str)| v_str == &value)
            .iter()
            .next()
            .unwrap()
            .0
            .clone()
    }
}
impl Default for Format {
    fn default() -> Self {
        Self::Audio {
            format: AudioFormat::MP3,
        }
    }
}
impl TrackInfo {
    pub fn colored(&self) -> String {
        format!(
            "Track name: '{}'{}{}\n\tArtist(s): [{}]",
            ratatui::crossterm::style::Stylize::green(self.track_name.clone()),
            match self.duration {
                Some(d) => {
                    format!(" {}", format_time(d))
                }
                None => {
                    "".to_string()
                }
            },
            match self.view_count {
                Some(views) =>
                    ratatui::crossterm::style::Stylize::dark_blue(format!(" Views: {}", views)),
                None => ratatui::crossterm::style::Stylize::dark_blue("".to_owned()),
            },
            ratatui::crossterm::style::Stylize::blue(self.artists.clone())
        )
    }
}
impl std::fmt::Display for TrackInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Track name: '{}'{}{}\n\tArtist(s): [{}]",
            self.track_name.clone(),
            match self.duration {
                Some(d) => {
                    format!(" {}", format_time(d))
                }
                None => {
                    "".to_string()
                }
            },
            match self.view_count {
                Some(views) => format!(" Views: {}", views),
                None => "".to_owned(),
            },
            self.artists.clone()
        )
    }
}
impl std::fmt::Debug for AppAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Download { .. } => f.debug_struct("Download").finish(),
            Self::Transcript => write!(f, "Transcript"),
            Self::Player { .. } => f.debug_struct("Player").finish(),
            Self::Quit => write!(f, "Quit"),
        }
    }
}
impl From<FormatInquire> for Format {
    fn from(value: FormatInquire) -> Self {
        match value {
            FormatInquire::Audio => Self::Audio {
                format: Default::default(),
            },
            FormatInquire::Video => Self::Video {
                format: Default::default(),
            },
        }
    }
}
impl From<&VideoItem> for YoutubeResponse {
    fn from(value: &VideoItem) -> Self {
        Self::Video(value.clone())
    }
}
impl From<TrackItem> for YoutubeResponse {
    fn from(value: TrackItem) -> Self {
        Self::Track(value)
    }
}
