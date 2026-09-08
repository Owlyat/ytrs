use std::path::PathBuf;

use clap::Parser;

#[derive(clap::Parser, Clone, Debug)]
#[command(name = "ytrs")]
#[command(about = "A CLI for initializing the YTRS TUI with arguments")]
pub struct Cli {
    #[clap(short, long)]
    pub libs_path: Option<PathBuf>,
    #[clap(short, long)]
    pub output_path: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<AppActionCli>,
}
impl Default for Cli {
    fn default() -> Self {
        Cli::parse()
    }
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum AppActionCli {
    /// Download directly from the url or query from the TUI
    Download {
        #[clap(short, long, conflicts_with = "url")]
        query: Option<String>,
        #[clap(short, long)]
        url: Option<String>,
        /// Download kind (default: audio)
        #[clap(short, long)]
        format: Option<DownloadFormat>,
        /// Codec: mp3|wav for audio, mp4|avi|mov for video
        #[clap(short, long)]
        codec: Option<String>,
    },
    /// Play from the provided url or file
    Player {
        #[clap(short, long)]
        file: Option<PathBuf>,
        #[clap(short, long, conflicts_with = "file")]
        url: Option<String>,
        #[clap(short, long, conflicts_with = "file")]
        api: Option<PlayerAPI>,
        #[clap(short, long)]
        midi: bool,
        /// Bypass TUI and render video in terminal using mpv
        #[clap(long)]
        embed: bool,
        /// Video output backend for video mode (default: from terminal capability, e.g. kitty/sixel/tct)
        #[clap(long)]
        vo: Option<String>,
        /// MPV audio output device (see list in the in-player setup menu)
        #[clap(long)]
        audio_device: Option<String>,
        /// Disable album art / thumbnails in the player
        #[clap(long)]
        no_art: bool,
        /// Image protocol for artwork (default: auto-detected)
        #[clap(long, value_enum)]
        img_protocol: Option<ImgProtocol>,
    },
    /// Download the transcript using the query
    Transcript {
        #[clap(short, long, conflicts_with = "url")]
        query: Option<String>,
        #[clap(short, long)]
        url: Option<String>,
        #[clap(short, long, help = "Requires Ollama or llama.cpp")]
        summarize: Option<bool>,
    },
    /// Update Yt-dlp
    Update,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum PlayerAPI {
    Video,
    Music,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default)]
pub enum DownloadFormat {
    #[default]
    Audio,
    Video,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default)]
pub enum ImgProtocol {
    #[default]
    Auto,
    /// Cell-based fallback, always works
    Halfblocks,
    /// GPU-graphics protocol (needs terminal support)
    Kitty,
    /// Inline images (needs terminal support, may cover the TUI if broken)
    Iterm2,
}
