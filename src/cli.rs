use std::path::PathBuf;

#[derive(clap::Parser, Clone)]
pub struct Cli {
    #[clap(short, long)]
    pub libs_path: Option<PathBuf>,
    #[clap(short, long)]
    pub output_path: Option<PathBuf>,
}
