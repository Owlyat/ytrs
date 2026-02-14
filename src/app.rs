use crate::cli::{AppActionCli, Cli};
use crate::mpv::{MpvIpc, MpvSpawnOptions};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{Timelike, Utc};
use image::DynamicImage;
use inquire::{Confirm, Select, Text as InquireText, validator::Validation};
use inquire_derive::Selectable;
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::Picture;
use lofty::probe::Probe;
use lofty::tag::{Accessor, Tag, TagExt};
use midir::{MidiInput, MidiInputPort, MidiOutput, MidiOutputConnection, MidiOutputPort};
use ollama_rs::Ollama;
use ollama_rs::generation::completion::request::GenerationRequest;
use ratatui::crossterm::event::KeyModifiers;
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
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::time::Duration;
use strum::IntoEnumIterator;
use thiserror::Error;
use yt_dlp::Youtube;
use yt_dlp::client::Libraries;
use yt_dlp::model::VideoCodecPreference;
use yt_dlp::model::caption::Subtitle;

use crate::utility::format_time;

#[derive(Default)]
pub struct YoutubeRs {
    pub api: Option<YoutubeAPI>,
    pub action: AppAction,
    pub mpv_installed: bool,
    pub last_search: Option<String>,
    pub summarize: Option<bool>,
    // Enter the player tui directly
    pub player: bool,
    pub run_midi: bool,
    args: Cli,
}
#[derive(Default)]
pub struct YoutubeRsBuilder {
    api: Option<YoutubeAPI>,
    action: Option<AppAction>,
    last_search: Option<String>,
    summarize: Option<bool>,
    #[allow(dead_code)]
    cli: Cli,
    // Enter the player tui directly
    pub player: Option<bool>,
    midi: bool,
}

impl YoutubeRs {
    pub fn builder() -> YoutubeRsBuilder {
        YoutubeRsBuilder::default()
    }
}

#[derive(strum::Display, strum::EnumIter, Clone, Copy, Default)]
pub enum AppAction {
    Download {
        format: Format,
    },
    Transcript,
    Player {
        format: Format,
    },
    #[default]
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

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, strum::Display, strum::EnumIter, Default, PartialEq, Copy, Debug, Selectable)]
pub enum AudioFormat {
    #[default]
    MP3,
    WAV,
}

#[allow(clippy::upper_case_acronyms)]
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

impl YoutubeRsBuilder {
    pub fn build(&mut self, cli: Cli) -> YoutubeRs {
        YoutubeRs {
            api: self.api,
            action: self.action.unwrap_or_default(),
            mpv_installed: YoutubeRs::check_mpv().unwrap_or_default(),
            last_search: Some(self.last_search.clone().unwrap_or_default()),
            args: cli,
            summarize: self.summarize,
            player: self.player.unwrap_or_default(),
            run_midi: self.midi,
        }
    }
    pub fn api(&mut self, music: Option<bool>, prompt: bool) -> &mut Self {
        if let Some(is_music) = music {
            if is_music {
                self.api = Some(YoutubeAPI::Music)
            } else {
                self.api = Some(YoutubeAPI::Video)
            }
        } else if prompt {
            self.api = Some(YoutubeAPI::select("Select API").prompt().unwrap());
        }

        self
    }
    pub fn midi(&mut self, run_midi: bool) -> &mut Self {
        self.midi = run_midi;
        self
    }
    pub fn action(&mut self, action: Option<AppAction>, cli: Option<AppActionCli>) -> &mut Self {
        if let Some(action) = cli {
            self.action = Some(match action {
                AppActionCli::Download { .. } => AppAction::Download {
                    format: Default::default(),
                },
                AppActionCli::Player { .. } => AppAction::Player {
                    format: Default::default(),
                },
                AppActionCli::Transcript { .. } => AppAction::Transcript,
            });
        } else if let Some(action) = action {
            self.action = Some(action);
        }
        self
    }
    pub fn transcript(&mut self) -> &mut Self {
        self.action = Some(AppAction::Transcript);
        self.api = Some(YoutubeAPI::Video);
        self
    }
    pub fn prompt_download(&mut self) -> &mut Self {
        self.action = Some(AppAction::Download {
            format: FormatInquire::select("Select Format")
                .prompt()
                .unwrap()
                .into(),
        });
        self
    }
    pub fn prompt_format(&mut self) -> &mut Self {
        if let Some(AppAction::Download { format }) = &mut self.action {
            match format {
                Format::Audio { format } => {
                    *format = AudioFormat::select("Select Audio Format").prompt().unwrap()
                }
                Format::Video { format } => {
                    *format = VideoFormat::select("Select Video Format").prompt().unwrap()
                }
            }
        }
        self
    }
    pub fn player(&mut self) -> &mut Self {
        self.action = Some(AppAction::Player {
            format: Default::default(),
        });
        self
    }
    pub fn prompt_player(&mut self) -> &mut Self {
        self.action = Some(AppAction::Player {
            format: FormatInquire::select("Format").prompt().unwrap().into(),
        });
        self.api = Some(YoutubeAPI::select("Select API").prompt().unwrap());
        self
    }
    pub fn audio_player(&mut self) -> &mut Self {
        self.action = Some(AppAction::Player {
            format: Format::Audio {
                format: AudioFormat::MP3,
            },
        });
        self.player = Some(true);
        self.api = Some(YoutubeAPI::Music);
        self
    }
    pub fn file(&mut self, p: PathBuf) -> &mut Self {
        if let Some(ext) = p.extension() {
            if let Some(i) = AudioFormat::iter()
                .find(|af| af.to_string().to_lowercase() == ext.to_string_lossy().to_lowercase())
                .iter()
                .next()
            {
                if let Some(AppAction::Player { format }) = &mut self.action {
                    *format = Format::Audio { format: *i };
                }
            } else if let Some(i) = VideoFormat::iter()
                .find(|vf| vf.to_string().to_lowercase() == ext.to_string_lossy().to_lowercase())
                && let Some(AppAction::Player { format }) = &mut self.action
            {
                *format = Format::Video { format: i }
            }
        }
        self.last_search = Some(p.to_string_lossy().to_string());
        self
    }
    pub fn url(&mut self, url: impl Into<String>) -> &mut Self {
        let url: String = url.into();
        if url.to_lowercase().starts_with("https://music.youtube.com") {
            self.api = Some(YoutubeAPI::Music);
        } else if url.to_lowercase().starts_with("https://www.youtube.com") {
            self.api = Some(YoutubeAPI::Video);
        } else {
            self.api = Some(YoutubeAPI::select("Select API").prompt().unwrap());
        }
        self.last_search = Some(url);
        self
    }
    pub fn query(&mut self, query: impl Into<String>) -> &mut Self {
        self.last_search = Some(query.into());
        self
    }
    pub fn do_summarize(&mut self, summarize: bool) -> &mut Self {
        self.summarize = Some(summarize);
        self
    }
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
        match self.action {
            AppAction::Download { format } => {
                if !self.libraries_exist(&self.args.clone()) {
                    Self::install_lib(&self.args).await?;
                }
                let (video_id, video_name) = match self.api {
                    Some(YoutubeAPI::Music) => {
                        let (track, search) = Self::query_ytmusic(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        (track.id.clone(), track.name.clone())
                    }
                    Some(YoutubeAPI::Video) => {
                        let (video, search) = Self::query_ytvideo(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        (video.id.clone(), video.name.clone())
                    }
                    None => return Ok(()),
                };
                let url = format!("https://www.youtube.com/watch?v={video_id}");
                match format {
                    Format::Audio { format } => {
                        self.download_audio(&url, &video_name, format, &self.args)
                            .await?;
                    }
                    Format::Video { format } => {
                        self.download_video(&url, &video_name, format, &self.args)
                            .await?;
                    }
                }
            }
            AppAction::Transcript => {
                if !self.libraries_exist(&self.args.clone()) {
                    Self::install_lib(&self.args).await?;
                }
                let video_id = match self.api {
                    Some(YoutubeAPI::Music) => {
                        let (track, search) = Self::query_ytmusic(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        track.id.clone()
                    }
                    Some(YoutubeAPI::Video) => {
                        let (video, search) = Self::query_ytvideo(self.last_search.clone()).await?;
                        self.last_search = Some(search);
                        video.id.clone()
                    }
                    None => unreachable!(),
                };
                self.download_transcript(&video_id, &self.args).await?;
            }
            AppAction::Player { format } => {
                if !self.mpv_installed {
                    self.mpv_installed = Self::check_mpv()?;
                }
                let mut response = match self.api {
                    Some(YoutubeAPI::Music) => {
                        if self.player {
                            None
                        } else {
                            let res = Self::query_ytmusic(self.last_search.clone()).await?;
                            self.last_search = Some(res.1);
                            Some(YoutubeResponse::Track(res.0))
                        }
                    }
                    Some(YoutubeAPI::Video) => {
                        let res = Self::query_ytvideo(self.last_search.clone()).await?;
                        self.last_search = Some(res.1);
                        Some(YoutubeResponse::Video(res.0))
                    }
                    None => None,
                };
                if response.is_none() {
                    self.player(
                        &mut None,
                        &mut None,
                        match format {
                            Format::Audio { .. } => true,
                            Format::Video { .. } => false,
                        },
                        self.run_midi,
                    )
                    .await;
                    return Ok(());
                }
                match format {
                    Format::Audio { .. } => {
                        let mut opt_thumbnail = if let Some(res) = &response {
                            Self::fetch_yt_thumbnail(&res.get_id(), &self.args)
                                .await
                                .ok()
                        } else {
                            None
                        };
                        self.player(&mut response, &mut opt_thumbnail, true, self.run_midi)
                            .await;
                    }
                    Format::Video { .. } => {
                        let mut opt_thumbnail = if let Some(res) = &response {
                            Self::fetch_yt_thumbnail(&res.get_id(), &self.args)
                                .await
                                .ok()
                        } else {
                            None
                        };
                        self.player(&mut response, &mut opt_thumbnail, false, self.run_midi)
                            .await;
                    }
                }
            }
            AppAction::Quit => return Err(YtrsError::Quit.into()),
        }
        Ok(())
    }
    async fn player(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        opt_thumbnail: &mut Option<DynamicImage>,
        audio_only: bool,
        run_midi: bool,
    ) {
        let mut midi_in = MidiInput::new("midir reading input").expect("Could not open Midi Input");
        midi_in.ignore(midir::Ignore::None);
        let midi_out =
            MidiOutput::new("midir forwarding output").expect("Could not open Midi Output");
        let in_port = midi_in.ports();
        let out_port = midi_out.ports();
        let opt_midi_in_port: Option<&MidiInputPort> = if !run_midi {
            None
        } else {
            match in_port.len() {
                0 => None,
                1 => Some(&in_port[0]),
                _ => {
                    let filter = |(i, p): (usize, &MidiInputPort)| -> String {
                        format!("{i}:{}", midi_in.port_name(p).unwrap())
                    };
                    let mut inputs = vec![String::from("None")];
                    in_port
                        .iter()
                        .enumerate()
                        .for_each(|(i, p)| inputs.push(filter((i, p))));
                    let res = inquire::Select::new("Select Midi Input Port", inputs)
                        .prompt()
                        .unwrap();
                    let port = in_port
                        .iter()
                        .enumerate()
                        .find(|(i, p)| filter((*i, p)) == res);
                    match port {
                        Some(p) => Some(&in_port[p.0]),
                        None => None,
                    }
                }
            }
        };
        let opt_midi_out_port: Option<&MidiOutputPort> = if !run_midi {
            None
        } else {
            match out_port.len() {
                0 => None,
                1 => Some(&out_port[0]),
                _ => {
                    let filter = |(i, p): (usize, &MidiOutputPort)| -> String {
                        format!("{i}:{}", midi_out.port_name(p).unwrap())
                    };
                    let mut inputs = vec![String::from("None")];
                    out_port
                        .iter()
                        .enumerate()
                        .for_each(|(i, p)| inputs.push(filter((i, p))));
                    let res = inquire::Select::new("Select Midi Output Port", inputs)
                        .prompt()
                        .unwrap();
                    let port = out_port
                        .iter()
                        .enumerate()
                        .find(|(i, p)| filter((*i, p)) == res);
                    match port {
                        Some(p) => Some(&out_port[p.0]),
                        None => None,
                    }
                }
            }
        };
        let mut img = if let Some(dyn_thumbnail) = &opt_thumbnail
            && let Ok(picker) = picker::Picker::from_query_stdio()
        {
            let protocol = picker.new_resize_protocol(dyn_thumbnail.clone());
            Some(protocol)
        } else {
            None
        };
        let mut empty_player = false;
        let mut audio_file_error = None;
        let mut file: Option<(TaggedFile, String)> = {
            if let Some(s) = &self.last_search
                && !s.is_empty()
            {
                let f = PathBuf::from(s);
                if f.exists() && f.is_file() {
                    use lofty::probe::Probe;
                    if let Ok(file) = Probe::open(&f) {
                        match file.guess_file_type() {
                            Ok(file) => match file.read() {
                                Ok(tagged_file) => {
                                    if let Some(tag) = tagged_file.primary_tag()
                                        && let Some(pic) = tag.pictures().first()
                                        && let Ok(dyn_img) = image::load_from_memory(pic.data())
                                    {
                                        img = if let Ok(picker) = picker::Picker::from_query_stdio()
                                        {
                                            let protocole =
                                                picker.new_resize_protocol(dyn_img.clone());
                                            Some(protocole)
                                        } else {
                                            None
                                        };
                                    }
                                    Some((tagged_file, f.to_string_lossy().to_string()))
                                }
                                Err(e) => {
                                    audio_file_error = Some(format!("Could not read file {e}"));
                                    None
                                }
                            },
                            Err(e) => {
                                audio_file_error = Some(format!("Could not guess file type: {e}"));
                                None
                            }
                        }
                    } else {
                        audio_file_error = Some("Could not open file".to_string());
                        None
                    }
                } else {
                    audio_file_error =
                        Some(format!("File '{}' does not exist", f.to_string_lossy()));
                    None
                }
            } else {
                empty_player = true;
                None
            }
        };
        let opts = MpvSpawnOptions::default();
        let mut mpv = MpvIpc::spawn(&opts, audio_only)
            .await
            .context("Failed to spawn mpv process")
            .expect("Could not spawn MPV");
        let mpv_vol = mpv.observe_prop::<f64>("volume", 1.0).await;
        if let Some(res) = response {
            mpv.send_command(json!(["loadfile", Self::get_video_url(&res.get_id())]))
                .await
                .context("Failed to load media")
                .expect("Could not send command to MPV");
        } else if let Some(file) = &file {
            mpv.send_command(json!(["loadfile", file.1]))
                .await
                .context("Failed to load media")
                .expect("Could not send command to MPV");
        } else if empty_player {
            // Pass
        } else {
            panic!(
                "Error : {}",
                audio_file_error.unwrap_or("No file found".to_string())
            );
        }
        let (midi_volume_tx, midi_volume_rx) = std::sync::mpsc::channel();
        let (midi_pause_tx, midi_pause_rx) = std::sync::mpsc::channel();
        let _conn_in = if let Some(in_port) = opt_midi_in_port {
            midi_in
                .connect(
                    in_port,
                    "midir-read-input",
                    move |_, message, midi_tx| {
                        if message[0] == 224 {
                            let volume_midi = u8_to_mpv_vol(message[2]);
                            let _ = midi_tx.0.send(volume_midi);
                        }
                        if message[1] == 93 || message[1] == 94 {
                            let _ = midi_tx.1.send(());
                        }
                    },
                    (midi_volume_tx, midi_pause_tx),
                )
                .ok()
        } else {
            None
        };
        let mut conn_out = if let Some(out_port) = opt_midi_out_port {
            midi_out.connect(out_port, "midir-forward").ok()
        } else {
            None
        };
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

        // TUI Main Loop
        loop {
            if let Some(v) = midi_volume_rx.try_iter().last() {
                // v is from 0 to 130
                mpv.send_command(json!(["set_property", "volume", v]))
                    .await
                    .unwrap();
            }
            if let Ok(()) = midi_pause_rx.try_recv() {
                pause_state = !pause_state;
                let _ = mpv.set_prop("pause", pause_state).await;
            }
            if !mpv.running().await {
                break;
            }
            if time_rx
                .has_changed()
                .expect("Error while checking if MPV time changed")
            {
                playback_time = *time_rx.borrow();
            }
            if playback_time == 0.0 && !vid_started {
                vid_started = true;
            }

            let _ = term.draw(|f| {
                self.draw(
                    response,
                    playback_time,
                    vid_started,
                    loader,
                    &mut loader_idx,
                    open_popup,
                    &videos_list,
                    &mut selected_list_item,
                    &popup_query,
                    &mut img,
                    f,
                    &mut file,
                    empty_player,
                    &mpv_vol.borrow(),
                );
            });
            let event_happened = ratatui::crossterm::event::poll(Duration::from_millis(50)).ok();
            if let Some(has_happened) = event_happened
                && has_happened
            {
                let event = read().unwrap();
                if open_popup {
                    self.handle_popup_event(
                        response,
                        &mut mpv,
                        &mut open_popup,
                        &mut videos_list,
                        &mut selected_list_item,
                        &mut popup_query,
                        &mut img,
                        &event,
                    )
                    .await;
                } else if let ControlFlow::Break(_) = self
                    .handle_playback_event(
                        response,
                        &mut mpv,
                        &mut pause_state,
                        &mut open_popup,
                        event,
                        empty_player,
                        &mut conn_out,
                        &mpv_vol.borrow(),
                    )
                    .await
                {
                    break;
                }
            }
        }
        mpv.quit().await;
        ratatui::restore();
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_popup_event(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        mpv: &mut MpvIpc,
        open_popup: &mut bool,
        videos_list: &mut Vec<(String, YoutubeResponse)>,
        selected_list_item: &mut ListState,
        popup_query: &mut String,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        event: &ratatui::crossterm::event::Event,
    ) {
        if event.is_key_press()
            && let KeyCode::Char(ch) = event.as_key_event().unwrap().code
        {
            popup_query.push(ch);
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Backspace {
            if event.as_key_event().unwrap().modifiers == KeyModifiers::CONTROL {
                popup_query.clear();
            } else {
                popup_query.pop();
            }
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Tab {
            self.api = match self.api {
                Some(YoutubeAPI::Music) => Some(YoutubeAPI::Video),
                Some(YoutubeAPI::Video) => Some(YoutubeAPI::Music),
                None => None,
            };
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Up {
            selected_list_item.select_previous();
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Down {
            selected_list_item.select_next();
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Esc {
            *open_popup = !*open_popup;
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Enter {
            if let Some(selected) = selected_list_item.selected()
                && popup_query.is_empty()
            {
                if let Some(vid) = videos_list.get(selected).map(|v| v.1.clone()) {
                    popup_query.clear();
                    mpv.send_command(json!(["loadfile", Self::get_video_url(&vid.get_id())]))
                        .await
                        .context("Failed to load media")
                        .expect("Could not send command to MPV");
                    if let Ok(thumbnail) = Self::fetch_yt_thumbnail(&vid.get_id(), &self.args).await
                    {
                        *img = if let Ok(picker) = picker::Picker::from_query_stdio() {
                            let protocol = picker.new_resize_protocol(thumbnail.clone());
                            Some(protocol)
                        } else {
                            None
                        };
                    } else {
                        *img = None;
                    }
                    *response = Some(vid);
                    videos_list.clear();
                }
            } else if !popup_query.is_empty() {
                match self.api {
                    Some(YoutubeAPI::Music) => {
                        let rp = RustyPipe::new();
                        let found_videos = rp
                            .query()
                            .unauthenticated()
                            .music_search_tracks(popup_query.clone())
                            .await
                            .context("Failed to search YouTube Music")
                            .expect("Failed to fetch youtube with rustypipe");
                        YoutubeRs::cleanup_rustypipe_cache();
                        *videos_list = found_videos
                            .clone()
                            .items
                            .items
                            .into_iter()
                            .map(|track| (TrackInfo::from(&track).to_string(), track.into()))
                            .collect();
                        popup_query.clear();
                    }
                    Some(YoutubeAPI::Video) => {
                        let found_videos = RustyPipe::new()
                            .query()
                            .unauthenticated()
                            .search(popup_query.clone())
                            .await
                            .context("Failed to search YouTube")
                            .unwrap();
                        YoutubeRs::cleanup_rustypipe_cache();
                        *videos_list = found_videos
                            .items
                            .items
                            .iter()
                            .map(|v| (VideoInfo::from(v).to_string(), v.into()))
                            .collect();
                        popup_query.clear();
                    }
                    None => {}
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        playback_time: f64,
        vid_started: bool,
        loader: [&str; 4],
        loader_idx: &mut usize,
        open_popup: bool,
        videos_list: &[(String, YoutubeResponse)],
        selected_list_item: &mut ListState,
        popup_query: &String,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        f: &mut Frame<'_>,
        file: &mut Option<(TaggedFile, String)>,
        empty_player: bool,
        mpv_vol: &f64,
    ) {
        if vid_started {
            // General Layout
            let layout = Layout::vertical(Constraint::from_percentages([60, 40])).split(f.area());
            // Top Image
            if let Some(protocol) = img {
                let img_layout = layout[0];
                // remove 50% width on both sides
                let img_layout = img_layout.centered_horizontally(Constraint::Percentage(50));
                // Size of the image once resized to the area to fit
                let img_size = protocol.size_for(ratatui_image::Resize::Scale(None), img_layout);
                let width_dif = img_layout.width - img_size.width;
                let height_dif = img_layout.height - img_size.height;
                let img_place = Rect::new(
                    img_layout.x + width_dif / 2,
                    img_layout.y + height_dif / 2,
                    img_layout.width,
                    img_layout.height,
                );
                f.render_stateful_widget(
                    StatefulImage::default().resize(ratatui_image::Resize::Scale(None)),
                    img_place,
                    protocol,
                );
                if let Some(x) = protocol.last_encoding_result()
                    && let Err(e) = x
                {
                    panic!("Error with last encoding result for image '{e}'");
                }
            }

            // Bottom Panel
            let info_layout = layout[1];
            let info_layout = info_layout.centered_horizontally(Constraint::Percentage(50));
            if open_popup {
                self.render_yt_search_popup(
                    videos_list,
                    selected_list_item,
                    popup_query,
                    f,
                    info_layout,
                );
            } else {
                self.render_yt_player(
                    response,
                    playback_time,
                    f,
                    info_layout,
                    file,
                    empty_player,
                    mpv_vol,
                );
            }
        } else {
            // Vid not started
            if Utc::now().second().is_multiple_of(2) {
                *loader_idx = loader_idx.saturating_add(1) % loader.len();
            }
            Block::bordered()
                .title(format!("[Loading MPV {}]", loader[*loader_idx]))
                .render(f.area(), f.buffer_mut());
        }
    }

    fn render_yt_search_popup(
        &mut self,
        videos_list: &[(String, YoutubeResponse)],
        selected_list_item: &mut ListState,
        popup_query: &String,
        f: &mut Frame<'_>,
        info_layout: Rect,
    ) {
        // Popup for yt search
        let areas =
            Layout::vertical([Constraint::Length(3), Constraint::Fill(3)]).split(info_layout);
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
                    format!("[▼▲ Select Entry | (Esc) Player | (Enter) Search/Play Entry | Tab Change Api: {}]",self.api.unwrap_or_default()),
                )
                .style(Style::default().yellow().on_blue()),
        )
        .highlight_symbol(">")
        .highlight_style(Style::default().red().on_cyan())
        .direction(ratatui::widgets::ListDirection::TopToBottom);
        f.render_stateful_widget(list, areas[1], selected_list_item);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_yt_player(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        playback_time: f64,
        f: &mut Frame<'_>,
        info_layout: Rect,
        file: &mut Option<(TaggedFile, String)>,
        empty_player: bool,
        mpv_vol: &f64,
    ) {
        // Playback Info When Audio is from Youtube
        if let Some(res) = response {
            Block::bordered()
                .style(Style::default().on_blue().yellow())
                .title_top(format!(
                    "{} - {}:{}",
                    res.get_name(),
                    format_time(playback_time as u32),
                    format_time(res.get_duration()),
                ))
                .title_alignment(HorizontalAlignment::Center)
                .title_top(format!("[Vol:{mpv_vol}]"))
                .title_alignment(HorizontalAlignment::Right)
                .title_bottom("['q' Quit | ▲▼ Volume(+/-) | ◀▶ Seek | 'y' Yank URL |'o' YtSearch]")
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
                .ratio(playback_time / res.get_duration() as f64)
                .render(gauge_layout, f.buffer_mut());
        } else if let Some(file) = file {
            Block::bordered()
                .style(Style::default().yellow().on_blue())
                .title_top(format!(
                    "{} - {}:{}",
                    PathBuf::from(&file.1)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    format_time(playback_time as u32),
                    format_time(file.0.properties().duration().as_secs() as u32),
                ))
                .title_alignment(HorizontalAlignment::Center)
                .title_bottom("['q' Quit | ▲▼ Volume(+/-) | ◀▶ Seek]")
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
                .ratio(playback_time / file.0.properties().duration().as_secs_f64())
                .render(gauge_layout, f.buffer_mut());
        } else if empty_player {
            Block::bordered()
                .style(Style::default().on_blue().yellow())
                .title_alignment(HorizontalAlignment::Center)
                .title_bottom("['q' Quit | ▲▼ Volume(+/-) | ◀▶ Seek | 'y' Yank URL |'o' YtSearch]")
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
                .ratio(playback_time / 1.0)
                .render(gauge_layout, f.buffer_mut());
        }
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

    async fn fetch_yt_thumbnail(video_id: &str, args: &Cli) -> Result<DynamicImage> {
        let thumbnail_url = if Self::ytdlp_exist(args) {
            Self::get_fetcher(args)
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

    async fn download_audio(
        &self,
        url: &str,
        video_name: &str,
        format: AudioFormat,
        args: &Cli,
    ) -> Result<()> {
        println!("Downloading Audio ...");
        let fetcher = Self::get_fetcher(args).await?;
        let safe_name =
            video_name.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "_");
        let vid_info = fetcher.fetch_video_infos(url.to_string()).await?;
        let downloaded = fetcher
            .download_audio_stream_with_quality(
                url.to_string(),
                format!("{safe_name}.{}", format.to_string().to_lowercase()),
                yt_dlp::model::AudioQuality::Best,
                yt_dlp::model::AudioCodecPreference::Custom(format.to_string()),
            )
            .await?;
        println!("Audio downloaded at '{downloaded:?}'");
        let tagged_file = Probe::open(&downloaded)?;
        let file_type = tagged_file.guess_file_type()?;
        let mut tagged_file = file_type.read()?;
        let tag = match tagged_file.primary_tag_mut() {
            Some(tag) => tag,
            None => {
                if let Some(first_tag) = tagged_file.first_tag_mut() {
                    first_tag
                } else {
                    let tag_type = tagged_file.primary_tag_type();
                    tagged_file.insert_tag(Tag::new(tag_type));
                    tagged_file.primary_tag_mut().unwrap()
                }
            }
        };
        tag.set_title(vid_info.title);
        tag.set_artist(vid_info.channel);
        tag.set_genre(vid_info.tags.iter().cloned().collect());
        let thumbnail = reqwest::Client::new()
            .get(vid_info.thumbnail)
            .send()
            .await?
            .bytes()
            .await?;
        tag.push_picture(
            Picture::unchecked(thumbnail.to_vec())
                .mime_type(lofty::picture::MimeType::Jpeg)
                .pic_type(lofty::picture::PictureType::CoverFront)
                .build(),
        );
        tag.save_to_path(downloaded, WriteOptions::default())?;

        Ok(())
    }

    async fn download_video(
        &self,
        url: &str,
        video_name: &str,
        format: VideoFormat,
        args: &Cli,
    ) -> Result<()> {
        println!("Downloading Video ...");
        let fetcher = Self::get_fetcher(args).await?;
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

    async fn download_transcript(&self, video_id: &str, args: &Cli) -> Result<()> {
        let fetcher = Self::get_fetcher(args).await?;

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
                    let (_, out) = Self::get_libs_path(&self.args);
                    let mut f = OpenOptions::new().write(true).create(true).open(format!(
                        "{}/subtitle_{l}.{}",
                        out.to_string_lossy(),
                        res_to_dl.file_extension()
                    ))?;
                    f.write_all(response.as_bytes())?;
                    println!(
                        "AutoGenerated Captions downloaded at '{}/subtitle_{l}.{}'",
                        out.to_string_lossy(),
                        res_to_dl.file_extension()
                    );
                    let res = if let Some(b) = self.summarize {
                        println!("Summarize : {b}");
                        b
                    } else {
                        match inquire::Confirm::new("Summarize with ai ?")
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
                        }
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
        let search_term = Self::yt_prompt(opt_search.clone())?;
        let found_videos: rustypipe::model::SearchResult<VideoItem> = RustyPipe::new()
            .query()
            .unauthenticated()
            .search(search_term.clone())
            .await
            .context("Failed to search YouTube")?;
        Self::cleanup_rustypipe_cache();
        if found_videos.items.items.len() == 1
            && let Some(item) = found_videos.items.items.first()
        {
            return Ok((item.clone(), opt_search.clone().unwrap_or_default()));
        }
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
    fn ytdlp_exist(args: &Cli) -> bool {
        if cfg!(target_os = "windows") {
            PathBuf::from(format!(
                "{}.exe",
                Self::get_libs(args).youtube.to_string_lossy()
            ))
            .exists()
        } else {
            Self::get_libs(args).youtube.exists()
        }
    }
    fn ffmpeg_check(args: &Cli) -> bool {
        if cfg!(target_os = "windows") {
            PathBuf::from(format!(
                "{}.exe",
                Self::get_libs(args).ffmpeg.to_string_lossy()
            ))
            .exists()
        } else {
            Self::get_libs(args).ffmpeg.exists()
        }
    }
    fn libraries_exist(&mut self, args: &Cli) -> bool {
        if !Self::ytdlp_exist(args) {
            println!(
                "YT-DLP not found at '{}'",
                Self::get_libs(args).youtube.to_string_lossy()
            );
        }
        if !Self::ffmpeg_check(args) {
            println!(
                "FFMPEG not found at '{}'",
                Self::get_libs(args).ffmpeg.to_string_lossy()
            );
        }
        Self::ytdlp_exist(args) && Self::ffmpeg_check(args)
    }

    async fn install_lib(args: &Cli) -> Result<()> {
        println!("Installing Libraries");
        let (exec_dir, output_dir) = Self::get_libs_path(args);
        let _ = Youtube::with_new_binaries(exec_dir, output_dir).await?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    fn get_libs_path(args: &Cli) -> (PathBuf, PathBuf) {
        let exec_dir = if let Some(libs_path) = &args.libs_path {
            libs_path.join("libs")
        } else {
            if cfg!(target_os = "windows") {
                PathBuf::from(env!("USERPROFILE"))
                    .join(".config")
                    .join("ytrs")
                    .join("libs")
            } else if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
                if let Ok(home_path_str) = std::env::var("HOME") {
                    PathBuf::from(home_path_str)
                        .join(".config")
                        .join("ytrs")
                        .join("libs")
                } else {
                    PathBuf::from("libs")
                }
            } else {
                PathBuf::from("libs")
            }
        };
        let output_dir = if let Some(output) = &args.output_path {
            output.join("output")
        } else {
            if cfg!(target_os = "windows") {
                PathBuf::from(env!("USERPROFILE"))
                    .join(".config")
                    .join("ytrs")
                    .join("output")
            } else if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
                if let Ok(home_path_str) = std::env::var("Home") {
                    PathBuf::from(home_path_str)
                        .join(".config")
                        .join("ytrs")
                        .join("output")
                } else {
                    PathBuf::from("output")
                }
            } else {
                PathBuf::from("output")
            }
        };
        (exec_dir, output_dir)
    }

    #[cfg(target_os = "linux")]
    fn get_libs_path(args: &Cli) -> (PathBuf, PathBuf) {
        let exec_dir = if let Some(libs_path) = &args.libs_path {
            libs_path.join("libs")
        } else if let Ok(home_path_str) = std::env::var("HOME") {
            PathBuf::from(home_path_str)
                .join(".config")
                .join("ytrs")
                .join("libs")
        } else {
            PathBuf::from("libs")
        };
        let output_dir = if let Some(output) = &args.output_path {
            output.join("output")
        } else if let Ok(home_path_str) = std::env::var("HOME") {
            PathBuf::from(home_path_str)
                .join(".config")
                .join("ytrs")
                .join("output")
        } else {
            PathBuf::from("output")
        };
        (exec_dir, output_dir)
    }
    #[cfg(target_os = "macos")]
    fn get_libs_path(args: &Cli) -> (PathBuf, PathBuf) {
        let exec_dir = if let Some(libs_path) = &args.libs_path {
            libs_path.join("libs")
        } else if let Ok(home_path_str) = std::env::var("HOME") {
            PathBuf::from(home_path_str)
                .join(".config")
                .join("ytrs")
                .join("libs")
        } else {
            PathBuf::from("libs")
        };
        let output_dir = if let Some(output) = &args.output_path {
            output.join("output")
        } else if let Ok(home_path_str) = std::env::var("HOME") {
            PathBuf::from(home_path_str)
                .join(".config")
                .join("ytrs")
                .join("output")
        } else {
            PathBuf::from("output")
        };
        (exec_dir, output_dir)
    }
    fn get_libs(args: &Cli) -> Libraries {
        let (libs, _) = Self::get_libs_path(args);
        let youtube = libs.join("yt-dlp");
        let ffmpeg = libs.join("ffmpeg");
        Libraries::new(youtube, ffmpeg)
    }
    async fn get_fetcher(args: &Cli) -> Result<Youtube> {
        let (_, out) = Self::get_libs_path(args);
        let libs = Self::get_libs(args);
        Youtube::new(libs, out)
            .await
            .context("Failed to retrieve Youtube Fetcher")
    }
    #[allow(clippy::too_many_arguments)]
    async fn handle_playback_event(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        mpv: &mut MpvIpc,
        pause_state: &mut bool,
        open_popup: &mut bool,
        event: ratatui::crossterm::event::Event,
        empty_player: bool,
        conn_out: &mut Option<MidiOutputConnection>,
        mpv_vol: &f64,
    ) -> ControlFlow<()> {
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Char('q') {
            return ControlFlow::Break(());
        }
        if event.is_key_press()
            && event.as_key_event().unwrap().code == KeyCode::Char('y')
            && let Some(res) = response
        {
            let current_url = Self::get_video_url(&res.get_id());
            let _ = Self::clipboard(&current_url);
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Char(' ') {
            *pause_state = !*pause_state;
            let _ = mpv.set_prop("pause", pause_state).await;
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Right {
            let _ = mpv.send_command(json!(["seek", "5", "relative"])).await;
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Left {
            let _ = mpv.send_command(json!(["seek", "-5", "relative"])).await;
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Up {
            let _ = mpv.send_command(json!(["add", "volume", "5"])).await;
            if let Some(out_midi_connection) = conn_out {
                let _ = out_midi_connection.send(&[224, 0, u32_to_midi(*mpv_vol as u32)]);
            }
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Down {
            let _ = mpv.send_command(json!(["add", "volume", "-5"])).await;
            if let Some(out_midi_connection) = conn_out {
                let _ = out_midi_connection.send(&[224, 0, u32_to_midi(*mpv_vol as u32)]);
            }
        }
        if (response.is_some() | empty_player)
            && event.is_key_press()
            && event.as_key_event().unwrap().code == KeyCode::Char('o')
        {
            *open_popup = !*open_popup;
        }
        ControlFlow::Continue(())
    }
}

fn u32_to_midi(val: u32) -> u8 {
    ((val * 127) / 130) as u8
}

fn u8_to_mpv_vol(val: u8) -> u32 {
    ((val as u32 * 130) / 127).clamp(0, 130)
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
            self.name,
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
            .map(|v| (v, v.to_string()))
            .find(|(_, v_str)| v_str == &value)
            .iter()
            .next()
            .unwrap()
            .0
    }
}
impl From<String> for VideoFormat {
    fn from(value: String) -> Self {
        Self::iter()
            .map(|v| (v, v.to_string()))
            .find(|(_, v_str)| v_str == &value)
            .iter()
            .next()
            .unwrap()
            .0
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
