//! Playfile queue sidebar.
//!
//! Toggled with Shift+P. Press `p` on a search result (empty input) to append
//! it; the first appended entry starts playing automatically.
//! While open: `j/k` select, `d` removes, `Shift+J/K` reorders,
//! `Enter` plays, `Esc` closes.

use crate::app::{TrackInfo, VideoInfo, YoutubeResponse};
use crate::config::Theme;
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::widgets::{Block, List, ListItem, ListState};
use std::path::Path;

/// A queued entry: a YouTube stream or a local file path.
#[derive(Clone)]
pub enum PlaylistItem {
    Stream(YoutubeResponse),
    File(String),
}

impl PlaylistItem {
    /// Stable key used to match what's playing with the queue.
    pub fn id(&self) -> String {
        match self {
            Self::Stream(res) => res.get_id(),
            Self::File(path) => Self::file_id(path),
        }
    }

    pub fn file_id(path: &str) -> String {
        format!("file:{path}")
    }

    fn label(&self) -> String {
        match self {
            Self::Stream(res) => match res {
                YoutubeResponse::Video(v) => VideoInfo::from(v).to_string(),
                YoutubeResponse::Track(t) => TrackInfo::from(t).to_string(),
            },
            Self::File(path) => Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone()),
        }
    }
}

/// In-player play queue.
#[derive(Default)]
pub struct Playlist {
    /// Whether the sidebar is currently visible.
    pub open: bool,
    items: Vec<PlaylistItem>,
    selected: usize,
}

impl Playlist {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Append an entry. Selection stays where it is.
    pub fn add(&mut self, item: PlaylistItem) {
        self.items.push(item);
    }

    /// Currently selected entry, if any.
    pub fn current(&self) -> Option<&PlaylistItem> {
        self.items.get(self.selected)
    }

    /// Entry at `idx`, if any.
    pub fn get(&self, idx: usize) -> Option<&PlaylistItem> {
        self.items.get(idx)
    }

    /// Position of the entry with this id (`PlaylistItem::id`), if queued.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|item| item.id() == id)
    }

    /// Move selection down (clamped).
    pub fn select_next(&mut self) {
        if !self.items.is_empty() && self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    /// Move selection up (clamped).
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Remove the selected entry and keep the selection in bounds.
    pub fn remove_selected(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.items.remove(self.selected.min(self.items.len() - 1));
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }

    /// Move the selected entry by `delta` (-1 up, +1 down), following it.
    pub fn move_selected(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as i32;
        let from = (self.selected as i32).clamp(0, len - 1);
        let to = (from + delta).clamp(0, len - 1) as usize;
        self.items.swap(from as usize, to);
        self.selected = to;
    }

    fn label(item: &PlaylistItem) -> String {
        item.label()
    }

    /// Render the sidebar into the given area.
    pub fn render(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut state = ListState::default();
        if !self.items.is_empty() {
            state.select(Some(self.selected.min(self.items.len() - 1)));
        }
        let list = if self.items.is_empty() {
            List::new(vec![ListItem::from(
                "(empty — press p on a search result)",
            )])
        } else {
            List::new(
                self.items
                    .iter()
                    .map(|item| ListItem::from(Self::label(item)))
                    .collect::<Vec<_>>(),
            )
        }
        .block(
            Block::bordered()
                .title_top(format!("Playlist ({})", self.items.len()))
                .title_alignment(HorizontalAlignment::Center)
                .title_bottom("[j/k] Select | [d] Remove | [J/K] Move | [Enter] Play | loop ∞ | (Esc/Sh+P) Close")
                .title_alignment(HorizontalAlignment::Center)
                .style(
                    Style::default()
                        .fg(theme.sidebar_fg())
                        .bg(theme.sidebar_bg()),
                ),
        )
        .highlight_symbol(">")
        .highlight_style(
            Style::default()
                .fg(theme.sidebar_highlight_fg())
                .bg(theme.sidebar_highlight_bg()),
        );
        f.render_stateful_widget(list, area, &mut state);
    }
}
