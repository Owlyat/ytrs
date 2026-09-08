mod midi;
use crate::cli::{Cli, ImgProtocol};
use crate::mpv::{MpvIpc, MpvSpawnOptions};
use anyhow::{Context, Result, anyhow, bail};
use image::DynamicImage;
use lofty::config::WriteOptions;
use lofty::file::{TaggedFile, TaggedFileExt};
use lofty::picture::Picture;
use lofty::probe::Probe;
use lofty::tag::{Accessor, Tag, TagExt};
use midir::{
    MidiInput, MidiInputConnection, MidiInputPort, MidiOutput, MidiOutputConnection, MidiOutputPort,
};
use ollama_rs::Ollama;
use ollama_rs::generation::completion::request::GenerationRequest;
use ratatui::crossterm::event::KeyModifiers;
use ratatui::style::Stylize;
use ratatui::widgets::ListState;
use ratatui::crossterm::event::{KeyCode, read};
use ratatui_image::picker;
use rustypipe::{
    client::RustyPipe,
    model::{TrackItem, VideoItem, paginator::Paginator},
};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;
use strum::IntoEnumIterator;
use thiserror::Error;
use tracing::{debug, error, info, warn};
use yt_dlp::client::Libraries;
use yt_dlp::model::caption::Subtitle;
use yt_dlp::model::{Video, VideoCodecPreference};
use yt_dlp::{Downloader, install_libraries};

use crate::config::Theme;
use crate::playlist::{Playlist, PlaylistItem};
use crate::sidebar::Sidebar;
use crate::ui;
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
    pub embed: bool,
    pub vo: Option<String>,
    pub audio_device: Option<String>,
    pub no_art: bool,
    pub img_protocol: ImgProtocol,
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
    embed: bool,
    vo: Option<String>,
    audio_device: Option<String>,
    no_art: bool,
    img_protocol: ImgProtocol,
}

impl YoutubeRs {
    pub fn builder() -> YoutubeRsBuilder {
        YoutubeRsBuilder::default()
    }
}

/// Detect a music source URL. `None` when the domain is unknown.
pub fn url_is_music(url: &str) -> Option<bool> {
    let lower = url.to_lowercase();
    if lower.starts_with("https://music.youtube.com") {
        Some(true)
    } else if lower.starts_with("https://www.youtube.com")
        || lower.starts_with("https://youtu.be")
    {
        Some(false)
    } else {
        None
    }
}

/// Extract a YouTube video id from watch / youtu.be / shorts / music URLs.
pub fn extract_video_id(url: &str) -> Option<String> {
    // watch?v=ID Parsons
    if let Some(v_pos) = url.find("v=") {
        let id: String = url[v_pos + 2..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if id.len() == 11 {
            return Some(id);
        }
    }
    // youtu.be/ID, /shorts/ID, /embed/ID, /live/ID
    let last = url.split(['?', '#']).next()?.rsplit('/').next()?;
    if last.len() == 11 && last.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Some(last.to_string());
    }
    None
}

#[derive(strum::Display, strum::EnumIter, Clone, Copy, Default, Debug)]
pub enum AppAction {
    Download {
        format: Format,
    },
    Transcript,
    Player {
        format: Format,
    },
    Update,
    #[default]
    Quit,
}

#[derive(strum::Display, strum::EnumIter, Default, Clone, Debug, Copy)]
pub enum YoutubeAPI {
    Music,
    #[default]
    Video,
}
#[derive(strum::Display, strum::EnumIter, Clone, PartialEq, Copy, Debug)]
pub enum Format {
    Audio { format: AudioFormat },
    Video { format: VideoFormat },
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, strum::Display, strum::EnumIter, Default, PartialEq, Copy, Debug)]
pub enum AudioFormat {
    #[default]
    MP3,
    WAV,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, strum::Display, strum::EnumIter, Default, PartialEq, Copy, Debug)]
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

/// Options applied at TUI launch, in order: TUI, MPV, audio output, MIDI.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub audio_only: bool,
    pub vo: Option<String>,
    pub embed: bool,
    pub audio_device: Option<String>,
    pub midi: bool,
    pub no_art: bool,
    pub img_protocol: ImgProtocol,
}

type MidiConnIn = MidiInputConnection<(std::sync::mpsc::Sender<u8>, std::sync::mpsc::Sender<()>)>;

/// Live MIDI connections + channels. Ports are (re)connected on demand,
/// either at launch (first port) or from the setup menu.
struct MidiRuntime {
    conn_in: Option<MidiConnIn>,
    conn_out: Option<MidiOutputConnection>,
    volume_rx: std::sync::mpsc::Receiver<u8>,
    pause_rx: std::sync::mpsc::Receiver<()>,
}

impl MidiRuntime {
    fn new() -> Self {
        let (_, volume_rx) = std::sync::mpsc::channel();
        let (_, pause_rx) = std::sync::mpsc::channel();
        Self {
            conn_in: None,
            conn_out: None,
            volume_rx,
            pause_rx,
        }
    }

    fn connect_input(&mut self, port: Option<MidiInputPort>) {
        self.conn_in = None;
        if let Some(port) = port {
            match MidiInput::new("ytrs-midi-in") {
                Ok(mut midi_in) => {
                    midi_in.ignore(midir::Ignore::None);
                    let (volume_tx, volume_rx) = std::sync::mpsc::channel();
                    let (pause_tx, pause_rx) = std::sync::mpsc::channel();
                    self.conn_in = listen_midi_input(midi_in, Some(port), volume_tx, pause_tx);
                    if self.conn_in.is_some() {
                        self.volume_rx = volume_rx;
                        self.pause_rx = pause_rx;
                    }
                }
                Err(e) => warn!(?e, "midi: input init failed"),
            }
        }
    }

    fn connect_output(&mut self, port: Option<MidiOutputPort>) {
        self.conn_out = None;
        if let Some(port) = port {
            match MidiOutput::new("ytrs-midi-out") {
                Ok(midi_out) => {
                    self.conn_out = midi_out.connect(&port, "midir-forward").ok();
                }
                Err(e) => warn!(?e, "midi: output init failed"),
            }
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
struct AudioDevice {
    name: String,
    description: Option<String>,
}

/// Background YouTube search: resolved results or a displayable error.
type SearchTask =
    tokio::task::JoinHandle<Result<Vec<(String, YoutubeResponse)>, String>>;

/// One page of suggestions + pager for the next page (Video only).
struct SuggestPage {
    rows: Vec<(String, YoutubeResponse)>,
    pager: Option<Paginator<VideoItem>>,
}

/// Background suggestions fetch (initial page or continuation).
type SuggestTask = tokio::task::JoinHandle<Result<SuggestPage, String>>;

/// Background thumbnail bytes fetch: (video id, image bytes).
type ThumbTask = tokio::task::JoinHandle<Vec<(String, image::DynamicImage)>>;

/// Outcome of a setup-menu event: keep going or respawn mpv with a new
/// audio-only/video mode.
enum SetupOutcome {
    Continue,
    Respawn { audio_only: bool },
}

impl AudioDevice {
    fn display(&self) -> String {
        match &self.description {
            Some(d) => format!("{} — {d}", self.name),
            None => self.name.clone(),
        }
    }
}

impl YoutubeRsBuilder {
    pub fn build(&mut self, cli: Cli) -> YoutubeRs {
        let yt = YoutubeRs {
            api: self.api,
            action: self.action.unwrap_or_default(),
            mpv_installed: YoutubeRs::check_mpv().unwrap_or_default(),
            last_search: Some(self.last_search.clone().unwrap_or_default()),
            args: cli,
            summarize: self.summarize,
            player: self.player.unwrap_or_default(),
            run_midi: self.midi,
            embed: self.embed,
            vo: self.vo.clone(),
            audio_device: self.audio_device.clone(),
            no_art: self.no_art,
            img_protocol: self.img_protocol,
        };
        debug!(
            api = ?yt.api,
            action = %yt.action,
            mpv_installed = yt.mpv_installed,
            last_search = ?yt.last_search,
            player = yt.player,
            midi = yt.run_midi,
            "builder produced YoutubeRs"
        );
        yt
    }
    pub fn api(&mut self, music: Option<bool>) -> &mut Self {
        self.api = Some(match music {
            Some(true) => {
                debug!("api: setting Music");
                YoutubeAPI::Music
            }
            _ => {
                debug!("api: setting Video (default)");
                YoutubeAPI::Video
            }
        });

        self
    }
    pub fn midi(&mut self, run_midi: bool) -> &mut Self {
        self.midi = run_midi;
        self
    }
    pub fn embed(&mut self, embed: bool) -> &mut Self {
        self.embed = embed;
        self
    }
    pub fn vo(&mut self, vo: Option<String>) -> &mut Self {
        self.vo = vo;
        self
    }
    pub fn audio_device(&mut self, device: Option<String>) -> &mut Self {
        self.audio_device = device;
        self
    }
    pub fn no_art(&mut self, no_art: bool) -> &mut Self {
        self.no_art = no_art;
        self
    }
    pub fn img_protocol(&mut self, protocol: ImgProtocol) -> &mut Self {
        self.img_protocol = protocol;
        self
    }
    pub fn transcript(&mut self) -> &mut Self {
        self.action = Some(AppAction::Transcript);
        self.api = Some(YoutubeAPI::Video);
        self
    }
    pub fn download(&mut self, format: Format) -> &mut Self {
        self.action = Some(AppAction::Download { format });
        self
    }
    pub fn player(&mut self) -> &mut Self {
        self.action = Some(AppAction::Player {
            format: Default::default(),
        });
        self
    }
    pub fn player_with_format(&mut self, format: Format) -> &mut Self {
        self.action = Some(AppAction::Player { format });
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
        self.api = Some(match url_is_music(&url) {
            Some(true) => YoutubeAPI::Music,
            _ => YoutubeAPI::Video,
        });
        if url_is_music(&url).is_none() {
            debug!("url: unknown domain, defaulting to Video API");
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
    /// App entry point (called as `app.run()` from `main`):
    /// terminal flows for download/transcript, TUI hub for the player.
    pub async fn run(&mut self) -> Result<()> {
        info!(action = %self.action, api = ?self.api, "run: starting");
        match self.action {
            AppAction::Download { format } => {
                info!(?format, "run: download action");
                if !Self::libraries_exist(&self.args.clone()) {
                    Self::install_lib(&self.args).await?;
                }
                let search_term = match self.last_search.clone() {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => crate::bootstrap::prompt_text("Youtube Search")?,
                };
                let (video_id, video_name) = self.pick_media(&search_term).await?;
                self.last_search = Some(search_term);
                debug!(?video_id, ?video_name, "run: downloading");
                match format {
                    Format::Audio { format } => {
                        Self::download_audio(video_id, &video_name, format, &self.args)
                            .await?;
                    }
                    Format::Video { format } => {
                        Self::download_video(&video_id, &video_name, format, &self.args)
                            .await?;
                    }
                }
            }
            AppAction::Transcript => {
                info!("run: transcript action");
                if !Self::libraries_exist(&self.args.clone()) {
                    Self::install_lib(&self.args).await?;
                }
                let search_term = match self.last_search.clone() {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => crate::bootstrap::prompt_text("Youtube Search")?,
                };
                let (video_id, _) = self.pick_media(&search_term).await?;
                self.last_search = Some(search_term);
                debug!(?video_id, "run: downloading transcript");
                self.download_transcript(&video_id, &self.args).await?;
            }
            AppAction::Player { format } => {
                info!(?format, "run: player action");
                if !self.mpv_installed {
                    self.mpv_installed = Self::check_mpv()?;
                    debug!(mpv_installed = self.mpv_installed, "run: rechecked MPV");
                }
                // URLs resolve straight to a response (no prompt);
                // anything else opens the empty hub and searches from the TUI.
                let mut response = match self.last_search.clone() {
                    Some(s) if s.starts_with("http") => {
                        self.match_url_response(&s).await?
                    }
                    _ => None,
                };
                debug!(
                    has_response = response.is_some(),
                    "run: launching TUI"
                );
                let mut opt_thumbnail = if let Some(res) = &response {
                    Self::fetch_yt_thumbnail(&res.get_id(), &self.args)
                        .await
                        .ok()
                } else {
                    None
                };
                let launch = LaunchOptions {
                    audio_only: matches!(format, Format::Audio { .. }),
                    vo: self.vo.clone(),
                    embed: self.embed,
                    audio_device: self.audio_device.clone(),
                    midi: self.run_midi,
                    no_art: self.no_art,
                    img_protocol: self.img_protocol,
                };
                self.run_tui(&mut response, &mut opt_thumbnail, launch)
                    .await;
            }
            AppAction::Quit => return Err(YtrsError::Quit.into()),
            AppAction::Update => {
                crate::bootstrap::update_yt_dlp(self.args.clone());
            }
        }
        Ok(())
    }
    /// Query terminal graphics support once (call after TUI init).
    /// An explicit `--img-protocol` choice is applied, otherwise the
    /// auto-detected protocol is kept as-is.
    fn make_picker(pref: ImgProtocol) -> Option<picker::Picker> {
        match picker::Picker::from_query_stdio() {
            Ok(mut picker) => {
                match pref {
                    ImgProtocol::Halfblocks => {
                        picker.set_protocol_type(picker::ProtocolType::Halfblocks)
                    }
                    ImgProtocol::Kitty => {
                        picker.set_protocol_type(picker::ProtocolType::Kitty)
                    }
                    ImgProtocol::Iterm2 => {
                        picker.set_protocol_type(picker::ProtocolType::Iterm2)
                    }
                    ImgProtocol::Auto => {}
                }
                debug!(
                    protocol = ?picker.protocol_type(),
                    font_size = ?picker.font_size(),
                    "picker: graphics protocol selected"
                );
                Some(picker)
            }
            Err(e) => {
                debug!(?e, "picker: terminal query failed, no artwork");
                None
            }
        }
    }

    /// Spawn mpv from launch options, apply the audio output device,
    /// and observe volume + playback-time + idle state.
    ///
    /// Video plays in mpv's own window (default vo); only `--embed` and an
    /// explicit `--vo` change the video output.
    async fn spawn_mpv(
        launch: &LaunchOptions,
        audio_only: bool,
        audio_device: Option<String>,
    ) -> Result<(
        MpvIpc,
        tokio::sync::watch::Receiver<f64>,
        tokio::sync::watch::Receiver<f64>,
        tokio::sync::watch::Receiver<bool>,
    )> {
        let mut opts = MpvSpawnOptions {
            ..Default::default()
        };
        // In embed mode, always enable video so a vo is used
        let spawn_audio_only = if launch.embed { false } else { audio_only };
        if spawn_audio_only {
            // Audio stays headless; mpv needs no video output.
        } else if launch.embed {
            // Embed owns the terminal: explicit vo (kitty default).
            opts.vo = Some(launch.vo.clone().unwrap_or_else(|| "kitty".to_string()));
        } else if let Some(vo) = launch.vo.clone() {
            // Plain video mode (own window): only an explicit --vo is passed.
            opts.vo = Some(vo);
        }
        let mut mpv = MpvIpc::spawn(&opts, spawn_audio_only).await?;
        debug!("run_tui: MPV spawned, applying audio output");
        if let Some(dev) = &audio_device {
            if let Err(e) = mpv.set_prop("audio-device", dev).await {
                warn!(?e, ?dev, "spawn_mpv: could not apply audio-device");
            }
        }
        let mpv_vol = mpv.observe_prop::<f64>("volume", 1.0).await;
        let time_rx = mpv.observe_prop::<f64>("playback-time", 0.0).await;
        let idle_rx = mpv.observe_prop::<bool>("idle-active", true).await;
        Ok((mpv, mpv_vol, time_rx, idle_rx))
    }

    /// Load the current response or local file into a (fresh) mpv instance.
    async fn load_media(
        mpv: &mut MpvIpc,
        response: &Option<YoutubeResponse>,
        file: &Option<(TaggedFile, String)>,
        audio_only: bool,
        args: &Cli,
    ) {
        if let Some(res) = response {
            let video_id = res.get_id();
            let url = Self::resolve_stream_url(args, &video_id, audio_only).await;
            debug!(url = %url[..url.len().min(80)], "load_media: resolved stream URL");
            match mpv.send_command(json!(["loadfile", url])).await {
                Ok(val) => debug!(?val, "load_media: loadfile command succeeded"),
                Err(e) => error!(?e, "load_media: loadfile command FAILED"),
            }
        } else if let Some(file) = file {
            debug!(path = %file.1, "load_media: loading local file into MPV");
            if let Err(e) = mpv.send_command(json!(["loadfile", file.1])).await {
                error!(?e, "load_media: failed to load local file");
            }
        } else {
            debug!("load_media: empty player mode, no media to load");
        }
    }

    async fn run_tui(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        opt_thumbnail: &mut Option<DynamicImage>,
        launch: LaunchOptions,
    ) {
        info!(
            ?launch,
            has_response = response.is_some(),
            "run_tui: starting"
        );
        // TUI first: launch options drive MPV, audio output and MIDI below.
        debug!("run_tui: initializing ratatui terminal");
        let mut term = ratatui::init();
        debug!(
            size = ?ratatui::crossterm::terminal::size(),
            "run_tui: terminal size"
        );
        // Single graphics-protocol query for this session (honors --img-protocol).
        let picker = Self::make_picker(launch.img_protocol);
        // Thumbnail / Album cover
        debug!("run_tui: setting up thumbnail");
        let mut img = if let Some(dyn_thumbnail) = &opt_thumbnail
            && let Some(picker) = picker.as_ref()
        {
            let protocol = picker.new_resize_protocol(dyn_thumbnail.clone());
            Some(protocol)
        } else {
            None
        };
        // MPV
        debug!("run_tui: checking for local file");
        let mut empty_player = false;
        let mut audio_file_error = None;
        let mut file: Option<(TaggedFile, String)> = self.get_file(
            &mut img,
            &mut empty_player,
            &mut audio_file_error,
            picker.as_ref(),
        );
        if file.is_none() && response.is_none() && !empty_player {
            // Opened for in-TUI search: don't crash, search from the player.
            warn!(?audio_file_error, "run_tui: no media yet, opening empty player");
            empty_player = true;
        }
        debug!(
            has_file = file.is_some(),
            empty_player,
            ?audio_file_error,
            "run_tui: file check result"
        );
        debug!("run_tui: spawning MPV");
        let mut audio_only = launch.audio_only;
        let mut current_audio_device = launch.audio_device.clone();
        let (mut mpv, mut mpv_vol, mut time_rx, mut idle_rx) = Self::spawn_mpv(
            &launch,
            audio_only,
            current_audio_device.clone(),
        )
        .await
        .context("Failed to spawn mpv process")
        .expect("Could not spawn MPV");
        Self::load_media(&mut mpv, response, &file, audio_only, &self.args).await;
        // Embed mode: skip TUI, just wait for mpv to exit
        if launch.embed && !audio_only {
            debug!("run_tui: embed mode active, skipping TUI");
            println!("Playing in terminal... Press Ctrl+C to exit");
            while mpv.running().await {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            mpv.quit().await;
            ratatui::restore();
            return;
        }
        // MIDI runtime: first ports auto-selected, reconfigurable in the setup menu.
        debug!("run_tui: initializing MIDI runtime");
        let mut midi = MidiRuntime::new();
        if launch.midi {
            midi.connect_input(
                MidiInput::new("ytrs-midi-in")
                    .ok()
                    .and_then(|m| m.ports().first().cloned()),
            );
            midi.connect_output(
                MidiOutput::new("ytrs-midi-out")
                    .ok()
                    .and_then(|m| m.ports().first().cloned()),
            );
        }
        // App Setup
        debug!("run_tui: entering TUI main loop");
        let mut playback_time = 0.0;
        let mut vid_started = false;
        let mut loader = ui::Loader::default();
        let mut pause_state = false;
        let mut open_popup = false;
        let mut videos_list: Vec<(String, YoutubeResponse)> = Vec::new();
        let mut selected_list_item = ListState::default();
        let mut popup_query = String::new();
        let mut search_task: Option<SearchTask> = None;
        let mut search_error: Option<String> = None;
        let mut suggestion = ui::SuggestionState::default();
        let mut sugg_task: Option<SuggestTask> = None;
        let mut sugg_error: Option<String> = None;
        let mut sugg_pager: Option<Paginator<VideoItem>> = None;
        let mut sugg_append = false;
        let mut thumb_task: Option<ThumbTask> = None;
        let mut transcript = ui::TranscriptState::default();
        let mut setup = ui::SetupState::default();
        let mut playlist = Playlist::default();
        let mut audio_device_names: Vec<String> = Vec::new();
        // Last seen mpv idle state, for playlist auto-advance on track end.
        let mut was_idle = true;

        // Theme and sidebar
        let mut theme = Theme::load();
        let (_, output_dir) = Self::get_libs_path(&self.args);
        let mut sidebar = Sidebar::new();

        // TUI Main Loop
        debug!("player: entering TUI main loop");
        // With --no-art the artwork stays empty.
        let mut no_art_img: Option<ratatui_image::protocol::StatefulProtocol> = None;
        loop {
            if let Some(v) = midi.volume_rx.try_iter().last() {
                // v is from 0 to 130
                mpv.send_command(json!(["set_property", "volume", v]))
                    .await
                    .unwrap();
            }
            if let Ok(()) = midi.pause_rx.try_recv() {
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
            // Track ended (mpv went idle): advance in the playlist with
            // wraparound, forever. Only when playing a queued entry.
            let _ = idle_rx.has_changed();
            let idle_now = *idle_rx.borrow();
            if !was_idle && idle_now {
                if Self::play_next_queued(
                    response,
                    &mut file,
                    &mut mpv,
                    &mut img,
                    &self.args,
                    &mut playlist,
                    audio_only,
                    picker.as_ref(),
                )
                .await
                {
                    debug!("playlist: track ended, advancing");
                }
            }
            was_idle = idle_now;

            // Background search finished: collect without blocking.
            // `await` on a finished JoinHandle returns immediately.
            if search_task.as_ref().is_some_and(|task| task.is_finished()) {
                let task = search_task.take().expect("search task checked above");
                match task.await {
                    Ok(Ok(items)) => {
                        debug!(count = items.len(), "run_tui: background search done");
                        videos_list = items;
                        selected_list_item
                            .select(videos_list.first().map(|_| 0));
                        popup_query.clear();
                        search_error = None;
                    }
                    Ok(Err(e)) => {
                        warn!(?e, "run_tui: background search failed");
                        search_error = Some(e);
                    }
                    Err(e) => {
                        debug!(?e, "run_tui: background search cancelled");
                        search_error = None;
                    }
                }
            }

            // Background suggestions finished: collect without blocking.
            if sugg_task.as_ref().is_some_and(|task| task.is_finished()) {
                let task = sugg_task.take().expect("suggest task checked above");
                match task.await {
                    Ok(Ok(page)) => {
                        debug!(count = page.rows.len(), "run_tui: suggestions done");
                        if sugg_append {
                            suggestion.items.extend(page.rows);
                        } else {
                            suggestion.items = page.rows;
                            suggestion.selected = 0;
                            suggestion.scrolltop = 0;
                            suggestion.thumbs.clear();
                        }
                        sugg_append = false;
                        sugg_pager = page.pager;
                        sugg_error = None;
                        suggestion.loading = false;
                        Self::spawn_missing_thumbs(&suggestion, &mut thumb_task);
                    }
                    Ok(Err(e)) => {
                        warn!(?e, "run_tui: suggestions failed");
                        sugg_error = Some(e);
                        suggestion.loading = false;
                    }
                    Err(e) => {
                        debug!(?e, "run_tui: suggestions cancelled");
                        sugg_error = None;
                        suggestion.loading = false;
                    }
                }
            }

            // Background thumbnails finished: build protocols on this thread.
            if thumb_task.as_ref().is_some_and(|task| task.is_finished()) {
                let task = thumb_task.take().expect("thumb task checked above");
                if let Ok(done) = task.await {
                    if let Some(picker) = picker.as_ref() {
                        for (id, img) in done {
                            suggestion
                                .thumbs
                                .insert(id, picker.new_resize_protocol(img));
                        }
                    }
                    if suggestion.thumbs.len() > 150 {
                        let live: std::collections::HashSet<&String> = suggestion
                            .items
                            .iter()
                            .map(|(_, res)| match res {
                                YoutubeResponse::Video(v) => &v.id,
                                YoutubeResponse::Track(t) => &t.id,
                            })
                            .collect();
                        suggestion.thumbs.retain(|id, _| live.contains(id));
                    }
                }
            }

            let _ = term.draw(|f| {
                let mut ctx = ui::DrawCtx {
                    playback: ui::Playback {
                        time: playback_time,
                        started: vid_started,
                        volume: *mpv_vol.borrow(),
                    },
                    loader: &mut loader,
                    search_open: open_popup,
                    search: ui::SearchView {
                        results: &videos_list,
                        selected: &mut selected_list_item,
                        query: &popup_query,
                        api: self.api,
                        searching: search_task.is_some(),
                        notice: search_error.as_deref().unwrap_or(""),
                    },
                    transcript: &transcript,
                    setup: &setup,
                    suggestion: &mut suggestion,
                    playlist: &playlist,
                    media: ui::Media::from_parts(response, &file, empty_player),
                    artwork: if launch.no_art { &mut no_art_img } else { &mut img },
                    theme: &theme,
                    sidebar: &mut sidebar,
                };
                ui::draw_screen(&mut ctx, f);
            });
            let event_happened = ratatui::crossterm::event::poll(Duration::from_millis(50))
                .is_ok_and(|event_happened| event_happened);
            if event_happened {
                let event = read().unwrap();
                if setup.open {
                    match Self::handle_setup_event(
                        &mut setup,
                        &mut mpv,
                        &mut midi,
                        &mut audio_device_names,
                        &mut current_audio_device,
                        &mut theme,
                        &event,
                    )
                    .await
                    {
                        SetupOutcome::Continue => {}
                        SetupOutcome::Respawn { audio_only: new_mode } => {
                            audio_only = new_mode;
                            debug!(audio_only, "run_tui: respawning mpv from setup");
                            mpv.quit().await;
                            (mpv, mpv_vol, time_rx, idle_rx) = Self::spawn_mpv(
                                &launch,
                                audio_only,
                                current_audio_device.clone(),
                            )
                            .await
                            .context("Failed to respawn mpv process")
                            .expect("Could not respawn MPV");
                            playback_time = 0.0;
                            vid_started = false;
                            pause_state = false;
                            was_idle = true;
                            Self::load_media(
                                &mut mpv,
                                response,
                                &file,
                                audio_only,
                                &self.args,
                            )
                            .await;
                        }
                    }
                } else if transcript.open {
                    Self::handle_transcript_event(
                        &mut transcript,
                        response,
                        &self.args,
                        &event,
                    )
                    .await;
                } else if suggestion.open {
                    Self::handle_suggestion_event(
                        &mut suggestion,
                        response,
                        &mut file,
                        &mut mpv,
                        &mut img,
                        audio_only,
                        picker.as_ref(),
                        &mut playlist,
                        &self.args,
                        &mut sugg_task,
                        &mut sugg_error,
                        &mut sugg_pager,
                        &mut sugg_append,
                        &mut thumb_task,
                        &event,
                    )
                    .await;
                } else if playlist.open {
                    Self::handle_playlist_event(
                        &mut playlist,
                        response,
                        &mut file,
                        &mut mpv,
                        &mut img,
                        audio_only,
                        picker.as_ref(),
                        &self.args,
                        &event,
                    )
                    .await;
                } else if sidebar.open {
                    self.handle_sidebar_event(
                        &mut sidebar,
                        &mut mpv,
                        &output_dir,
                        response,
                        &mut file,
                        &mut img,
                        picker.as_ref(),
                        &mut playlist,
                        &mut transcript,
                        &event,
                    )
                    .await;
                } else if open_popup {
                    self.handle_popup_event(
                        response,
                        &mut mpv,
                        &mut open_popup,
                        &mut videos_list,
                        &mut selected_list_item,
                        &mut popup_query,
                        &mut img,
                        audio_only,
                        picker.as_ref(),
                        &mut playlist,
                        &mut search_task,
                        &mut search_error,
                        &event,
                    )
                    .await;
                } else if let ControlFlow::Break(_) = self
                    .handle_playback_event(
                        response,
                        &mut file,
                        &mut mpv,
                        &mut pause_state,
                        &mut open_popup,
                        event,
                        empty_player,
                        &mut midi,
                        &mpv_vol.borrow(),
                        &mut sidebar,
                        &output_dir,
                        &mut transcript,
                        &mut setup,
                        &mut audio_device_names,
                        &mut playlist,
                        &mut img,
                        audio_only,
                        picker.as_ref(),
                        &mut suggestion,
                        &mut sugg_task,
                        &mut sugg_error,
                    )
                    .await
                {
                    if let Some(task) = search_task.take() {
                        task.abort();
                    }
                    if let Some(task) = sugg_task.take() {
                        task.abort();
                    }
                    if let Some(task) = thumb_task.take() {
                        task.abort();
                    }
                    break;
                }
            }
        }
        mpv.quit().await;
        ratatui::restore();
    }

    fn get_file(
        &mut self,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        empty_player: &mut bool,
        audio_file_error: &mut Option<String>,
        picker: Option<&picker::Picker>,
    ) -> Option<(TaggedFile, String)> {
        if let Some(s) = &self.last_search
            && !s.is_empty()
        {
            debug!(path = %s, "get_file: checking path");
            let f = PathBuf::from(s);
            if f.exists() && f.is_file() {
                debug!(path = %f.display(), "get_file: file exists, probing");
                use lofty::probe::Probe;
                if let Ok(file) = Probe::open(&f) {
                    match file.guess_file_type() {
                        Ok(file) => match file.read() {
                            Ok(tagged_file) => {
                                debug!("get_file: file read successfully");
                                if let Some(tag) = tagged_file.primary_tag()
                                    && let Some(pic) = tag.pictures().first()
                                    && let Ok(dyn_img) = image::load_from_memory(pic.data())
                                    && let Some(picker) = picker
                                {
                                    debug!("get_file: found album art");
                                    let protocole =
                                        picker.new_resize_protocol(dyn_img.clone());
                                    *img = Some(protocole);
                                }
                                Some((tagged_file, f.to_string_lossy().to_string()))
                            }
                            Err(e) => {
                                error!(?e, "get_file: could not read file");
                                *audio_file_error = Some(format!("Could not read file {e}"));
                                None
                            }
                        },
                        Err(e) => {
                            error!(?e, "get_file: could not guess file type");
                            *audio_file_error = Some(format!("Could not guess file type: {e}"));
                            None
                        }
                    }
                } else {
                    error!("get_file: could not open file");
                    *audio_file_error = Some("Could not open file".to_string());
                    None
                }
            } else {
                warn!(path = %f.display(), "get_file: file does not exist");
                *audio_file_error = Some(format!("File '{}' does not exist", f.to_string_lossy()));
                None
            }
        } else {
            debug!("get_file: no path in last_search, setting empty_player=true");
            *empty_player = true;
            None
        }
    }

    /// Probe a path into tagged audio + display path. No UI side effects.
    fn probe_file(path: &Path) -> Option<(TaggedFile, String)> {
        use lofty::probe::Probe;
        let probed = Probe::open(path).ok()?;
        let tagged = probed.guess_file_type().ok()?.read().ok()?;
        Some((tagged, path.to_string_lossy().to_string()))
    }

    /// Album art of tagged audio as an image protocol, if any.
    fn album_art(
        tagged: &TaggedFile,
        picker: Option<&picker::Picker>,
    ) -> Option<ratatui_image::protocol::StatefulProtocol> {
        let pic = tagged.primary_tag()?.pictures().first()?;
        let dyn_img = image::load_from_memory(pic.data()).ok()?;
        Some(picker?.new_resize_protocol(dyn_img))
    }

    /// Load a stream response into mpv (thumbnail + stream) and remember it.
    #[allow(clippy::too_many_arguments)]
    async fn play_stream(
        response: &mut Option<YoutubeResponse>,
        mpv: &mut MpvIpc,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        args: &Cli,
        vid: YoutubeResponse,
        audio_only: bool,
        picker: Option<&picker::Picker>,
    ) {
        let video_id = vid.get_id();
        debug!(?video_id, "play_item: loading stream into mpv");
        let url = Self::resolve_stream_url(args, &video_id, audio_only).await;
        match mpv.send_command(json!(["loadfile", url])).await {
            Ok(_) => debug!("play_item: loadfile command succeeded"),
            Err(e) => error!(?e, "play_item: loadfile command FAILED"),
        }
        *img = Self::load_thumbnail(args, &vid.get_id(), picker).await;
        *response = Some(vid);
    }

    /// Load a local file into mpv and remember it (tags + cover refreshed).
    #[allow(clippy::too_many_arguments)]
    async fn play_file(
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        mpv: &mut MpvIpc,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        path: String,
        picker: Option<&picker::Picker>,
    ) {
        debug!(?path, "play_item: loading local file into mpv");
        *response = None;
        match Self::probe_file(Path::new(&path)) {
            Some((tagged, name)) => {
                match mpv.send_command(json!(["loadfile", name])).await {
                    Ok(_) => debug!("play_item: loadfile command succeeded"),
                    Err(e) => error!(?e, "play_item: loadfile command FAILED"),
                }
                if let Some(art) = Self::album_art(&tagged, picker) {
                    *img = Some(art);
                }
                *file = Some((tagged, name));
            }
            None => error!(?path, "play_item: could not read local file"),
        }
    }

    /// Load a queue entry into mpv and remember it: streams resolve to a URL,
    /// local files play directly with refreshed tags + cover art.
    #[allow(clippy::too_many_arguments)]
    async fn play_item(
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        mpv: &mut MpvIpc,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        args: &Cli,
        item: PlaylistItem,
        audio_only: bool,
        picker: Option<&picker::Picker>,
    ) {
        match item {
            PlaylistItem::Stream(vid) => {
                Self::play_stream(response, mpv, img, args, vid, audio_only, picker).await;
            }
            PlaylistItem::File(path) => {
                Self::play_file(response, file, mpv, img, path, picker).await;
            }
        }
    }

    /// Run a suggestions fetch off the TUI event loop: related items for
    /// the given id, in display-row form. Video paginates, Music is finite.
    async fn run_suggestions(api: YoutubeAPI, id: String) -> Result<SuggestPage, String> {
        match api {
            YoutubeAPI::Video => {
                let details = RustyPipe::new()
                    .query()
                    .unauthenticated()
                    .video_details(&id)
                    .await
                    .map_err(|e| format!("Suggestions failed: {e:#}"))?;
                YoutubeRs::cleanup_rustypipe_cache();
                let rows = details
                    .recommended
                    .items
                    .iter()
                    .map(|v| (VideoInfo::from(v).to_string(), v.into()))
                    .collect();
                Ok(SuggestPage {
                    rows,
                    pager: Some(details.recommended),
                })
            }
            YoutubeAPI::Music => {
                let details = RustyPipe::new()
                    .query()
                    .unauthenticated()
                    .music_details(&id)
                    .await
                    .map_err(|e| format!("Suggestions failed: {e:#}"))?;
                YoutubeRs::cleanup_rustypipe_cache();
                let Some(related_id) = details.related_id else {
                    return Err("No related items for this track".to_string());
                };
                let related = RustyPipe::new()
                    .query()
                    .unauthenticated()
                    .music_related(&related_id)
                    .await
                    .map_err(|e| format!("Suggestions failed: {e:#}"))?;
                YoutubeRs::cleanup_rustypipe_cache();
                let rows = related
                    .tracks
                    .into_iter()
                    .chain(related.other_versions)
                    .map(|track| (TrackInfo::from(&track).to_string(), track.into()))
                    .collect();
                Ok(SuggestPage { rows, pager: None })
            }
        }
    }

    /// Fetch the next suggestions page (Video continuation).
    async fn run_suggest_more(
        pager: Paginator<VideoItem>,
    ) -> Result<SuggestPage, String> {
        let query = RustyPipe::new().query();
        match pager
            .next(query)
            .await
            .map_err(|e| format!("More suggestions failed: {e:#}"))?
        {
            Some(next) => {
                let rows = next
                    .items
                    .iter()
                    .map(|v| (VideoInfo::from(v).to_string(), v.into()))
                    .collect();
                Ok(SuggestPage {
                    rows,
                    pager: Some(next),
                })
            }
            None => Ok(SuggestPage {
                rows: Vec::new(),
                pager: None,
            }),
        }
    }

    /// Smallest thumbnail (id, url) of a response, to keep downloads light.
    fn thumb_url(res: &YoutubeResponse) -> Option<(String, String)> {
        let (id, thumbs) = match res {
            YoutubeResponse::Video(v) => (v.id.clone(), &v.thumbnail),
            YoutubeResponse::Track(t) => (t.id.clone(), &t.cover),
        };
        thumbs
            .iter()
            .filter(|t| t.width > 0)
            .min_by_key(|t| t.width)
            .or(thumbs.first())
            .map(|t| (id, t.url.clone()))
    }

    /// Download + decode thumbnails off the event loop. Failures are skipped.
    async fn fetch_thumbs(requests: Vec<(String, String)>) -> Vec<(String, image::DynamicImage)> {
        let client = reqwest::Client::new();
        let mut out = Vec::new();
        for (id, url) in requests {
            if let Ok(resp) = client.get(&url).send().await
                && let Ok(bytes) = resp.bytes().await
                && let Ok(img) = image::load_from_memory(&bytes)
            {
                out.push((id, img));
            }
        }
        out
    }

    /// Spawn a thumbnail fetch for suggestion items missing from the cache.
    /// No-op while another thumbnail task runs.
    fn spawn_missing_thumbs(
        suggestion: &ui::SuggestionState,
        thumb_task: &mut Option<ThumbTask>,
    ) {
        if thumb_task.is_some() {
            return;
        }
        let missing: Vec<(String, String)> = suggestion
            .items
            .iter()
            .filter_map(|(_, res)| {
                let (id, url) = Self::thumb_url(res)?;
                (!suggestion.thumbs.contains_key(&id)).then_some((id, url))
            })
            .collect();
        if !missing.is_empty() {
            debug!(count = missing.len(), "run_tui: fetching thumbnails");
            *thumb_task = Some(tokio::spawn(Self::fetch_thumbs(missing)));
        }
    }

    /// Run a YouTube/Music search off the TUI event loop.
    /// Returns display rows or an error string for the popup status line.
    async fn run_search(
        api: YoutubeAPI,
        query: String,
    ) -> Result<Vec<(String, YoutubeResponse)>, String> {
        match api {
            YoutubeAPI::Music => {
                let found = RustyPipe::new()
                    .query()
                    .unauthenticated()
                    .music_search_tracks(query)
                    .await
                    .map_err(|e| format!("YouTube Music search failed: {e:#}"))?;
                YoutubeRs::cleanup_rustypipe_cache();
                Ok(found
                    .items
                    .items
                    .into_iter()
                    .map(|track| (TrackInfo::from(&track).to_string(), track.into()))
                    .collect())
            }
            YoutubeAPI::Video => {
                let found: rustypipe::model::SearchResult<VideoItem> = RustyPipe::new()
                    .query()
                    .unauthenticated()
                    .search(query)
                    .await
                    .map_err(|e| format!("YouTube search failed: {e:#}"))?;
                YoutubeRs::cleanup_rustypipe_cache();
                Ok(found
                    .items
                    .items
                    .iter()
                    .map(|v| (VideoInfo::from(v).to_string(), v.into()))
                    .collect())
            }
        }
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
        audio_only: bool,
        picker: Option<&picker::Picker>,
        playlist: &mut Playlist,
        search_task: &mut Option<SearchTask>,
        search_error: &mut Option<String>,
        event: &ratatui::crossterm::event::Event,
    ) {
        // Item under the cursor when the input is empty: `p` appends it to
        // the playlist instead of being typed. Otherwise `p` types normally.
        let picked = if popup_query.is_empty() {
            selected_list_item
                .selected()
                .and_then(|i| videos_list.get(i))
                .map(|v| v.1.clone())
        } else {
            None
        };
        // NOTE: `p` with a picked item is the playlist shortcut below, not text.
        if event.is_key_press()
            && let KeyCode::Char(ch) = event.as_key_event().unwrap().code
            && (ch != 'p' || picked.is_none())
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
            // Switch the search source only: mpv keeps playing untouched.
            // Audio-only vs video is controlled from the setup menu.
            match self.api {
                Some(YoutubeAPI::Music) => self.api = Some(YoutubeAPI::Video),
                Some(YoutubeAPI::Video) => self.api = Some(YoutubeAPI::Music),
                None => self.api = Some(YoutubeAPI::Video),
            }
            // Drop results from the other source, but keep the query text
            // so Enter re-runs it right away.
            videos_list.clear();
            selected_list_item.select(None);
            search_error.take();
            if let Some(task) = search_task.take() {
                task.abort();
            }
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Up {
            selected_list_item.select_previous();
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Down {
            selected_list_item.select_next();
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Esc {
            if let Some(task) = search_task.take() {
                task.abort();
            }
            *open_popup = false;
            selected_list_item.select(None);
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Enter {
            if let Some(selected) = selected_list_item.selected()
                && popup_query.is_empty()
            {
                if let Some(vid) = videos_list.get(selected).map(|v| v.1.clone()) {
                    popup_query.clear();
                    Self::play_stream(
                        response,
                        mpv,
                        img,
                        &self.args,
                        vid,
                        audio_only,
                        picker,
                    )
                    .await;
                    // Back to the Player view (gauge + info panel).
                    videos_list.clear();
                    selected_list_item.select(None);
                    *open_popup = false;
                }
            } else if !popup_query.is_empty() {
                // Search in the background: the TUI keeps rendering and
                // stays responsive while rustypipe works.
                if search_task.is_none()
                    && let Some(api) = self.api
                {
                    debug!(query = %popup_query, ?api, "popup: spawning background search");
                    videos_list.clear();
                    selected_list_item.select(None);
                    search_error.take();
                    let query = popup_query.clone();
                    *search_task = Some(tokio::spawn(Self::run_search(api, query)));
                }
            }
        }
        // `p` appends the picked result to the playlist (a typed `p`
        // still goes to the query above when nothing is picked).
        // The first entry starts playing right away.
        if event.is_key_press()
            && event.as_key_event().unwrap().code == KeyCode::Char('p')
            && let Some(vid) = picked
        {
            let first = playlist.is_empty();
            playlist.add(PlaylistItem::Stream(vid.clone()));
            debug!(first, "popup: added entry to playlist");
            if first {
                Self::play_stream(response, mpv, img, &self.args, vid, audio_only, picker)
                    .await;
                videos_list.clear();
                selected_list_item.select(None);
                *open_popup = false;
            }
        }
    }

    fn clipboard(text: &str) -> Result<()> {
        terminal_clipboard::set_string(text)
            .map_err(|e| anyhow::anyhow!("Clipboard error: {:?}", e))?;
        Ok(())
    }
    fn get_video_url(video_id: impl Into<String>) -> String {
        format!("https://www.youtube.com/watch?v={}", video_id.into())
    }
    fn cleanup_rustypipe_cache() {
        // Missing file is fine (e.g. search failed before writing it).
        let _ = std::fs::remove_file("./rustypipe_cache.json");
    }

    async fn fetch_yt_thumbnail(video_id: &str, args: &Cli) -> Result<DynamicImage> {
        let thumbnail_url = if Self::ytdlp_exist(args) {
            Self::fetch_video_info(args, video_id)
                .await
                .context("Could not get fetcher")?
                .thumbnail
                .context("Could not get thumbnail")?
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

    /// Load a YouTube thumbnail and convert it to a ratatui-image protocol, or None on failure.
    async fn load_thumbnail(
        args: &Cli,
        video_id: &str,
        picker: Option<&picker::Picker>,
    ) -> Option<ratatui_image::protocol::StatefulProtocol> {
        let dyn_img = Self::fetch_yt_thumbnail(video_id, args).await.ok()?;
        let picker = picker?;
        Some(picker.new_resize_protocol(dyn_img))
    }

    async fn download_audio(
        video_id: impl std::fmt::Display,
        video_name: &str,
        format: AudioFormat,
        args: &Cli,
    ) -> Result<()> {
        info!(video_id = %video_id, video_name = %video_name, ?format, "download_audio: starting");
        let safe_name =
            video_name.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "_");
        let vid_info = Self::fetch_video_info(args, video_id.to_string().as_str())
            .await
            .unwrap();
        let fetcher = Self::get_downloader(args).await?;
        let downloaded = fetcher
            .download_audio_stream_with_quality(
                &vid_info,
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
        tag.set_artist(vid_info.channel.unwrap());
        tag.set_genre(vid_info.tags.iter().cloned().collect());
        let thumbnail = reqwest::Client::new()
            .get(vid_info.thumbnail.unwrap())
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
        video_id: impl std::fmt::Display,
        video_name: &str,
        format: VideoFormat,
        args: &Cli,
    ) -> Result<()> {
        info!(video_id = %video_id, video_name = %video_name, ?format, "download_video: starting");
        let fetcher = Self::get_downloader(args).await?;
        let safe_name =
            video_name.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "_");
        let video_info = fetcher
            .fetch_video_infos(Self::get_video_url(video_id.to_string()))
            .await?;
        let downloaded = fetcher
            .download_video_with_quality(
                &video_info,
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
        info!(?video_id, "download_transcript: starting");
        let fetcher = Self::get_downloader(args).await?;

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
                if let Some(desc) = video.description {
                    println!("{}: \n{}", "Video Description".green(), desc);
                }
                return Ok(());
            }
            let lang = match crate::bootstrap::prompt_select(
                "Generated Lang",
                &cap.iter().map(|(lang, _)| lang.clone()).collect::<Vec<_>>(),
            )? {
                Some(idx) => cap[idx].0.clone(),
                None => Err(anyhow!(YtrsError::Quit))?,
            };
            for (l, cap) in cap {
                if lang == l {
                    let res: Vec<Subtitle> = cap
                        .iter()
                        .map(|v| Subtitle::from_automatic_caption(v, l.clone()))
                        .collect();
                    let res_to_dl = match crate::bootstrap::prompt_select(
                        "Caption",
                        &res
                            .iter()
                            .map(|s| format!("{} [{}]", s.url, s.file_extension()))
                            .collect::<Vec<_>>(),
                    )? {
                        Some(idx) => res[idx].clone(),
                        None => Err(anyhow!(YtrsError::Quit))?,
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
                        crate::bootstrap::prompt_confirm("Summarize with ai ?", false)?
                    };
                    if res {
                        use tokio::io::{self, AsyncWriteExt};
                        use tokio_stream::StreamExt;

                        let ollama = Ollama::default();
                        let models = ollama.list_local_models().await?;
                        let model = match crate::bootstrap::prompt_select(
                            "Which LLM to use:",
                            &models.iter().map(|llm| llm.name.clone()).collect::<Vec<_>>(),
                        )? {
                            Some(idx) => models[idx].name.clone(),
                            None => Err(anyhow!(YtrsError::Quit))?,
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

        let selected_lang = match crate::bootstrap::prompt_select("Lang", &languages)? {
            Some(idx) => languages[idx].clone(),
            None => Err(anyhow!(YtrsError::Quit))?,
        };
        // Download English subtitles
        let subtitle_path = fetcher
            .download_subtitle(
                &video,
                selected_lang.clone(),
                format!("subtitle_{selected_lang}.srt"),
                true,
            )
            .await?;
        println!("Subtitle downloaded to: {:?}", subtitle_path);

        Ok(())
    }

    async fn search_tracks(term: &str) -> Result<Vec<TrackItem>> {
        debug!(?term, "search_tracks: searching YouTube Music");
        let found = RustyPipe::new()
            .query()
            .unauthenticated()
            .music_search_tracks(term.to_string())
            .await
            .context("Failed to search YouTube Music")?;
        Self::cleanup_rustypipe_cache();
        debug!(count = found.items.items.len(), "search_tracks: results received");
        Ok(found.items.items)
    }

    async fn search_videos(term: &str) -> Result<Vec<VideoItem>> {
        debug!(?term, "search_videos: searching YouTube");
        let found: rustypipe::model::SearchResult<VideoItem> = RustyPipe::new()
            .query()
            .unauthenticated()
            .search(term.to_string())
            .await
            .context("Failed to search YouTube")?;
        Self::cleanup_rustypipe_cache();
        debug!(count = found.items.items.len(), "search_videos: results received");
        Ok(found.items.items)
    }

    /// Resolve a search term (or URL) to `(video_id, display_name)`.
    /// URLs auto-match their id against the results, single results are
    /// taken directly, otherwise a numbered stdin pick replaces the old
    /// inquire selection.
    async fn pick_media(&self, term: &str) -> Result<(String, String)> {
        use crate::bootstrap::prompt_select;

        let wanted_id = extract_video_id(term);
        match self.api {
            Some(YoutubeAPI::Music) => {
                let items = Self::search_tracks(term).await?;
                if items.is_empty() {
                    bail!("No results for '{term}'");
                }
                if let Some(id) = wanted_id {
                    return items
                        .iter()
                        .find(|t| t.id == id)
                        .map(|t| (t.id.clone(), t.name.clone()))
                        .context("URL video not found in search results");
                }
                if items.len() == 1 {
                    let t = &items[0];
                    return Ok((t.id.clone(), t.name.clone()));
                }
                let labels: Vec<String> =
                    items.iter().map(|t| TrackInfo::from(t).colored()).collect();
                match prompt_select("Select Music", &labels)? {
                    Some(idx) => {
                        let t = &items[idx];
                        Ok((t.id.clone(), t.name.clone()))
                    }
                    None => bail!("User cancelled"),
                }
            }
            _ => {
                let items = Self::search_videos(term).await?;
                if items.is_empty() {
                    bail!("No results for '{term}'");
                }
                if let Some(id) = wanted_id {
                    return items
                        .iter()
                        .find(|v| v.id == id)
                        .map(|v| (v.id.clone(), v.name.clone()))
                        .context("URL video not found in search results");
                }
                if items.len() == 1 {
                    let v = &items[0];
                    return Ok((v.id.clone(), v.name.clone()));
                }
                let labels: Vec<String> =
                    items.iter().map(|v| VideoInfo::from(v).colored()).collect();
                match prompt_select("Select video", &labels)? {
                    Some(idx) => {
                        let v = &items[idx];
                        Ok((v.id.clone(), v.name.clone()))
                    }
                    None => bail!("User cancelled"),
                }
            }
        }
    }

    /// Resolve a `--url` straight to a response (no prompt): match the
    /// extracted id against search results. `Ok(None)` opens the empty hub.
    async fn match_url_response(&self, url: &str) -> Result<Option<YoutubeResponse>> {
        let Some(id) = extract_video_id(url) else {
            warn!("match_url_response: no video id in URL, opening empty hub");
            return Ok(None);
        };
        debug!(?id, "match_url_response: resolving URL");
        match self.api {
            Some(YoutubeAPI::Music) => match Self::search_tracks(&id).await {
                Ok(items) => Ok(items
                    .into_iter()
                    .find(|t| t.id == id)
                    .map(YoutubeResponse::Track)),
                Err(e) => {
                    warn!(?e, "match_url_response: music lookup failed");
                    Ok(None)
                }
            },
            _ => match Self::search_videos(&id).await {
                Ok(items) => Ok(items
                    .into_iter()
                    .find(|v| v.id == id)
                    .map(YoutubeResponse::Video)),
                Err(e) => {
                    warn!(?e, "match_url_response: video lookup failed");
                    Ok(None)
                }
            },
        }
    }
    pub fn check_mpv() -> Result<bool> {
        debug!("check_mpv: running mpv --version");
        let output = std::process::Command::new("mpv")
            .args(["--version"])
            .output();
        match output {
            Ok(output) => {
                let success = output.status.success();
                debug!(success, "check_mpv: result");
                Ok(success)
            }
            Err(e) => {
                error!(?e, "check_mpv: mpv not found");
                Err(YtrsError::MpvNotFound.into())
            }
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
    fn libraries_exist(args: &Cli) -> bool {
        let ytdlp = Self::ytdlp_exist(args);
        let ffmpeg = Self::ffmpeg_check(args);
        if !ytdlp {
            warn!(
                path = %Self::get_libs(args).youtube.to_string_lossy(),
                "libraries_exist: yt-dlp not found"
            );
        }
        if !ffmpeg {
            warn!(
                path = %Self::get_libs(args).ffmpeg.to_string_lossy(),
                "libraries_exist: ffmpeg not found"
            );
        }
        debug!(ytdlp, ffmpeg, "libraries_exist: check result");
        ytdlp && ffmpeg
    }

    async fn install_lib(args: &Cli) -> Result<()> {
        info!("install_lib: installing yt-dlp and ffmpeg");
        let (exec_dir, _) = Self::get_libs_path(args);
        debug!(?exec_dir, "install_lib: target directory");
        install_libraries!(exec_dir)?;
        info!("install_lib: done");
        Ok(())
    }
    #[cfg(target_os = "windows")]
    fn get_libs_path(args: &Cli) -> (PathBuf, PathBuf) {
        let base = crate::utility::home_dir().map(|home| home.join(".config").join("ytrs"));
        let exec_dir = if let Some(libs_path) = &args.libs_path {
            libs_path.join("libs")
        } else if let Some(base) = &base {
            base.join("libs")
        } else {
            PathBuf::from("libs")
        };
        let output_dir = if let Some(output) = &args.output_path {
            output.join("output")
        } else if let Some(base) = &base {
            base.join("output")
        } else {
            PathBuf::from("output")
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
    async fn get_downloader(args: &Cli) -> Result<Downloader> {
        let (_, out) = Self::get_libs_path(args);
        let libs = Self::get_libs(args);
        Downloader::builder(libs, out)
            .build()
            .await
            .context("Failed to retrieve Youtube Fetcher")
    }
    pub async fn update_yt_dlp(args: &Cli) -> Result<()> {
        let (libs, out) = Self::get_libs_path(args);
        let libraries = Libraries::new(libs.join("yt-dlp"), libs.join("ffmpeg"));
        let dl = Downloader::builder(libraries, out).build().await?;
        dl.update_downloader().await?;
        Ok(())
    }
    async fn fetch_video_info(args: &Cli, video_id: &str) -> Option<Video> {
        let (libs, out) = Self::get_libs_path(args);
        let libraries = Libraries::new(libs.join("yt-dlp"), libs.join("ffmpeg"));
        if let Ok(x) = yt_dlp::Downloader::builder(libraries, out).build().await {
            if let Ok(x) = x.fetch_video_infos(Self::get_video_url(video_id)).await {
                return Some(x);
            }
        }
        None
    }
    fn get_stream_url(video: &Video, audio_only: bool) -> Option<String> {
        video
            .formats
            .iter()
            .filter(|f| {
                if audio_only {
                    f.is_audio() && !f.is_video()
                } else {
                    f.is_audio() && f.is_video()
                }
            })
            .filter_map(|f| f.url().ok().cloned())
            .next()
    }

    /// Resolve the best playback URL for a video: try yt-dlp stream URL first, fall back to page URL.
    async fn resolve_stream_url(args: &Cli, video_id: &str, audio_only: bool) -> String {
        let stream_url = Self::fetch_video_info(args, video_id)
            .await
            .and_then(|video| Self::get_stream_url(&video, audio_only));
        match stream_url {
            Some(url) => url,
            None => Self::get_video_url(video_id),
        }
    }
    /// Play the entry queued after what's currently playing (wraparound,
    /// forever). Only when the current track/ file is itself queued.
    /// Returns true when something was loaded.
    #[allow(clippy::too_many_arguments)]
    async fn play_next_queued(
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        mpv: &mut MpvIpc,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        args: &Cli,
        playlist: &mut Playlist,
        audio_only: bool,
        picker: Option<&picker::Picker>,
    ) -> bool {
        let current_key: Option<String> = response
            .as_ref()
            .map(|r| r.get_id())
            .or_else(|| file.as_ref().map(|f| PlaylistItem::file_id(&f.1)));
        if let Some(key) = current_key
            && let Some(pos) = playlist.index_of(&key)
            && let Some(item) = playlist.get((pos + 1) % playlist.len()).cloned()
        {
            Self::play_item(response, file, mpv, img, args, item, audio_only, picker).await;
            true
        } else {
            false
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_playback_event(
        &mut self,
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        mpv: &mut MpvIpc,
        pause_state: &mut bool,
        open_popup: &mut bool,
        event: ratatui::crossterm::event::Event,
        empty_player: bool,
        midi: &mut MidiRuntime,
        mpv_vol: &f64,
        sidebar: &mut Sidebar,
        output_dir: &Path,
        transcript: &mut ui::TranscriptState,
        setup: &mut ui::SetupState,
        audio_device_names: &mut Vec<String>,
        playlist: &mut Playlist,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        audio_only: bool,
        picker: Option<&picker::Picker>,
        suggestion: &mut ui::SuggestionState,
        sugg_task: &mut Option<SuggestTask>,
        sugg_error: &mut Option<String>,
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
        // Download the current stream in the background (player format).
        // The file shows up in the download sidebar once finished.
        if event.is_key_press()
            && event.as_key_event().unwrap().code == KeyCode::Char('d')
            && let Some(res) = response
        {
            let video_id = res.get_id();
            let video_name = res.get_name();
            let format = match self.action {
                AppAction::Player { format } | AppAction::Download { format } => format,
                _ => Format::Audio {
                    format: AudioFormat::MP3,
                },
            };
            let args = self.args.clone();
            info!(?video_id, ?format, "player: background download started");
            tokio::spawn(async move {
                if !YoutubeRs::libraries_exist(&args)
                    && let Err(e) = YoutubeRs::install_lib(&args).await
                {
                    error!(?e, "player: background download lib install failed");
                    return;
                }
                let outcome = match format {
                    Format::Audio { format } => {
                        YoutubeRs::download_audio(video_id, &video_name, format, &args).await
                    }
                    Format::Video { format } => {
                        YoutubeRs::download_video(&video_id, &video_name, format, &args).await
                    }
                };
                match outcome {
                    Ok(()) => info!("player: background download finished"),
                    Err(e) => error!(?e, "player: background download failed"),
                }
            });
        }
        // Suggestion screen for the current stream (same-type related
        // items). Fetched in the background, like search.
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Char('s')
        {
            if suggestion.open {
                if let Some(task) = sugg_task.take() {
                    task.abort();
                }
                suggestion.open = false;
                suggestion.loading = false;
            } else {
                *open_popup = false;
                suggestion.open = true;
                suggestion.selected = 0;
                suggestion.scrolltop = 0;
                suggestion.items.clear();
                suggestion.thumbs.clear();
                sugg_error.take();
                if let Some(res) = response {
                    let api = match res {
                        YoutubeResponse::Video(_) => YoutubeAPI::Video,
                        YoutubeResponse::Track(_) => YoutubeAPI::Music,
                    };
                    suggestion.api = Some(api);
                    suggestion.title = res.get_name();
                    suggestion.notice.clear();
                    suggestion.loading = true;
                    let id = res.get_id();
                    debug!(?api, ?id, "player: spawning suggestions fetch");
                    *sugg_task = Some(tokio::spawn(Self::run_suggestions(api, id)));
                } else {
                    suggestion.api = None;
                    suggestion.title.clear();
                    suggestion.notice = "Play a stream first".to_string();
                    suggestion.loading = false;
                }
            }
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
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Home {
            let _ = mpv.send_command(json!(["seek", 0, "absolute"])).await;
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::End
            && !Self::play_next_queued(
                response,
                file,
                mpv,
                img,
                &self.args,
                playlist,
                audio_only,
                picker,
            )
            .await
        {
            // Nothing queued after this track: stop playback.
            let _ = mpv.send_command(json!(["stop"])).await;
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Up {
            let _ = mpv.send_command(json!(["add", "volume", "5"])).await;
            if let Some(out_midi_connection) = &mut midi.conn_out {
                let _ = out_midi_connection.send(&[224, 0, u32_to_midi(*mpv_vol as u32)]);
            }
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Down {
            let _ = mpv.send_command(json!(["add", "volume", "-5"])).await;
            if let Some(out_midi_connection) = &mut midi.conn_out {
                let _ = out_midi_connection.send(&[224, 0, u32_to_midi(*mpv_vol as u32)]);
            }
        }
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Char('D') {
            sidebar.toggle(output_dir);
        }
        if (response.is_some() | empty_player)
            && event.is_key_press()
            && event.as_key_event().unwrap().code == KeyCode::Char('o')
        {
            if self.api.is_none() {
                self.api = Some(YoutubeAPI::Video);
            }
            *open_popup = !*open_popup;
        }
        // Playlist sidebar (play queue).
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Char('P') {
            playlist.toggle();
        }
        // Transcript menu for the current stream: pick a track first.
        if event.is_key_press()
            && event.as_key_event().unwrap().code == KeyCode::Char('t')
            && let Some(res) = response
        {
            let video_id = res.get_id();
            transcript.open = true;
            transcript.title = res.get_name();
            transcript.picking = true;
            transcript.filter.clear();
            transcript.filtering = false;
            transcript.sel = 0;
            transcript.lines.clear();
            transcript.summary.clear();
            transcript.selected = None;
            transcript.list_error.clear();
            match Self::list_transcript_tracks(&video_id, &self.args).await {
                Ok(tracks) => {
                    if tracks.is_empty() {
                        transcript.list_error =
                            "No transcripts available for this video".to_string();
                    } else {
                        transcript.tracks = tracks;
                    }
                }
                Err(e) => {
                    transcript.list_error = format!("Transcript list failed: {e:#}");
                }
            }
        }
        // Setup menu: MIDI ports + mpv audio output + playback mode.
        if event.is_key_press() && event.as_key_event().unwrap().code == KeyCode::Char('e') {
            setup.open = true;
            Self::refresh_setup(setup, mpv, audio_device_names, audio_only).await;
        }
        ControlFlow::Continue(())
    }

    /// Handle key events when the playlist sidebar is open.
    /// `j/k` select, `d` removes, `Shift+J/K` reorders, `Enter` plays, `Esc` closes.
    #[allow(clippy::too_many_arguments)]
    async fn handle_playlist_event(
        playlist: &mut Playlist,
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        mpv: &mut MpvIpc,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        audio_only: bool,
        picker: Option<&picker::Picker>,
        args: &Cli,
        event: &ratatui::crossterm::event::Event,
    ) {
        if !event.is_key_press() {
            return;
        }
        match event.as_key_event().unwrap().code {
            KeyCode::Esc | KeyCode::Char('P') => {
                playlist.toggle();
            }
            KeyCode::Up | KeyCode::Char('k') => playlist.select_prev(),
            KeyCode::Down | KeyCode::Char('j') => playlist.select_next(),
            KeyCode::Char('K') => playlist.move_selected(-1),
            KeyCode::Char('J') => playlist.move_selected(1),
            KeyCode::Char('d') => playlist.remove_selected(),
            KeyCode::Enter => {
                if let Some(item) = playlist.current().cloned() {
                    Self::play_item(response, file, mpv, img, args, item, audio_only, picker)
                        .await;
                }
            }
            _ => {}
        }
    }

    /// (Re)fetch suggestions for `id`, replacing the list.
    #[allow(clippy::too_many_arguments)]
    fn start_suggest_fetch(
        suggestion: &mut ui::SuggestionState,
        sugg_task: &mut Option<SuggestTask>,
        sugg_error: &mut Option<String>,
        sugg_pager: &mut Option<Paginator<VideoItem>>,
        sugg_append: &mut bool,
        thumb_task: &mut Option<ThumbTask>,
        api: YoutubeAPI,
        id: String,
    ) {
        if let Some(task) = sugg_task.take() {
            task.abort();
        }
        if let Some(task) = thumb_task.take() {
            task.abort();
        }
        sugg_error.take();
        suggestion.api = Some(api);
        suggestion.items.clear();
        suggestion.thumbs.clear();
        suggestion.selected = 0;
        suggestion.scrolltop = 0;
        suggestion.loading = true;
        *sugg_pager = None;
        *sugg_append = false;
        debug!(?api, ?id, "suggestion: spawning fetch");
        *sugg_task = Some(tokio::spawn(Self::run_suggestions(api, id)));
    }

    /// Fetch the next page when the selection nears the bottom (Video only:
    /// Music has no continuation).
    #[allow(clippy::too_many_arguments)]
    fn maybe_fetch_more_suggestions(
        suggestion: &mut ui::SuggestionState,
        sugg_task: &mut Option<SuggestTask>,
        sugg_pager: &mut Option<Paginator<VideoItem>>,
        sugg_append: &mut bool,
    ) {
        if sugg_task.is_some() || suggestion.items.is_empty() {
            return;
        }
        if suggestion.selected + 2 < suggestion.items.len() {
            return;
        }
        if let Some(pager) = sugg_pager.clone() {
            debug!("suggestion: fetching next page");
            *sugg_append = true;
            suggestion.loading = true;
            *sugg_task = Some(tokio::spawn(Self::run_suggest_more(pager)));
        }
    }

    /// Handle key events when the suggestion screen is open: 2D grid nav,
    /// Enter plays, p queues, Tab switches Music/Video + refetch.
    #[allow(clippy::too_many_arguments)]
    async fn handle_suggestion_event(
        suggestion: &mut ui::SuggestionState,
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        mpv: &mut MpvIpc,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        audio_only: bool,
        picker: Option<&picker::Picker>,
        playlist: &mut Playlist,
        args: &Cli,
        sugg_task: &mut Option<SuggestTask>,
        sugg_error: &mut Option<String>,
        sugg_pager: &mut Option<Paginator<VideoItem>>,
        sugg_append: &mut bool,
        thumb_task: &mut Option<ThumbTask>,
        event: &ratatui::crossterm::event::Event,
    ) {
        if !event.is_key_press() {
            return;
        }
        match event.as_key_event().unwrap().code {
            KeyCode::Esc | KeyCode::Char('s') => {
                if let Some(task) = sugg_task.take() {
                    task.abort();
                }
                if let Some(task) = thumb_task.take() {
                    task.abort();
                }
                suggestion.open = false;
                suggestion.loading = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                suggestion.selected =
                    ui::grid_move(suggestion.selected, suggestion.items.len(), 0, -1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                suggestion.selected =
                    ui::grid_move(suggestion.selected, suggestion.items.len(), 0, 1);
                Self::maybe_fetch_more_suggestions(
                    suggestion,
                    sugg_task,
                    sugg_pager,
                    sugg_append,
                );
            }
            KeyCode::Left | KeyCode::Char('h') => {
                suggestion.selected =
                    ui::grid_move(suggestion.selected, suggestion.items.len(), -1, 0);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                suggestion.selected =
                    ui::grid_move(suggestion.selected, suggestion.items.len(), 1, 0);
            }
            KeyCode::Enter => {
                if let Some((_, vid)) =
                    suggestion.items.get(suggestion.selected).cloned()
                {
                    Self::play_item(
                        response,
                        file,
                        mpv,
                        img,
                        args,
                        PlaylistItem::Stream(vid),
                        audio_only,
                        picker,
                    )
                    .await;
                }
            }
            KeyCode::Char('p') => {
                if let Some((_, vid)) =
                    suggestion.items.get(suggestion.selected).cloned()
                {
                    let first = playlist.is_empty();
                    playlist.add(PlaylistItem::Stream(vid.clone()));
                    debug!(first, "suggestion: added entry to playlist");
                    if first {
                        Self::play_stream(response, mpv, img, args, vid, audio_only, picker)
                            .await;
                    }
                }
            }
            KeyCode::Tab => {
                if let Some(res) = response {
                    let api = match suggestion.api {
                        Some(YoutubeAPI::Music) => YoutubeAPI::Video,
                        _ => YoutubeAPI::Music,
                    };
                    Self::start_suggest_fetch(
                        suggestion,
                        sugg_task,
                        sugg_error,
                        sugg_pager,
                        sugg_append,
                        thumb_task,
                        api,
                        res.get_id(),
                    );
                } else {
                    suggestion.notice = "Play a stream first".to_string();
                }
            }
            _ => {}
        }
    }

    /// Handle key events when the sidebar is open.
    /// `Enter` loads the selected file into the player, `p` appends it to
    /// the playlist (autoplay when first). Transcript files open in the
    /// transcript view instead of playing.
    #[allow(clippy::too_many_arguments)]
    async fn handle_sidebar_event(
        &mut self,
        sidebar: &mut Sidebar,
        mpv: &mut MpvIpc,
        output_dir: &Path,
        response: &mut Option<YoutubeResponse>,
        file: &mut Option<(TaggedFile, String)>,
        img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
        picker: Option<&picker::Picker>,
        playlist: &mut Playlist,
        transcript: &mut ui::TranscriptState,
        event: &ratatui::crossterm::event::Event,
    ) {
        if !event.is_key_press() {
            return;
        }
        match event.as_key_event().unwrap().code {
            KeyCode::Esc | KeyCode::Char('D') => {
                if sidebar.confirm_delete {
                    sidebar.confirm_delete = false;
                } else {
                    sidebar.open = false;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                sidebar.up();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                sidebar.down();
            }
            KeyCode::Enter => {
                if let Some(path) = sidebar.selected() {
                    if is_transcript_file(path) {
                        Self::open_transcript_file(transcript, path);
                        sidebar.open = false;
                    } else {
                        let path_str = path.to_string_lossy().to_string();
                        self.last_search = Some(path_str.clone());
                        Self::play_file(response, file, mpv, img, path_str, picker).await;
                        sidebar.open = false;
                    }
                }
            }
            KeyCode::Char('p') => {
                if let Some(path) = sidebar.selected() {
                    let path_str = path.to_string_lossy().to_string();
                    if Self::probe_file(Path::new(&path_str)).is_some() {
                        let first = playlist.is_empty();
                        playlist.add(PlaylistItem::File(path_str.clone()));
                        debug!(path = %path_str, first, "sidebar: added file to playlist");
                        if first {
                            self.last_search = Some(path_str.clone());
                            Self::play_file(response, file, mpv, img, path_str, picker)
                                .await;
                            sidebar.open = false;
                        }
                    } else {
                        warn!(path = %path_str, "sidebar: unreadable file, not queued");
                    }
                }
            }
            KeyCode::Char('r') => {
                sidebar.refresh(output_dir);
            }
            KeyCode::Char('d') => {
                if sidebar.selected().is_some() {
                    sidebar.confirm_delete = true;
                }
            }
            KeyCode::Char('y') => {
                if sidebar.confirm_delete {
                    sidebar.confirm_delete = false;
                    if let Some(path) = sidebar.selected() {
                        let path_str = path.to_string_lossy().to_string();
                        match std::fs::remove_file(path) {
                            Ok(()) => {
                                debug!(path = %path_str, "sidebar: deleted file");
                                sidebar.refresh(output_dir);
                            }
                            Err(e) => error!(?e, path = %path_str, "sidebar: delete failed"),
                        }
                    }
                }
            }
            KeyCode::Char('n') => {
                sidebar.confirm_delete = false;
            }
            _ => {}
        }
    }

    /// Handle key events when the transcript menu is open.
    /// Picking level: choose a track (`/` filters). Reading level: scroll,
    /// reload and summarize. `Esc` goes back a level.
    async fn handle_transcript_event(
        transcript: &mut ui::TranscriptState,
        response: &Option<YoutubeResponse>,
        args: &Cli,
        event: &ratatui::crossterm::event::Event,
    ) {
        if !event.is_key_press() {
            return;
        }
        if transcript.picking {
            Self::handle_transcript_pick(transcript, response, args, event).await;
            return;
        }
        match event.as_key_event().unwrap().code {
            KeyCode::Esc => {
                transcript.picking = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                transcript.scroll = transcript.scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = transcript.lines.len().saturating_sub(1);
                transcript.scroll = (transcript.scroll + 1).min(max);
            }
            KeyCode::Home => {
                transcript.scroll = 0;
            }
            KeyCode::End => {
                transcript.scroll = transcript.lines.len().saturating_sub(1);
            }
            // Retry loading the selected track.
            KeyCode::Char('r') => {
                if let Some(track) = transcript.selected.clone()
                    && let Some(res) = response
                {
                    Self::load_transcript_track(transcript, &res.get_id(), args, &track)
                        .await;
                }
            }
            // Summarize the loaded script with the first local Ollama model.
            KeyCode::Char('s') => {
                if transcript.summary.is_empty() && !transcript.lines.is_empty() {
                    match Self::summarize_lines(&transcript.lines).await {
                        Ok((model, text)) => {
                            let mut out = vec![format!("Model: {model}")];
                            out.extend(text.lines().map(str::to_string));
                            transcript.summary = out;
                        }
                        Err(e) => {
                            transcript.summary =
                                vec![format!("Summarize failed (Ollama?): {e:#}")];
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Picking level of the transcript menu: navigate, `/`-filter, load.
    async fn handle_transcript_pick(
        transcript: &mut ui::TranscriptState,
        response: &Option<YoutubeResponse>,
        args: &Cli,
        event: &ratatui::crossterm::event::Event,
    ) {
        let visible_len = transcript.visible_tracks().len();
        let max_sel = visible_len.saturating_sub(1);
        transcript.sel = transcript.sel.min(max_sel);
        match event.as_key_event().unwrap().code {
            KeyCode::Esc => {
                if transcript.filtering {
                    transcript.filtering = false;
                } else {
                    transcript.open = false;
                }
            }
            KeyCode::Char('/') => {
                transcript.filtering = !transcript.filtering;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                transcript.sel = transcript.sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                transcript.sel = (transcript.sel + 1).min(max_sel);
            }
            KeyCode::Backspace => {
                if transcript.filtering {
                    if event
                        .as_key_event()
                        .unwrap()
                        .modifiers
                        .contains(KeyModifiers::CONTROL)
                    {
                        transcript.filter.clear();
                    } else {
                        transcript.filter.pop();
                    }
                    transcript.sel = 0;
                }
            }
            KeyCode::Enter => {
                if let Some(track) = transcript.visible_tracks().get(transcript.sel).map(|t| (*t).clone())
                    && let Some(res) = response
                {
                    Self::load_transcript_track(transcript, &res.get_id(), args, &track).await;
                }
            }
            _ => {}
        }
        // Typing while filtering narrows the list (handled after match so
        // that `/`, Esc and navigation above keep working).
        if transcript.filtering
            && event.is_key_press()
            && let KeyCode::Char(ch) = event.as_key_event().unwrap().code
            && ch != '/'
        {
            transcript.filter.push(ch);
            transcript.sel = 0;
        }
    }

    /// Open a local subtitle file in the transcript reading view.
    fn open_transcript_file(transcript: &mut ui::TranscriptState, path: &Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        debug!(path = %path.display(), "sidebar: opening transcript file");
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut lines = clean_srt(&text);
        if lines.is_empty() {
            lines = clean_caption_text(&text);
        }
        transcript.open = true;
        transcript.picking = false;
        transcript.title = name;
        transcript.lang = transcript_lang_from_name(path);
        transcript.scroll = 0;
        transcript.summary.clear();
        transcript.selected = None;
        transcript.lines = if lines.is_empty() {
            vec!["Empty or unreadable transcript file".to_string()]
        } else {
            lines
        };
    }

    /// Fetch one picked track into the reading view.
    async fn load_transcript_track(
        transcript: &mut ui::TranscriptState,
        video_id: &str,
        args: &Cli,
        track: &ui::TranscriptTrack,
    ) {
        transcript.lines = vec!["Loading transcript…".to_string()];
        transcript.summary.clear();
        transcript.scroll = 0;
        match Self::fetch_transcript_for(video_id, args, &track.lang).await {
            Ok((lang, lines)) => {
                debug!(count = lines.len(), "load_transcript_track: loaded");
                transcript.lang = lang;
                transcript.lines = lines;
                transcript.selected = Some(track.clone());
                transcript.picking = false;
            }
            Err(e) => {
                debug!(?e, "load_transcript_track: failed");
                transcript.list_error = format!("Transcript unavailable: {e:#}");
                transcript.picking = true;
            }
        }
    }

    /// Handle key events when the setup menu is open.
    /// Applying the playback mode respawns mpv (handled by the main loop).
    /// `t` cycles the theme live (persisted to theme.toml).
    async fn handle_setup_event(
        setup: &mut ui::SetupState,
        mpv: &mut MpvIpc,
        midi: &mut MidiRuntime,
        audio_names: &mut Vec<String>,
        current_audio_device: &mut Option<String>,
        theme: &mut Theme,
        event: &ratatui::crossterm::event::Event,
    ) -> SetupOutcome {
        if !event.is_key_press() {
            return SetupOutcome::Continue;
        }
        match event.as_key_event().unwrap().code {
            KeyCode::Esc => {
                setup.open = false;
            }
            KeyCode::Tab => {
                setup.advance_focus();
            }
            KeyCode::Char('t') => {
                let next = Theme::next_preset_name(&theme.preset);
                *theme = Theme::preset(next);
                theme.save();
                setup.notice = format!("Theme: {next} (saved)");
            }
            KeyCode::Up | KeyCode::Char('k') => setup.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => setup.move_selection(1),
            KeyCode::Enter => match setup.focus {
                ui::SetupFocus::MidiIn => {
                    midi.connect_input(midi_input_port_at(setup.midi_in_sel));
                    setup.notice = if midi.conn_in.is_some() {
                        format!(
                            "MIDI input: {}",
                            setup.midi_in.get(setup.midi_in_sel).cloned().unwrap_or_default()
                        )
                    } else {
                        "MIDI input disconnected".to_string()
                    };
                }
                ui::SetupFocus::MidiOut => {
                    midi.connect_output(midi_output_port_at(setup.midi_out_sel));
                    setup.notice = if midi.conn_out.is_some() {
                        format!(
                            "MIDI output: {}",
                            setup
                                .midi_out
                                .get(setup.midi_out_sel)
                                .cloned()
                                .unwrap_or_default()
                        )
                    } else {
                        "MIDI output disconnected".to_string()
                    };
                }
                ui::SetupFocus::Audio => {
                    if let Some(name) = audio_names.get(setup.audio_sel).cloned() {
                        match mpv.set_prop("audio-device", &name).await {
                            Ok(()) => {
                                *current_audio_device = Some(name.clone());
                                setup.notice = format!("Audio output: {name}");
                            }
                            Err(e) => setup.notice = format!("Audio output failed: {e:#}"),
                        }
                    }
                }
                ui::SetupFocus::Playback => {
                    let audio_only = setup.play_audio_only();
                    setup.notice = if audio_only {
                        "MPV respawned audio-only".to_string()
                    } else {
                        "MPV respawned with video".to_string()
                    };
                    return SetupOutcome::Respawn { audio_only };
                }
            },
            _ => {}
        }
        SetupOutcome::Continue
    }

    /// Refresh the setup menu lists: MIDI port names + mpv audio devices +
    /// playback mode (preselected from the current mpv mode).
    async fn refresh_setup(
        setup: &mut ui::SetupState,
        mpv: &mut MpvIpc,
        audio_names: &mut Vec<String>,
        audio_only: bool,
    ) {
        setup.midi_in = midi_input_names();
        setup.midi_out = midi_output_names();
        setup.midi_in_sel = setup
            .midi_in_sel
            .min(setup.midi_in.len().saturating_sub(1));
        setup.midi_out_sel = setup
            .midi_out_sel
            .min(setup.midi_out.len().saturating_sub(1));
        setup.play_modes = vec!["Audio only".to_string(), "Video".to_string()];
        setup.play_sel = if audio_only { 0 } else { 1 };
        match mpv.get_prop::<Vec<AudioDevice>>("audio-device-list").await {
            Ok(devices) => {
                *audio_names = devices.iter().map(|d| d.name.clone()).collect();
                setup.audio_devices = devices.iter().map(|d| d.display()).collect();
                if let Ok(current) = mpv.get_prop::<String>("audio-device").await
                    && let Some(idx) = audio_names.iter().position(|n| *n == current)
                {
                    setup.audio_sel = idx;
                }
                setup.notice.clear();
            }
            Err(e) => setup.notice = format!("audio-device-list failed: {e:#}"),
        }
    }

    /// List available transcript tracks for a video: manual subtitles
    /// first, then auto-generated captions.
    async fn list_transcript_tracks(
        video_id: &str,
        args: &Cli,
    ) -> Result<Vec<ui::TranscriptTrack>> {
        info!(?video_id, "list_transcript_tracks: starting");
        let fetcher = Self::get_downloader(args).await?;
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let video = fetcher.fetch_video_infos(url).await?;
        let mut manual: Vec<String> = video.subtitles.keys().cloned().collect();
        manual.sort();
        let mut auto: Vec<String> = video.automatic_captions.keys().cloned().collect();
        auto.sort();
        debug!(?manual, ?auto, "list_transcript_tracks: found");
        let tracks = manual
            .into_iter()
            .map(|lang| ui::TranscriptTrack { lang, manual: true })
            .chain(auto.into_iter().map(|lang| ui::TranscriptTrack {
                lang,
                manual: false,
            }))
            .collect();
        Ok(tracks)
    }

    /// Fetch one transcript track by language: manual subtitles first
    /// (SRT preferred), then automatic captions. Plain reqwest download
    /// with a timeout — the parallel yt-dlp asset machinery hangs on some
    /// timedtext URLs. Returns (lang, lines).
    async fn fetch_transcript_for(
        video_id: &str,
        args: &Cli,
        lang: &str,
    ) -> Result<(String, Vec<String>)> {
        use yt_dlp::model::caption::Extension;

        info!(?video_id, ?lang, "fetch_transcript_for: starting");
        let fetcher = Self::get_downloader(args).await?;
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let video = fetcher.fetch_video_infos(url).await?;

        let manual: Vec<Subtitle> = video.subtitles.get(lang).cloned().unwrap_or_default();
        let subs: Vec<Subtitle> = if manual.is_empty() {
            video
                .automatic_captions
                .get(lang)
                .map(|caps| {
                    caps.iter()
                        .map(|c| Subtitle::from_automatic_caption(c, lang.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            manual
        };
        let sub = subs
            .iter()
            .find(|s| s.is_format(&Extension::Srt))
            .or_else(|| subs.iter().find(|s| s.is_format(&Extension::Vtt)))
            .or_else(|| subs.first())
            .context("No downloadable track for this language")?;
        debug!(url = %sub.url, ext = sub.file_extension(), "fetch_transcript_for: downloading");
        let text = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .get(&sub.url)
            .send()
            .await
            .context("Transcript download failed")?
            .error_for_status()
            .context("Transcript download failed")?
            .text()
            .await?;
        debug!(bytes = text.len(), "fetch_transcript_for: downloaded");
        // SRT/VTT clean first, XML-caption clean as fallback (auto tracks).
        let mut lines = clean_srt(&text);
        if lines.is_empty() {
            lines = clean_caption_text(&text);
        }
        if lines.is_empty() {
            bail!("Transcript content came back empty");
        }
        debug!(count = lines.len(), "fetch_transcript_for: cleaned");
        Ok((lang.to_string(), lines))
    }

    /// Summarize script lines with the first local Ollama model.
    async fn summarize_lines(lines: &[String]) -> Result<(String, String)> {
        use tokio_stream::StreamExt;

        let ollama = Ollama::default();
        let models = ollama.list_local_models().await?;
        let model = models
            .first()
            .context("No local Ollama models available")?
            .name
            .clone();
        let mut stream = ollama
            .generate_stream(GenerationRequest::new(
                model.clone(),
                format!(
                    "Summarize this content in a few bullet points:\n```{}```",
                    lines.join("\n")
                ),
            ))
            .await?;
        let mut out = String::new();
        while let Some(res) = stream.next().await {
            for resp in res? {
                out.push_str(&resp.response);
            }
        }
        Ok((model, out))
    }
}

fn listen_midi_input(
    midi_in: MidiInput,
    opt_midi_in_port: Option<MidiInputPort>,
    midi_volume_tx: std::sync::mpsc::Sender<u8>,
    midi_pause_tx: std::sync::mpsc::Sender<()>,
) -> Option<MidiInputConnection<(std::sync::mpsc::Sender<u8>, std::sync::mpsc::Sender<()>)>> {
    if let Some(in_port) = opt_midi_in_port {
        midi_in
            .connect(
                &in_port,
                "midir-read-input",
                move |_, message, midi_tx| {
                    let midi_event = midi::parse_midi(message);
                    match midi_event {
                        midi::MidiEvent::NoteOn {
                            channel: _,
                            note,
                            velocity: _,
                        } => {
                            if matches!(note, 93 | 94) {
                                let _ = midi_tx.1.send(());
                            }
                        }
                        midi::MidiEvent::PitchBend { channel: _, value } => {
                            let _ = midi_tx.0.send(pitch_bend_to_mpv_vol(value));
                        }
                        _ => {}
                    }
                },
                (midi_volume_tx, midi_pause_tx),
            )
            .ok()
    } else {
        None
    }
}

fn midi_output_names() -> Vec<String> {
    let mut names = vec!["None".to_string()];
    if let Ok(midi_out) = MidiOutput::new("ytrs-midi-out") {
        for (i, port) in midi_out.ports().iter().enumerate() {
            names.push(format!("{i}:{}", midi_out.port_name(port).unwrap_or_default()));
        }
    }
    names
}

fn midi_input_names() -> Vec<String> {
    let mut names = vec!["None".to_string()];
    if let Ok(midi_in) = MidiInput::new("ytrs-midi-in") {
        for (i, port) in midi_in.ports().iter().enumerate() {
            names.push(format!(
                "{i}:{}",
                midi_in.port_name(port).unwrap_or_default()
            ));
        }
    }
    names
}

/// Selection index into the `*_names()` lists: 0 is "None", otherwise ports[idx - 1].
fn midi_output_port_at(sel: usize) -> Option<MidiOutputPort> {
    if sel == 0 {
        return None;
    }
    MidiOutput::new("ytrs-midi-out")
        .ok()?
        .ports()
        .get(sel - 1)
        .cloned()
}

fn midi_input_port_at(sel: usize) -> Option<MidiInputPort> {
    if sel == 0 {
        return None;
    }
    MidiInput::new("ytrs-midi-in")
        .ok()?
        .ports()
        .get(sel - 1)
        .cloned()
}

fn u32_to_midi(val: u32) -> u8 {
    ((val * 127) / 130) as u8
}

fn pitch_bend_to_mpv_vol(bend: i16) -> u8 {
    let bend = bend.clamp(-8192, 8191);
    let normalized = bend as i32 + 8192;
    let vol = ((normalized * 130) + 8191) / 16383;
    vol as u8
}

/// Subtitle/transcript file extensions opened in the transcript view.
fn is_transcript_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("srt" | "vtt" | "ass" | "ssa" | "ttml" | "srv3" | "dfxp" | "sbv" | "txt")
    )
}

/// Language guess from our `subtitle_{lang}.ext` file names.
fn transcript_lang_from_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("subtitle_"))
        .unwrap_or("")
        .to_string()
}

/// Drop `<tags>` from a caption line and unescape common entities.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Keep readable SRT lines: drop counters, timestamps and markup.
fn clean_srt(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty() && !l.chars().all(|c| c.is_ascii_digit()) && !l.contains("-->")
        })
        .map(strip_tags)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Keep readable auto-caption lines: drop markup, empties and consecutive dupes.
fn clean_caption_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cleaned = strip_tags(line).trim().to_string();
        if !cleaned.is_empty() && out.last() != Some(&cleaned) {
            out.push(cleaned);
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn transcript_file_detection() {
        assert!(is_transcript_file(&PathBuf::from("subtitle_en.srt")));
        assert!(is_transcript_file(&PathBuf::from("sub.VTT".to_lowercase())));
        assert!(is_transcript_file(&PathBuf::from("a.ass")));
        assert!(!is_transcript_file(&PathBuf::from("song.mp3")));
        assert!(!is_transcript_file(&PathBuf::from("video.mp4")));
        assert!(!is_transcript_file(&PathBuf::from("noext")));
    }

    #[test]
    fn transcript_lang_from_our_filenames() {
        assert_eq!(
            transcript_lang_from_name(&PathBuf::from("subtitle_en.srt")),
            "en"
        );
        assert_eq!(
            transcript_lang_from_name(&PathBuf::from("subtitle_fr.srv3")),
            "fr"
        );
        assert_eq!(transcript_lang_from_name(&PathBuf::from("other.srt")), "");
    }
}
