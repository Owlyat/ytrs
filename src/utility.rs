pub fn format_time(d: u32) -> impl std::fmt::Display {
    let hours = d / 3600;
    let minutes = (d % 3600) / 60;
    let secs = d % 60;
    let hours = if hours > 0 {
        format!("{hours:02}:")
    } else {
        "".to_owned()
    };
    let minutes = if minutes > 0 {
        format!("{minutes:02}:")
    } else {
        "".to_owned()
    };
    format!("[{}{}{secs:02}]", hours, minutes)
}
