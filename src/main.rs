use core::panic;
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, read},
    widgets::Paragraph,
};
use rustypipe::{
    client::RustyPipe,
    model::{SearchResult, TrackItem, VideoItem, traits::YtEntity},
};
use std::{path::PathBuf, thread};
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
        yt_query: YtMusicQuery,
    },
    Watch {
        yt_query: YTQuery,
    },
    Listen {
        yt_query: YtMusicQuery,
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
        loop {
            *self = Self::Watch {
                yt_query: YTQuery::from(inquire::Text::new("Query :").prompt().unwrap().as_str())
                    .await,
            };
            if let Self::Watch { yt_query } = self {
                if std::process::Command::new("mpv")
                    .args(["--version"])
                    .output()
                    .is_ok()
                {
                    std::process::Command::new("mpv")
                        .arg(format!(
                            "https://www.youtube.com/watch?v={}",
                            yt_query.video.id
                        ))
                        .output()
                        .unwrap();
                } else {
                    panic!("MPV not installed")
                }
            }
        }
    }
    async fn listen(&mut self) {
        loop {
            if let Ok(yt_query) = YtMusicQuery::new_music_search().await {
                *self = Self::Listen { yt_query }
            } else {
                break;
            }
            // Thread to run command
            let url = format!(
                "https://www.youtube.com/watch?v={}",
                self.get_id().clone().unwrap()
            );
            let handle = thread::spawn(move || {
                if std::process::Command::new("mpv")
                    .args(["--version"])
                    .output()
                    .is_ok()
                {
                    std::process::Command::new("mpv")
                        .args(["--no-video", url.as_str()])
                        .output()
                        .unwrap();
                } else {
                    panic!("MPV not installed")
                }
            });
            let mut term = ratatui::init();

            'playing: loop {
                if handle.is_finished() {
                    ratatui::restore();
                    break 'playing;
                } else {
                    term.draw(|f| {
                        f.render_widget(Paragraph::new("Press <q> to terminate"), f.area())
                    })
                    .unwrap();
                    if let Ok(Event::Key(KeyEvent { code, .. })) = read() {
                        if code == KeyCode::Char('q') {
                            ratatui::restore();
                            std::process::Command::new("Taskkill")
                                .args(["/f", "/im", "mpv.exe"])
                                .output()
                                .unwrap();
                            break 'playing;
                        }
                    }
                }
            }
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
                if let Ok(yt_query) = YtMusicQuery::new_music_search().await {
                    *self = Self::Ytdlp { yt_query };
                }
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

pub struct YtMusicQuery {
    video: TrackItem,
}
impl YtMusicQuery {
    async fn new_music_search() -> Result<Self, ()> {
        let search_term = inquire::Text::new("Youtube Search :")
            .prompt()
            .expect("No search term");
        let rp = RustyPipe::new();
        let found_videos = rp
            .query()
            .unauthenticated()
            .music_search_tracks(search_term)
            .await
            .unwrap();
        let mut found_videos_str: Vec<String> = found_videos
            .clone()
            .items
            .items
            .into_iter()
            .map(|x| x.name.to_string())
            .collect();
        found_videos_str.push("Exit".to_owned());
        let selected_vid_str = inquire::Select::new("Select Music", found_videos_str)
            .prompt()
            .unwrap();
        if let Some(vid) = found_videos
            .items
            .items
            .into_iter()
            .find(|track| track.name() == selected_vid_str)
        {
            Ok(Self { video: vid })
        } else {
            Err(())
        }
    }
}

pub struct YTQuery {
    video: VideoItem,
}

impl YTQuery {
    pub async fn from(query: &str) -> Self {
        let found_videos: SearchResult<VideoItem> = RustyPipe::new()
            .query()
            .unauthenticated()
            .search(query)
            .await
            .expect("Error");

        let mut videos: Vec<String> = found_videos
            .items
            .items
            .iter()
            .map(|v| format!("➡️ {}", v.name))
            .collect();
        videos.push("Exit".to_owned());
        if let Ok(video_name) = inquire::Select::new("Select video to watch", videos).prompt() {
            let selected_vid = found_videos
                .items
                .items
                .into_iter()
                .find(|v| video_name.contains(&v.name));
            if let Some(vid) = selected_vid {
                return Self { video: vid };
            }
        } else {
            // Error
        }
        panic!()
    }
}
