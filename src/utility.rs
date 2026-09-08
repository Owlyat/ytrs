use std::path::PathBuf;

pub fn format_time(d: u32) -> impl std::fmt::Display {
    let hours = d / 3600;
    let minutes = (d % 3600) / 60;
    let secs = d % 60;
    let hours_str = if hours > 0 {
        format!("{hours:02}:")
    } else {
        "".to_owned()
    };
    let minutes = if minutes > 0 || hours > 0 {
        format!("{minutes:02}:")
    } else {
        "".to_owned()
    };
    format!("[{}{}{secs:02}]", hours_str, minutes)
}

/// Home directory resolved at runtime: `%USERPROFILE%` on Windows,
/// `$HOME` on Linux/macOS. `None` when the variable is missing
/// (unlike `env!`, this never breaks compilation on other platforms).
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let var = "USERPROFILE";
    #[cfg(not(target_os = "windows"))]
    let var = "HOME";
    std::env::var(var).ok().map(PathBuf::from)
}
