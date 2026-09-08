//! Application theme configuration.
//!
//! Loads from `~/.config/ytrs/theme.toml`. Falls back to defaults if missing.
//! Colors accept named ratatui colors ("blue", "yellow") or hex ("#FF0000").
//!
//! File format: a `preset` key selects a built-in base (`default`,
//! `groovebox`, `tokyonight`, `dracula`, `nord`); any color set under
//! `[player]` / `[sidebar]` overrides the preset.
//!
//! ```toml
//! preset = "tokyonight"
//! [player]
//! gauge_fill = "#ff9e64"
//! ```

use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_preset() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Built-in base theme. Unknown names fall back to `default`.
    #[serde(default = "default_preset")]
    pub preset: String,
    #[serde(default)]
    pub player: PlayerTheme,
    #[serde(default)]
    pub sidebar: SidebarTheme,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerTheme {
    /// Background color for the info panel block.
    #[serde(default)]
    pub bg: String,
    /// Foreground/text color for the info panel block.
    #[serde(default)]
    pub fg: String,
    /// Gauge fill color.
    #[serde(default)]
    pub gauge_fill: String,
    /// Gauge background (track) color.
    #[serde(default)]
    pub gauge_bg: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SidebarTheme {
    /// Background color for the sidebar block.
    #[serde(default)]
    pub bg: String,
    /// Foreground/text color for list items.
    #[serde(default)]
    pub fg: String,
    /// Background of the highlighted/focused item.
    #[serde(default)]
    pub highlight_bg: String,
    /// Foreground of the highlighted/focused item.
    #[serde(default)]
    pub highlight_fg: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self::preset("default")
    }
}

impl Theme {
    /// Built-in preset names, in cycle order.
    pub fn preset_names() -> Vec<&'static str> {
        vec!["default", "groovebox", "tokyonight", "dracula", "nord"]
    }

    /// Full built-in theme (unknown names fall back to `default`).
    pub fn preset(name: &str) -> Self {
        let (player, sidebar) = match name {
            // Gruvbox dark.
            "groovebox" => (
                PlayerTheme {
                    bg: "#282828".into(),
                    fg: "#ebdbb2".into(),
                    gauge_fill: "#fabd2f".into(),
                    gauge_bg: "#3c3836".into(),
                },
                SidebarTheme {
                    bg: "#282828".into(),
                    fg: "#ebdbb2".into(),
                    highlight_bg: "#fabd2f".into(),
                    highlight_fg: "#282828".into(),
                },
            ),
            // Tokyo Night.
            "tokyonight" => (
                PlayerTheme {
                    bg: "#1a1b26".into(),
                    fg: "#c0caf5".into(),
                    gauge_fill: "#7aa2f7".into(),
                    gauge_bg: "#24283b".into(),
                },
                SidebarTheme {
                    bg: "#1a1b26".into(),
                    fg: "#c0caf5".into(),
                    highlight_bg: "#7aa2f7".into(),
                    highlight_fg: "#1a1b26".into(),
                },
            ),
            // Dracula.
            "dracula" => (
                PlayerTheme {
                    bg: "#282a36".into(),
                    fg: "#f8f8f2".into(),
                    gauge_fill: "#bd93f9".into(),
                    gauge_bg: "#44475a".into(),
                },
                SidebarTheme {
                    bg: "#282a36".into(),
                    fg: "#f8f8f2".into(),
                    highlight_bg: "#bd93f9".into(),
                    highlight_fg: "#282a36".into(),
                },
            ),
            // Nord Polar Night.
            "nord" => (
                PlayerTheme {
                    bg: "#2e3440".into(),
                    fg: "#eceff4".into(),
                    gauge_fill: "#88c0d0".into(),
                    gauge_bg: "#3b4252".into(),
                },
                SidebarTheme {
                    bg: "#2e3440".into(),
                    fg: "#eceff4".into(),
                    highlight_bg: "#88c0d0".into(),
                    highlight_fg: "#2e3440".into(),
                },
            ),
            // Historical default (also used for unknown names).
            _ => (
                PlayerTheme {
                    bg: "blue".into(),
                    fg: "yellow".into(),
                    gauge_fill: "yellow".into(),
                    gauge_bg: "blue".into(),
                },
                SidebarTheme {
                    bg: "blue".into(),
                    fg: "yellow".into(),
                    highlight_bg: "cyan".into(),
                    highlight_fg: "red".into(),
                },
            ),
        };
        Self {
            preset: name.to_string(),
            player,
            sidebar,
        }
    }

    /// Next preset name after `current` (wraps around).
    pub fn next_preset_name(current: &str) -> &'static str {
        let names = Self::preset_names();
        let idx = names.iter().position(|n| *n == current).unwrap_or(0);
        names[(idx + 1) % names.len()]
    }

    /// Overlay non-empty file colors over `base`.
    fn overlay(base: &mut Theme, file: Theme) {
        if !file.player.bg.is_empty() {
            base.player.bg = file.player.bg;
        }
        if !file.player.fg.is_empty() {
            base.player.fg = file.player.fg;
        }
        if !file.player.gauge_fill.is_empty() {
            base.player.gauge_fill = file.player.gauge_fill;
        }
        if !file.player.gauge_bg.is_empty() {
            base.player.gauge_bg = file.player.gauge_bg;
        }
        if !file.sidebar.bg.is_empty() {
            base.sidebar.bg = file.sidebar.bg;
        }
        if !file.sidebar.fg.is_empty() {
            base.sidebar.fg = file.sidebar.fg;
        }
        if !file.sidebar.highlight_bg.is_empty() {
            base.sidebar.highlight_bg = file.sidebar.highlight_bg;
        }
        if !file.sidebar.highlight_fg.is_empty() {
            base.sidebar.highlight_fg = file.sidebar.highlight_fg;
        }
    }

    /// Load theme from `~/.config/ytrs/theme.toml`. Creates the file with defaults if missing.
    pub fn load() -> Self {
        let path = config_dir();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str::<Theme>(&contents) {
                    Ok(file) => {
                        let mut theme = if Self::preset_names().contains(&file.preset.as_str()) {
                            Self::preset(&file.preset)
                        } else {
                            eprintln!(
                                "Unknown theme preset '{}', using default.",
                                file.preset
                            );
                            Self::preset("default")
                        };
                        Self::overlay(&mut theme, file);
                        return theme;
                    }
                    Err(e) => {
                        eprintln!("Failed to parse theme.toml: {e}. Using defaults.");
                    }
                },
                Err(e) => {
                    eprintln!("Failed to read theme.toml: {e}. Using defaults.");
                }
            }
        }
        let theme = Theme::default();
        theme.save();
        theme
    }

    pub fn save(&self) {
        let path = config_dir();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(contents) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, contents);
        }
    }

    // -- Player helpers --

    pub fn player_bg(&self) -> Color {
        parse_color(&self.player.bg)
    }
    pub fn player_fg(&self) -> Color {
        parse_color(&self.player.fg)
    }
    pub fn gauge_fill(&self) -> Color {
        parse_color(&self.player.gauge_fill)
    }
    pub fn gauge_bg(&self) -> Color {
        parse_color(&self.player.gauge_bg)
    }

    // -- Sidebar helpers --

    pub fn sidebar_bg(&self) -> Color {
        parse_color(&self.sidebar.bg)
    }
    pub fn sidebar_fg(&self) -> Color {
        parse_color(&self.sidebar.fg)
    }
    pub fn sidebar_highlight_bg(&self) -> Color {
        parse_color(&self.sidebar.highlight_bg)
    }
    pub fn sidebar_highlight_fg(&self) -> Color {
        parse_color(&self.sidebar.highlight_fg)
    }
}

fn config_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        PathBuf::from(env!("USERPROFILE"))
            .join(".config")
            .join("ytrs")
            .join("theme.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("ytrs")
            .join("theme.toml")
    } else {
        PathBuf::from("theme.toml")
    }
}

/// Parse a color string into a ratatui `Color`.
/// Supports named colors ("blue", "yellow", "red", "cyan", "green", "magenta", "white", "black",
/// "gray", "dark_gray", "light_blue", "light_yellow", "light_red", "light_cyan", "light_green",
/// "light_magenta") and hex ("#RRGGBB" or "#RRGGBBAA").
fn parse_color(s: &str) -> Color {
    let s = s.trim();
    // Hex
    if s.starts_with('#') {
        let hex = &s[1..];
        match hex.len() {
            6 => {
                if let Ok(r) = u8::from_str_radix(&hex[0..2], 16)
                    && let Ok(g) = u8::from_str_radix(&hex[2..4], 16)
                    && let Ok(b) = u8::from_str_radix(&hex[4..6], 16)
                {
                    return Color::Rgb(r, g, b);
                }
            }
            8 => {
                if let Ok(r) = u8::from_str_radix(&hex[0..2], 16)
                    && let Ok(g) = u8::from_str_radix(&hex[2..4], 16)
                    && let Ok(b) = u8::from_str_radix(&hex[4..6], 16)
                {
                    return Color::Rgb(r, g, b);
                }
            }
            _ => {}
        }
        return Color::Reset;
    }
    // Named
    match s.to_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "dark_grey" => Color::DarkGray,
        "light_red" => Color::LightRed,
        "light_green" => Color::LightGreen,
        "light_yellow" => Color::LightYellow,
        "light_blue" => Color::LightBlue,
        "light_magenta" => Color::LightMagenta,
        "light_cyan" => Color::LightCyan,
        _ => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_and_fallback() {
        assert_eq!(Theme::preset("tokyonight").player.bg, "#1a1b26");
        assert_eq!(Theme::preset("groovebox").player.gauge_fill, "#fabd2f");
        // Unknown names fall back to default (and keep the requested name out).
        let fallback = Theme::preset("nope");
        assert_eq!(fallback.player.bg, Theme::preset("default").player.bg);
    }

    #[test]
    fn preset_names_cycle() {
        let names = Theme::preset_names();
        assert!(names.contains(&"default"));
        assert!(names.contains(&"groovebox"));
        assert!(names.contains(&"tokyonight"));
        assert_eq!(Theme::next_preset_name("default"), names[1]);
        assert_eq!(
            Theme::next_preset_name(names[names.len() - 1]),
            names[0]
        );
        assert_eq!(Theme::next_preset_name("unknown"), names[1]);
    }

    #[test]
    fn file_overrides_preset_partially() {
        let file: Theme = toml::from_str(
            r##"
            preset = "tokyonight"
            [player]
            gauge_fill = "#ff9e64"
            "##,
        )
        .unwrap();
        let mut theme = Theme::preset(&file.preset);
        Theme::overlay(&mut theme, file);
        // Override applied…
        assert_eq!(theme.player.gauge_fill, "#ff9e64");
        // …preset kept everywhere else.
        assert_eq!(theme.player.bg, "#1a1b26");
        assert_eq!(theme.sidebar.highlight_bg, "#7aa2f7");
    }

    #[test]
    fn empty_file_means_defaults() {
        let file: Theme = toml::from_str("").unwrap();
        assert_eq!(file.preset, "default");
        let mut theme = Theme::preset(&file.preset);
        Theme::overlay(&mut theme, file);
        assert_eq!(theme.player.bg, Theme::preset("default").player.bg);
    }
}
