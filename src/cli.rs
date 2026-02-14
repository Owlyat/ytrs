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
    },
    /// Download the transcript using the query
    Transcript {
        #[clap(short, long, conflicts_with = "url")]
        query: Option<String>,
        #[clap(short, long)]
        url: Option<String>,
        #[clap(short, long, help = "Requires Ollama")]
        summarize: Option<bool>,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum PlayerAPI {
    Video,
    Music,
}
