//! Sidebar for browsing downloaded files in the output directory.
//!
//! Toggled with Shift+D. Lists files, allows selection with arrow keys,
//! and launches the selected file with MPV on Enter.

use crate::config::Theme;
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use std::path::{Path, PathBuf};

/// State for the download files sidebar.
pub struct Sidebar {
    /// Whether the sidebar is currently visible.
    pub open: bool,
    /// Files found in the output directory.
    files: Vec<PathBuf>,
    /// Current selection index.
    pub state: ListState,
    /// A delete confirmation (`y/n`) is pending for the selected file.
    pub confirm_delete: bool,
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            open: false,
            files: Vec::new(),
            state: ListState::default(),
            confirm_delete: false,
        }
    }

    /// Toggle the sidebar open/closed. Refreshes the file list on open.
    pub fn toggle(&mut self, output_dir: &Path) {
        self.open = !self.open;
        self.confirm_delete = false;
        if self.open {
            self.refresh(output_dir);
        }
    }

    /// Scan the output directory for files.
    pub fn refresh(&mut self, output_dir: &Path) {
        self.files.clear();
        if let Ok(entries) = std::fs::read_dir(output_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.files.push(path);
                }
            }
        }
        self.files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        if !self.files.is_empty() {
            self.state.select(Some(0));
        }
    }

    /// Move selection up.
    pub fn up(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let i = self.state.selected().map_or(0, |i| {
            if i == 0 { self.files.len() - 1 } else { i - 1 }
        });
        self.state.select(Some(i));
    }

    /// Move selection down.
    pub fn down(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let i = self.state.selected().map_or(0, |i| {
            if i + 1 >= self.files.len() { 0 } else { i + 1 }
        });
        self.state.select(Some(i));
    }

    /// Get the currently selected file path.
    pub fn selected(&self) -> Option<&Path> {
        self.state.selected().and_then(|i| self.files.get(i).map(|p| p.as_path()))
    }

    /// Render the sidebar into the given area.
    pub fn render(&mut self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let items: Vec<ListItem> = self
            .files
            .iter()
            .map(|p| {
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                ListItem::new(Line::from(name))
            })
            .collect();

        let footer = if self.confirm_delete {
            match self.selected() {
                Some(path) => format!(
                    "Delete '{}'? [y/n]",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                None => "Nothing selected".to_string(),
            }
        } else {
            "[Enter] Play | [p] Queue | [d] Delete | [r] Refresh".to_string()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Downloads ")
                    .title_bottom(footer)
                    .title_alignment(HorizontalAlignment::Center)
                    .borders(Borders::ALL)
                    .style(Style::default().fg(theme.sidebar_fg()).bg(theme.sidebar_bg())),
            )
            .highlight_style(
                Style::default()
                    .fg(theme.sidebar_highlight_fg())
                    .bg(theme.sidebar_highlight_bg())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        f.render_stateful_widget(list, area, &mut self.state);
    }
}
