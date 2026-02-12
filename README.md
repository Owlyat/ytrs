# YTRS
### Requirement
[MPV](https://mpv.io/installation/) needs to be installed.
### Info
[yt-dlp](https://github.com/yt-dlp/yt-dlp) / [ffmpeg](https://ffmpeg.org/) automatically install:
- Path for Windows:
  - `%USERPROFILE%\.config\ytrs\libs`
  - `%USERPROFILE%\.config\ytrs\output`
- Path for linux/macos:
  - `Home\.config\ytrs\libs`
  - `Home\.config\ytrs\output`
You can provide a flag in the cli to install/download where you want.
Libraries:
```
ytrs -l <Path>
```
Download:
```
ytrs -o <Path>
```
(This will always append a output directory to the path)
For more help:
```
ytrs -h
```


You might also want [Ollama](https://ollama.com/) for Summarizing Transcripts.

### Installation
To run the app you can clone the repo and:
```
cargo build --release
```
You can also use:
```
cargo install ytrs
```
