use core::panic;
use rustypipe::{
    client::RustyPipe,
    model::{TrackItem, traits::YtEntity},
    param::StreamFilter,
};
use std::{path::PathBuf, process::Command};
use yt_dlp::{Youtube, fetcher::deps::Libraries};

#[tokio::main]
async fn main() {
    YTRSAction::default().run().await;
}

const ACTIONS: &[&str] = &["Watch", "Listen", "YT-DLP", "Exit"];

#[derive(Default)]
pub enum YTRSAction {
    #[default]
    None,
    Ytdlp {
        yt_query: YtQuery,
    },
    Watch {
        yt_query: YtQuery,
    },
    Listen {
        yt_query: YtQuery,
    },
}
impl YTRSAction {
    pub async fn run(&mut self) {
        loop {
            match inquire::Select::new("Select an action", ACTIONS.to_vec())
                .prompt()
                .unwrap()
            {
                "Watch" => {
                    self.watch().await;
                }
                "Listen" => self.listen().await,
                "YT-DLP" => self.yt_dlp().await,
                "Exit" => break,
                _ => {}
            }
        }
    }
    async fn watch(&mut self) {
        *self = Self::Watch {
            yt_query: YtQuery::new().await,
        };
        let rp = RustyPipe::new();
        let player = rp.query().player(self.get_id().unwrap()).await.unwrap();
        let (video, audio) = player.select_video_audio_stream(&StreamFilter::default());
        let mut args = vec![video.expect("No Video Stream").url.to_owned()];
        if let Some(audio) = audio {
            args.push(format!("--audio-file={}", audio.url));
        }
        if Command::new("mpv").args(["--version"]).output().is_ok() {
            Command::new("mpv").args(args).output().unwrap();
        } else {
            panic!("MPV not installed")
        }
    }
    async fn listen(&mut self) {
        *self = Self::Listen {
            yt_query: YtQuery::new().await,
        };
        let url = &format!("https://www.youtube.com/watch?v={}", self.get_id().unwrap());

        if Command::new("mpv").args(["--version"]).output().is_ok() {
            Command::new("mpv")
                .args(["--no-video", url])
                .output()
                .unwrap();
        } else {
            panic!("MPV not installed")
        }
    }
    async fn yt_dlp(&mut self) {
        let output_dir = PathBuf::from("output");
        let libraries_dir = PathBuf::from("libs");

        match inquire::Select::new("Action", vec!["Download", "Update", "Install"])
            .prompt()
            .unwrap()
        {
            "Download" => {
                *self = Self::Ytdlp {
                    yt_query: YtQuery::new().await,
                };
                let url = &format!("https://www.youtube.com/watch?v={}", self.get_id().unwrap());

                let ytdlp = libraries_dir.join("yt-dlp");
                let ffmpeg = libraries_dir.join("ffmpeg");

                let libraries = Libraries::new(ytdlp, ffmpeg);
                let fetcher = Youtube::new(libraries, output_dir).unwrap();

                match inquire::Select::new("Download :", vec!["All", "Audio-Only", "Video-Only"])
                    .prompt()
                    .unwrap()
                {
                    "All" => {
                        let extension = inquire::Select::new(
                            "Format",
                            vec!["mp4", "webm", "avi", "mov", "mkv", "flv"],
                        )
                        .prompt()
                        .unwrap();
                        match fetcher
                            .download_video_from_url(
                                url.clone(),
                                format!("{}.{}", self.get_name().unwrap(), extension),
                            )
                            .await
                        {
                            Ok(path) => {
                                println!("✅ Video Downloaded at {}", path.to_string_lossy());
                            }
                            Err(e) => {
                                println!("❌ Error downloading video {}", e);
                            }
                        }
                    }
                    "Audio-Only" => {
                        let extension = inquire::Select::new(
                            "Extension",
                            vec![
                                "mp3", "wav", "flac", "aac", "alac", "ogg", "m4a", "opus", "vorbis",
                            ],
                        )
                        .prompt()
                        .unwrap();
                        match fetcher
                            .download_video_from_url(
                                url.clone(),
                                format!("{}.{}", self.get_name().unwrap(), extension),
                            )
                            .await
                        {
                            Ok(path) => {
                                println!("✅ Audio Downloaded at {}", path.to_string_lossy());
                            }
                            Err(e) => {
                                println!("❌ Error while downloading audio {}", e);
                            }
                        }
                    }
                    "Video-Only" => {
                        let extension =
                            inquire::Select::new("Extension", vec!["mp4", "avi", "mov", "webm"])
                                .prompt()
                                .unwrap();
                        match fetcher
                            .download_video_stream_from_url(
                                url.clone(),
                                format!("{}.{}", self.get_name().unwrap(), extension),
                            )
                            .await
                        {
                            Ok(path) => {
                                println!("✅ Video Downloaded at {}", path.to_string_lossy());
                            }
                            Err(e) => {
                                println!("❌ Error while downloading Video {}", e);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "Update" => {
                let ytdlp = libraries_dir.join("yt-dlp");
                let ffmpeg = libraries_dir.join("ffmpeg");

                let libraries = Libraries::new(ytdlp, ffmpeg);
                let fetcher = Youtube::new(libraries, output_dir);

                if let Ok(ytdlp) = fetcher {
                    if let Ok(()) = ytdlp.update_downloader().await {
                    } else {
                        println!("❌ Error while updating yt-dlp");
                    }
                }
            }
            "Install" => {
                println!("➡️ Downloading Libs ...");

                let fetcher = Youtube::with_new_binaries(libraries_dir, output_dir).await;
                match fetcher {
                    Ok(_) => {
                        println!("✅ Libs Installed Successfully");
                    }
                    Err(e) => {
                        println!("❌ Error while Installing libraries, {}", e);
                    }
                }
            }
            _ => {}
        }
    }
    fn get_id(&self) -> Result<String, ()> {
        match self {
            YTRSAction::None => {}
            YTRSAction::Ytdlp { yt_query } => return Ok(yt_query.video.id.clone()),
            YTRSAction::Watch { yt_query } => return Ok(yt_query.video.id.clone()),
            YTRSAction::Listen { yt_query } => return Ok(yt_query.video.id.clone()),
        }
        Err(())
    }
    fn get_name(&self) -> Result<String, ()> {
        match self {
            YTRSAction::None => (),
            YTRSAction::Ytdlp { yt_query } => return Ok(yt_query.video.name.clone()),
            YTRSAction::Watch { yt_query } => return Ok(yt_query.video.name.clone()),
            YTRSAction::Listen { yt_query } => return Ok(yt_query.video.name.clone()),
        }
        Err(())
    }
}

pub struct YtQuery {
    video: TrackItem,
}
impl YtQuery {
    async fn new() -> Self {
        let search_term = inquire::Text::new("Youtube Search :").prompt().unwrap();
        let rp = RustyPipe::new();
        let found_videos = rp
            .query()
            .unauthenticated()
            .music_search_tracks(search_term)
            .await
            .unwrap();
        let selected_vid_str = inquire::Select::new(
            "Select Music",
            found_videos
                .clone()
                .items
                .items
                .into_iter()
                .map(|x| x.name.to_string())
                .collect(),
        )
        .prompt()
        .unwrap();
        let vid = found_videos
            .items
            .items
            .into_iter()
            .find(|track| track.name() == selected_vid_str)
            .unwrap();
        Self { video: vid }
    }
}
