//! Player TUI widgets, rendered according to [`Screen`] state.
//!
//! Each widget owns exactly what it needs — no `&mut YoutubeRs` —
//! so `draw` in `app.rs` stays a thin state dispatch.

use std::path::Path;

use chrono::{Timelike, Utc};
use lofty::file::{AudioFile, TaggedFile};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Gauge, List, ListItem, ListState, Paragraph};
use ratatui_image::StatefulImage;
use tracing::warn;

use crate::app::{YoutubeAPI, YoutubeResponse};
use crate::config::Theme;
use crate::playlist::Playlist;
use crate::sidebar::Sidebar;
use crate::utility::format_time;

/// Which screen the player TUI must show.
pub enum Screen {
    /// MPV hasn't started yet.
    Loading,
    /// MPV is running; the bottom panel depends on [`ActiveView`].
    Active(ActiveView),
}

/// Bottom panel shown while MPV is running.
pub enum ActiveView {
    Player,
    Search,
    Transcript,
    Setup,
    Suggestion,
}

impl Screen {
    /// Overlay priority: setup > transcript > suggestion > playlist > sidebar > search > player.
    pub fn from_ctx(ctx: &DrawCtx<'_>) -> Self {
        if !ctx.playback.started {
            Self::Loading
        } else if ctx.setup.open {
            Self::Active(ActiveView::Setup)
        } else if ctx.transcript.open {
            Self::Active(ActiveView::Transcript)
        } else if ctx.suggestion.open {
            Self::Active(ActiveView::Suggestion)
        } else if ctx.search_open {
            Self::Active(ActiveView::Search)
        } else {
            Self::Active(ActiveView::Player)
        }
    }
}

/// Live playback progress + MPV state.
#[derive(Debug, Clone, Copy)]
pub struct Playback {
    pub time: f64,
    pub started: bool,
    pub volume: f64,
}

/// What the player panel must describe.
#[derive(Clone, Copy)]
pub enum Media<'a> {
    /// Streamed Youtube response.
    Stream(&'a YoutubeResponse),
    /// Local file: parsed tags + display path.
    File(&'a TaggedFile, &'a str),
    /// Player opened with no media yet.
    Empty,
    /// Inconsistent state (neither response, file, nor empty flag).
    Missing,
}

impl<'a> Media<'a> {
    pub fn from_parts(
        response: &'a Option<YoutubeResponse>,
        file: &'a Option<(TaggedFile, String)>,
        empty_player: bool,
    ) -> Self {
        if let Some(res) = response {
            Self::Stream(res)
        } else if let Some((tagged, name)) = file {
            Self::File(tagged, name)
        } else if empty_player {
            Self::Empty
        } else {
            Self::Missing
        }
    }
}

/// Loading spinner state for the MPV boot screen.
#[derive(Debug)]
pub struct Loader {
    frames: [&'static str; 4],
    idx: usize,
}

impl Default for Loader {
    fn default() -> Self {
        Self {
            frames: ["/", "|", "\\", "-"],
            idx: 0,
        }
    }
}

impl Loader {
    pub fn tick(&mut self) {
        if Utc::now().second().is_multiple_of(2) {
            self.idx = self.idx.saturating_add(1) % self.frames.len();
        }
    }

    pub fn current(&self) -> &str {
        self.frames[self.idx]
    }
}

/// Everything [`draw_screen`] needs for one frame.
pub struct DrawCtx<'a> {
    pub playback: Playback,
    pub loader: &'a mut Loader,
    pub search_open: bool,
    pub search: SearchView<'a>,
    pub transcript: &'a TranscriptState,
    pub setup: &'a SetupState,
    pub suggestion: &'a mut SuggestionState,
    pub playlist: &'a Playlist,
    pub media: Media<'a>,
    pub artwork: &'a mut Option<ratatui_image::protocol::StatefulProtocol>,
    pub theme: &'a Theme,
    pub sidebar: &'a mut Sidebar,
}

/// Top-level dispatch: exactly one widget tree per [`Screen`] state.
pub fn draw_screen(ctx: &mut DrawCtx<'_>, f: &mut Frame<'_>) {
    match Screen::from_ctx(ctx) {
        Screen::Loading => render_loading(f, ctx.loader),
        Screen::Active(view) => {
            let (main_area, sidebar_area) =
                split_shell(f.area(), ctx.sidebar.open || ctx.playlist.open);
            let [artwork_area, panel_area] = split_player(main_area);
            render_artwork(f, artwork_area, ctx.artwork);
            match view {
                ActiveView::Search => ctx.search.render(f, panel_area, ctx.theme),
                ActiveView::Transcript => ctx.transcript.render(f, panel_area, ctx.theme),
                ActiveView::Setup => ctx.setup.render(f, panel_area, ctx.theme),
                ActiveView::Suggestion => ctx.suggestion.render(f, panel_area, ctx.theme),
                ActiveView::Player => PlayerView {
                    media: ctx.media,
                    playback_time: ctx.playback.time,
                    volume: ctx.playback.volume,
                    theme: ctx.theme,
                }
                .render(f, panel_area),
            }
            // Playlist sidebar takes the slot when open, else the download sidebar.
            if ctx.playlist.open {
                if let Some(area) = sidebar_area {
                    ctx.playlist.render(f, area, ctx.theme);
                }
            } else {
                render_sidebar_overlay(f, ctx.sidebar, sidebar_area, ctx.theme);
            }
        }
    }
}

/// Shell layout: main area + optional 25% sidebar strip on the right.
pub fn split_shell(area: Rect, sidebar_open: bool) -> (Rect, Option<Rect>) {
    if sidebar_open {
        let split = Layout::horizontal([Constraint::Min(60), Constraint::Percentage(25)])
            .split(area);
        (split[0], Some(split[1]))
    } else {
        (area, None)
    }
}

/// Player layout: artwork on top (60%), info panel below (40%).
pub fn split_player(area: Rect) -> [Rect; 2] {
    let chunks =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);
    [chunks[0], chunks[1]]
}

/// Sidebar overlay rendered on top of the active view, if open.
pub fn render_sidebar_overlay(
    f: &mut Frame<'_>,
    sidebar: &mut Sidebar,
    area: Option<Rect>,
    theme: &Theme,
) {
    if sidebar.open && let Some(area) = area {
        sidebar.render(f, area, theme);
    }
}

/// Loading spinner shown while MPV boots.
pub fn render_loading(f: &mut Frame<'_>, loader: &mut Loader) {
    loader.tick();
    Block::bordered()
        .title(format!("[Loading MPV {}]", loader.current()))
        .render(f.area(), f.buffer_mut());
}

/// Centered album art / thumbnail shared by the [`ActiveView`] screens.
pub fn render_artwork(
    f: &mut Frame<'_>,
    area: Rect,
    img: &mut Option<ratatui_image::protocol::StatefulProtocol>,
) {
    if let Some(protocol) = img {
        // Size of the image once resized to the area to fit
        let img_size = protocol.size_for(
            ratatui_image::Resize::Scale(None),
            Size::new(area.width, area.height),
        );
        // Clamp to layout bounds so image never overflows into info panel
        let img_size = Size::new(
            img_size.width.min(area.width),
            img_size.height.min(area.height),
        );
        let img_place = Rect::new(
            area.x + (area.width.saturating_sub(img_size.width)) / 2,
            area.y + (area.height.saturating_sub(img_size.height)) / 2,
            img_size.width,
            img_size.height,
        );
        f.render_stateful_widget(
            StatefulImage::default().resize(ratatui_image::Resize::Scale(None)),
            img_place,
            protocol,
        );
        if let Some(x) = protocol.last_encoding_result()
            && let Err(e) = x
        {
            panic!("Error with last encoding result for image '{e}'");
        }
    }
}

/// Youtube search popup (query input + results list).
pub struct SearchView<'a> {
    pub results: &'a [(String, YoutubeResponse)],
    pub selected: &'a mut ListState,
    pub query: &'a str,
    pub api: Option<YoutubeAPI>,
    /// A background search is running (list area shows a loading status).
    pub searching: bool,
    /// Last search error to display when there are no results.
    pub notice: &'a str,
}

impl SearchView<'_> {
    pub fn render(&mut self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let areas = Layout::vertical([Constraint::Length(3), Constraint::Fill(3)]).split(area);
        Paragraph::new(format!("YTSearch: {}", self.query))
            .block(
                Block::bordered()
                    .title_top("Search")
                    .title_alignment(HorizontalAlignment::Center)
                    .style(
                        Style::default()
                            .fg(theme.player_fg())
                            .bg(theme.player_bg()),
                    ),
            )
            .render(areas[0], f.buffer_mut());
        if self.results.is_empty() {
            let status = if self.searching {
                "Searching… (Esc to cancel)".to_string()
            } else if !self.notice.is_empty() {
                self.notice.to_string()
            } else {
                "Type a query and press Enter".to_string()
            };
            Paragraph::new(status)
                .block(
                    Block::bordered()
                        .title_bottom(
                            format!("[▼▲ Select | (Enter) Search/Play | (p) +Playlist | (Tab) Source: {} | (Esc) Player]",self.api.unwrap_or_default()),
                        )
                        .style(
                            Style::default()
                                .fg(theme.player_fg())
                                .bg(theme.player_bg()),
                        ),
                )
                .render(areas[1], f.buffer_mut());
            return;
        }
        let list = List::new(
            self.results
                .iter()
                .map(|v| ListItem::from(v.0.clone()))
                .collect::<Vec<ListItem>>(),
        )
        .block(
            Block::bordered()
                .title_bottom(
                    format!("[▼▲ Select | (Enter) Search/Play | (p) +Playlist | (Tab) Source: {} | (Esc) Player]",self.api.unwrap_or_default()),
                )
                .style(
                    Style::default()
                        .fg(theme.player_fg())
                        .bg(theme.player_bg()),
                ),
        )
        .highlight_symbol(">")
        .highlight_style(
            Style::default()
                .fg(theme.sidebar_highlight_fg())
                .bg(theme.sidebar_highlight_bg()),
        )
        .direction(ratatui::widgets::ListDirection::TopToBottom);
        f.render_stateful_widget(list, areas[1], self.selected);
    }
}

/// Columns in the suggestion grid.
pub const SUGGEST_COLS: usize = 2;
/// Card geometry in cells (thumbnail box + 2 title lines + borders).
const SUGGEST_THUMB_H: u16 = 8;
const SUGGEST_TITLE_H: u16 = 2;
const SUGGEST_CARD_H: u16 = SUGGEST_THUMB_H + SUGGEST_TITLE_H + 2;

/// 2D grid movement over a row-major list. Returns the clamped index.
pub fn grid_move(selected: usize, len: usize, dx: i32, dy: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let cols = SUGGEST_COLS;
    let sel = selected.min(len - 1);
    let (mut row, mut col) = (sel / cols, sel % cols);
    if dx != 0 {
        let row_len = if row == (len - 1) / cols {
            (len - 1) % cols + 1
        } else {
            cols
        };
        col = (col as i32 + dx).clamp(0, row_len as i32 - 1) as usize;
    }
    if dy != 0 {
        let max_row = (len - 1) / cols;
        row = (row as i32 + dy).clamp(0, max_row as i32) as usize;
        let row_len = if row == max_row {
            (len - 1) % cols + 1
        } else {
            cols
        };
        col = col.min(row_len - 1);
    }
    row * cols + col
}

/// Suggestion screen state: related videos/tracks for the current item.
#[derive(Default)]
pub struct SuggestionState {
    pub open: bool,
    pub api: Option<YoutubeAPI>,
    pub title: String,
    pub items: Vec<(String, YoutubeResponse)>,
    pub selected: usize,
    /// First visible grid row.
    pub scrolltop: usize,
    /// Thumbnail protocols by video id, filled in the background.
    pub thumbs: std::collections::HashMap<
        String,
        ratatui_image::protocol::StatefulProtocol,
    >,
    pub loader: Loader,
    /// A fetch (initial or next page) is in flight.
    pub loading: bool,
    pub notice: String,
}

impl SuggestionState {
    fn card_block(selected: bool, theme: &Theme) -> Block<'static> {
        let block = Block::bordered();
        if selected {
            block.style(
                Style::default()
                    .fg(theme.sidebar_highlight_fg())
                    .bg(theme.sidebar_highlight_bg()),
            )
        } else {
            block.style(Style::default().fg(theme.player_fg()).bg(theme.player_bg()))
        }
    }

    pub fn render(&mut self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if self.loading {
            self.loader.tick();
        }
        let mut footer = String::from(
            "[(hjkl/arrows) Move | (Enter) Play | (p) +Playlist | (Tab) Music/Video | (s/Esc) Close]",
        );
        if self.loading && !self.items.is_empty() {
            footer = format!("{footer} Loading {}…", self.loader.current());
        }
        let outer = Block::bordered()
            .title_top(format!(
                "Suggestions ({}): {}",
                self.api.unwrap_or_default(),
                self.title
            ))
            .title_alignment(HorizontalAlignment::Center)
            .title_bottom(footer)
            .title_alignment(HorizontalAlignment::Center)
            .style(Style::default().fg(theme.player_fg()).bg(theme.player_bg()));
        let inner = outer.inner(area);
        outer.render(area, f.buffer_mut());

        if self.items.is_empty() {
            let status = if self.loading {
                format!("Loading… (Esc to cancel)")
            } else if !self.notice.is_empty() {
                self.notice.clone()
            } else {
                "No suggestions".to_string()
            };
            Paragraph::new(status).render(inner, f.buffer_mut());
            return;
        }

        let card_h = SUGGEST_CARD_H;
        let rows_visible = (inner.height / card_h.max(1)).max(1) as usize;
        let sel_row = self.selected / SUGGEST_COLS;
        if sel_row < self.scrolltop {
            self.scrolltop = sel_row;
        } else if sel_row >= self.scrolltop + rows_visible {
            self.scrolltop = sel_row - rows_visible + 1;
        }
        let col_w = inner.width / SUGGEST_COLS as u16;
        for r in 0..rows_visible {
            let row = self.scrolltop + r;
            for c in 0..SUGGEST_COLS {
                let idx = row * SUGGEST_COLS + c;
                if idx >= self.items.len() {
                    break;
                }
                let x = inner.x + c as u16 * col_w;
                let w = if c + 1 == SUGGEST_COLS {
                    inner.width.saturating_sub(c as u16 * col_w)
                } else {
                    col_w
                };
                let card = Rect::new(x, inner.y + r as u16 * card_h, w, card_h);
                let selected = idx == self.selected;
                Self::card_block(selected, theme).render(card, f.buffer_mut());
                let msgs = card.inner(Margin {
                    horizontal: 1,
                    vertical: 1,
                });
                if msgs.height == 0 || msgs.width == 0 {
                    continue;
                }
                let thumb_h = SUGGEST_THUMB_H.min(msgs.height);
                let thumb_area = Rect::new(msgs.x, msgs.y, msgs.width, thumb_h);
                let id = self.items[idx].1.get_id();
                if let Some(protocol) = self.thumbs.get_mut(&id) {
                    f.render_stateful_widget(
                        StatefulImage::default()
                            .resize(ratatui_image::Resize::Scale(None)),
                        thumb_area,
                        protocol,
                    );
                } else {
                    Paragraph::new("…").render(thumb_area, f.buffer_mut());
                }
                let title_y = msgs.y + thumb_h;
                if title_y < msgs.y + msgs.height {
                    let title_area = Rect::new(
                        msgs.x,
                        title_y,
                        msgs.width,
                        msgs.y + msgs.height - title_y,
                    );
                    Paragraph::new(self.items[idx].0.clone())
                        .render(title_area, f.buffer_mut());
                }
            }
        }
    }
}

/// Transcript overlay: script lines on top, AI summary below.
#[derive(Default)]
pub struct TranscriptState {
    pub open: bool,
    pub title: String,
    pub lang: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub summary: Vec<String>,
    // Track picking (shown first).
    pub picking: bool,
    pub tracks: Vec<TranscriptTrack>,
    pub selected: Option<TranscriptTrack>,
    pub filter: String,
    pub filtering: bool,
    pub sel: usize,
    pub list_error: String,
}

/// One available transcript: manual subtitles or auto-generated captions.
#[derive(Clone, Default)]
pub struct TranscriptTrack {
    pub lang: String,
    pub manual: bool,
}

impl TranscriptTrack {
    pub fn label(&self) -> String {
        format!(
            "{} — {}",
            self.lang,
            if self.manual {
                "subtitles"
            } else {
                "auto-generated"
            }
        )
    }
}

impl TranscriptState {
    /// Tracks matching the `/` filter (case-insensitive).
    pub fn visible_tracks(&self) -> Vec<&TranscriptTrack> {
        let q = self.filter.to_lowercase();
        self.tracks
            .iter()
            .filter(|t| q.is_empty() || t.label().to_lowercase().contains(&q))
            .collect()
    }

    pub fn render(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if self.picking {
            self.render_picker(f, area, theme);
        } else {
            self.render_reader(f, area, theme);
        }
    }

    fn render_picker(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let areas = Layout::vertical([Constraint::Length(3), Constraint::Fill(3)]).split(area);
        Paragraph::new(format!("/{}", self.filter))
            .block(
                Block::bordered()
                    .title_top(if self.filtering {
                        "Filter (typing…)"
                    } else {
                        "Filter (/ to edit)"
                    })
                    .title_alignment(HorizontalAlignment::Center)
                    .style(
                        Style::default()
                            .fg(theme.player_fg())
                            .bg(theme.player_bg()),
                    ),
            )
            .render(areas[0], f.buffer_mut());
        let panel = || {
            Block::bordered().style(
                Style::default()
                    .fg(theme.player_fg())
                    .bg(theme.player_bg()),
            )
        };
        if !self.list_error.is_empty() {
            Paragraph::new(self.list_error.clone())
                .block(
                    panel()
                        .title_top(format!("Transcripts: {}", self.title))
                        .title_alignment(HorizontalAlignment::Center)
                        .title_bottom("[(Esc) Close]")
                        .title_alignment(HorizontalAlignment::Center),
                )
                .render(areas[1], f.buffer_mut());
            return;
        }
        let visible = self.visible_tracks();
        if visible.is_empty() {
            Paragraph::new(if self.tracks.is_empty() {
                "No transcripts available for this video".to_string()
            } else {
                "No tracks match the filter".to_string()
            })
            .block(
                panel()
                    .title_top(format!("Transcripts: {}", self.title))
                    .title_alignment(HorizontalAlignment::Center)
                    .title_bottom("[(Esc) Back/Close]")
                    .title_alignment(HorizontalAlignment::Center),
            )
            .render(areas[1], f.buffer_mut());
            return;
        }
        let mut state = ListState::default();
        state.select(Some(self.sel.min(visible.len() - 1)));
        let list = List::new(
            visible
                .iter()
                .map(|t| ListItem::from(t.label()))
                .collect::<Vec<_>>(),
        )
        .block(
            panel()
                .title_top(format!("Transcripts: {} ({})", self.title, visible.len()))
                .title_alignment(HorizontalAlignment::Center)
                .title_bottom("[(▲▼/j/k) Select | (/) Filter | (Enter) Load | (Esc) Back/Close]")
                .title_alignment(HorizontalAlignment::Center),
        )
        .highlight_symbol(">")
        .highlight_style(
            Style::default()
                .fg(theme.sidebar_highlight_fg())
                .bg(theme.sidebar_highlight_bg()),
        );
        f.render_stateful_widget(list, areas[1], &mut state);
    }

    fn render_reader(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let chunks =
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);
        let header = if self.lang.is_empty() {
            format!("Transcript: {}", self.title)
        } else {
            format!("Transcript: {} [{}]", self.title, self.lang)
        };
        let panel = || {
            Block::bordered().style(
                Style::default()
                    .fg(theme.player_fg())
                    .bg(theme.player_bg()),
            )
        };
        // Paginated: only the visible window is joined/rendered, so very
        // long transcripts (tens of thousands of lines) stay cheap.
        // `scroll` is the top line index.
        let total = self.lines.len();
        let page = chunks[0].height.saturating_sub(2).max(1) as usize;
        let start = self.scroll.min(total.saturating_sub(1));
        let end = (start + page).min(total);
        let window = if total == 0 {
            String::new()
        } else {
            self.lines[start..end].join("\n")
        };
        Paragraph::new(window)
            .block(
                panel()
                    .title_top(format!(
                        "{header} [{}/{total}]",
                        if total == 0 { 0 } else { start + 1 }
                    ))
                    .title_alignment(HorizontalAlignment::Center)
                    .title_bottom("[(Esc) List | (▲▼/Home/End) Scroll | (r) Reload | (s) Summarize]")
                    .title_alignment(HorizontalAlignment::Center),
            )
            .render(chunks[0], f.buffer_mut());
        let summary_text = if self.summary.is_empty() {
            "Press 's' to summarize with AI (requires Ollama or llama.cpp).".to_string()
        } else {
            self.summary.join("\n")
        };
        Paragraph::new(summary_text)
            .block(
                panel()
                    .title_top("Summary")
                    .title_alignment(HorizontalAlignment::Center),
            )
            .render(chunks[1], f.buffer_mut());
    }
}

/// Focused column of the setup menu.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum SetupFocus {
    #[default]
    MidiIn,
    MidiOut,
    Audio,
    Playback,
}

impl SetupFocus {
    fn next(self) -> Self {
        match self {
            Self::MidiIn => Self::MidiOut,
            Self::MidiOut => Self::Audio,
            Self::Audio => Self::Playback,
            Self::Playback => Self::MidiIn,
        }
    }
}

/// Setup overlay: MIDI in/out ports + mpv audio output + playback mode.
#[derive(Default)]
pub struct SetupState {
    pub open: bool,
    pub focus: SetupFocus,
    pub midi_in: Vec<String>,
    pub midi_out: Vec<String>,
    pub midi_in_sel: usize,
    pub midi_out_sel: usize,
    pub audio_devices: Vec<String>,
    pub audio_sel: usize,
    pub play_modes: Vec<String>,
    pub play_sel: usize,
    pub notice: String,
}

impl SetupState {
    fn column(
        title: &str,
        items: &[String],
        selected: usize,
        focused: bool,
        area: Rect,
        f: &mut Frame<'_>,
        theme: &Theme,
    ) {
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(selected.min(items.len() - 1)));
        }
        let marker = if focused { "▶ " } else { "" };
        let list = List::new(
            items
                .iter()
                .map(|s| ListItem::from(s.as_str()))
                .collect::<Vec<_>>(),
        )
        .block(
            Block::bordered()
                .title_top(format!("{marker}{title}"))
                .title_alignment(HorizontalAlignment::Center)
                .style(if focused {
                    Style::default().fg(theme.player_fg()).bg(theme.player_bg())
                } else {
                    Style::default()
                }),
        )
        .highlight_symbol(">")
        .highlight_style(
            Style::default()
                .fg(theme.sidebar_highlight_fg())
                .bg(theme.sidebar_highlight_bg()),
        );
        f.render_stateful_widget(list, area, &mut state);
    }

    pub fn render(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let shell =
            Layout::vertical([Constraint::Fill(1), Constraint::Min(3)]).split(area);
        let columns =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(shell[0]);
        let left =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(columns[0]);
        let right =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(columns[1]);
        Self::column(
            "MIDI Input",
            &self.midi_in,
            self.midi_in_sel,
            self.focus == SetupFocus::MidiIn,
            left[0],
            f,
            theme,
        );
        Self::column(
            "MIDI Output",
            &self.midi_out,
            self.midi_out_sel,
            self.focus == SetupFocus::MidiOut,
            left[1],
            f,
            theme,
        );
        Self::column(
            "Audio Output",
            &self.audio_devices,
            self.audio_sel,
            self.focus == SetupFocus::Audio,
            right[0],
            f,
            theme,
        );
        Self::column(
            "Playback Mode",
            &self.play_modes,
            self.play_sel,
            self.focus == SetupFocus::Playback,
            right[1],
            f,
            theme,
        );
        Paragraph::new(format!(
            "[(Tab) Focus | (▲▼) Move | (Enter) Apply | (t) Theme | (Esc) Close] {}",
            self.notice
        ))
        .block(
            Block::bordered()
                .title_top("Setup")
                .style(
                    Style::default()
                        .fg(theme.player_fg())
                        .bg(theme.player_bg()),
                ),
        )
        .render(shell[1], f.buffer_mut());
    }

    pub fn advance_focus(&mut self) {
        self.focus = self.focus.next();
    }

    pub fn move_selection(&mut self, delta: i32) {
        let (len, sel) = match self.focus {
            SetupFocus::MidiIn => (self.midi_in.len(), &mut self.midi_in_sel),
            SetupFocus::MidiOut => (self.midi_out.len(), &mut self.midi_out_sel),
            SetupFocus::Audio => (self.audio_devices.len(), &mut self.audio_sel),
            SetupFocus::Playback => (self.play_modes.len(), &mut self.play_sel),
        };
        if len == 0 {
            return;
        }
        let next = *sel as i32 + delta;
        *sel = next.clamp(0, len as i32 - 1) as usize;
    }

    /// Currently selected playback mode as mpv `audio_only`.
    pub fn play_audio_only(&self) -> bool {
        self.play_sel == 0
    }
}

/// Bottom info panel: current stream, local file, or empty player + progress gauge.
pub struct PlayerView<'a> {
    pub media: Media<'a>,
    pub playback_time: f64,
    pub volume: f64,
    pub theme: &'a Theme,
}

impl PlayerView<'_> {
    pub fn render(self, f: &mut Frame<'_>, area: Rect) {
        let gauge_layout = gauge_rect(area);

        match self.media {
            Media::Stream(res) => {
                let duration = res.get_duration() as f64;
                let ratio = if duration > 0.0 {
                    (self.playback_time / duration).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                player_block(self.theme)
                    .title_top(format!(
                        "{} - {}:{}",
                        res.get_name(),
                        format_time(self.playback_time as u32),
                        format_time(res.get_duration()),
                    ))
                    .title_alignment(HorizontalAlignment::Center)
                    .title_top(format!("[Vol:{}]", self.volume))
                    .title_alignment(HorizontalAlignment::Right)
                    .title_bottom("['q' Quit | ▲▼ Vol | ◀▶ Seek | Home/End | 'y' Yank | 'd' DL | 'o' Search | 't' Script | 'e' Setup | 'P' List | 's' Suggest]")
                    .title_alignment(HorizontalAlignment::Center)
                    .render(area, f.buffer_mut());
                render_gauge(f, gauge_layout, ratio, self.theme);
            }
            Media::File(tagged, name) => {
                let file_name = Path::new(name)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let file_duration = tagged.properties().duration().as_secs();
                let ratio = if file_duration > 0 {
                    self.playback_time / file_duration as f64
                } else {
                    0.0
                };
                player_block(self.theme)
                    .title_top(format!(
                        "{} - {}:{}",
                        file_name,
                        format_time(self.playback_time as u32),
                        format_time(file_duration as u32),
                    ))
                    .title_alignment(HorizontalAlignment::Center)
                    .title_bottom("['q' Quit | ▲▼ Vol | ◀▶ Seek | Home/End | 'o' Search | 't' Script | 'e' Setup | 'P' List | 's' Suggest]")
                    .title_alignment(HorizontalAlignment::Center)
                    .render(area, f.buffer_mut());
                render_gauge(f, gauge_layout, ratio, self.theme);
            }
            Media::Empty => {
                player_block(self.theme)
                    .title_alignment(HorizontalAlignment::Center)
                    .title_bottom("['q' Quit | 'o' Search | 't' Script | 'e' Setup | 'P' List | 's' Suggest]")
                    .title_alignment(HorizontalAlignment::Center)
                    .render(area, f.buffer_mut());
                render_gauge(f, gauge_layout, 0.0, self.theme);
            }
            Media::Missing => warn!("Render conditions wrong"),
        }
    }
}

/// Inner layout for the gauge widget, centered vertically within the info panel.
fn gauge_rect(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
    .centered_vertically(Constraint::Percentage(50))
}

/// Progress gauge with the given ratio.
fn render_gauge(f: &mut Frame<'_>, area: Rect, ratio: f64, theme: &Theme) {
    Gauge::default()
        .block(
            Block::bordered().style(
                Style::default()
                    .fg(theme.gauge_fill())
                    .bg(theme.gauge_bg()),
            ),
        )
        .ratio(ratio)
        .render(area, f.buffer_mut());
}

/// Standard info block with theme colors.
fn player_block(theme: &Theme) -> Block<'static> {
    Block::bordered().style(Style::default().fg(theme.player_fg()).bg(theme.player_bg()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn draw_frame(width: u16, height: u16, started: bool) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let mut loader = Loader::default();
            let theme = Theme::default();
            let mut sidebar = Sidebar::new();
            let mut list_state = ListState::default();
            let search = SearchView {
                results: &[],
                selected: &mut list_state,
                query: "",
                api: None,
                searching: false,
                notice: "",
            };
            let transcript = TranscriptState::default();
            let setup = SetupState::default();
            let playlist = Playlist::default();
            let mut suggestion = SuggestionState::default();
            let mut ctx = DrawCtx {
                playback: Playback {
                    time: 10.0,
                    started,
                    volume: 50.0,
                },
                loader: &mut loader,
                search_open: false,
                search,
                transcript: &transcript,
                setup: &setup,
                suggestion: &mut suggestion,
                playlist: &playlist,
                media: Media::Empty,
                artwork: &mut None,
                theme: &theme,
                sidebar: &mut sidebar,
            };
            draw_screen(&mut ctx, f);
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn panel_rows(buf: &ratatui::buffer::Buffer, from_row: u16) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in from_row..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn player_panel_renders_below_artwork() {
        let buf = draw_frame(80, 24, true);
        // Artwork zone (top 60% ≈ rows 0..14) must stay empty without image…
        let top = panel_rows(&buf, 0);
        // …and the bottom panel (rows 14..24) must contain the bordered block.
        let bottom = panel_rows(&buf, 14);
        assert!(
            bottom.contains('╭') || bottom.contains('┌') || bottom.contains('+'),
            "bottom panel has no visible block borders:\n{bottom}\n--- top:\n{top}"
        );
    }

    #[test]
    fn loading_screen_renders() {
        let buf = draw_frame(80, 24, false);
        let all = panel_rows(&buf, 0);
        assert!(
            all.contains("Loading MPV"),
            "loading screen missing:\n{all}"
        );
    }

    #[test]
    fn artwork_does_not_cover_panel() {
        use ratatui_image::picker::Picker;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        // Solid 160x160 test image encoded through the real pipeline.
        let dyn_img = image::DynamicImage::new_rgb8(160, 160);
        term.draw(|f| {
            let mut loader = Loader::default();
            let theme = Theme::default();
            let mut sidebar = Sidebar::new();
            let mut list_state = ListState::default();
            // NOTE: encodes through the real pipeline (halfblocks write cells).
            let picker = Picker::halfblocks();
            let mut art = Some(picker.new_resize_protocol(dyn_img.clone()));
            let search = SearchView {
                results: &[],
                selected: &mut list_state,
                query: "",
                api: None,
                searching: false,
                notice: "",
            };
            let transcript = TranscriptState::default();
            let setup = SetupState::default();
            let playlist = Playlist::default();
            let mut suggestion = SuggestionState::default();
            let mut ctx = DrawCtx {
                playback: Playback {
                    time: 10.0,
                    started: true,
                    volume: 50.0,
                },
                loader: &mut loader,
                search_open: false,
                search,
                transcript: &transcript,
                setup: &setup,
                suggestion: &mut suggestion,
                playlist: &playlist,
                media: Media::Empty,
                artwork: &mut art,
                theme: &theme,
                sidebar: &mut sidebar,
            };
            draw_screen(&mut ctx, f);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let bottom = panel_rows(&buf, 14);
        assert!(
            bottom.contains('╭') || bottom.contains('┌') || bottom.contains('+'),
            "artwork covered the bottom panel:\n{bottom}"
        );
    }

    #[test]
    fn search_shows_loading_status() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let mut list_state = ListState::default();
            let mut search = SearchView {
                results: &[],
                selected: &mut list_state,
                query: "strange",
                api: None,
                searching: true,
                notice: "",
            };
            let area = f.area();
            let chunks =
                Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .split(area);
            search.render(f, chunks[1], &Theme::default());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let all = panel_rows(&buf, 0);
        assert!(
            all.contains("Searching"),
            "search loading status missing:\n{all}"
        );
        // Input box border follows the theme fg (default preset: yellow).
        assert_eq!(
            buf[(0, 14)].fg,
            ratatui::style::Color::Yellow,
            "search box does not use the theme"
        );
    }

    #[test]
    fn search_shows_error_notice() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let mut list_state = ListState::default();
            let mut search = SearchView {
                results: &[],
                selected: &mut list_state,
                query: "strange",
                api: None,
                searching: false,
                notice: "YouTube search failed: offline",
            };
            let area = f.area();
            let chunks =
                Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .split(area);
            search.render(f, chunks[1], &Theme::default());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let all = panel_rows(&buf, 0);
        assert!(
            all.contains("offline"),
            "search error notice missing:\n{all}"
        );
    }

    #[test]
    fn setup_reflects_theme() {
        use ratatui::style::Color;

        let render_fg = |theme: &Theme| {
            let backend = TestBackend::new(80, 24);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| {
                let mut setup = SetupState::default();
                setup.midi_in = vec!["None".to_string()];
                setup.render(f, f.area(), theme);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            // Focused "MIDI Input" column (top-left) border takes the theme fg.
            buf[(0, 0)].fg
        };

        assert_eq!(render_fg(&Theme::preset("default")), Color::Yellow);
        assert_eq!(
            render_fg(&Theme::preset("groovebox")),
            Color::Rgb(235, 219, 178)
        );
    }

    #[test]
    fn transcript_picker_lists_and_filters_tracks() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let theme = Theme::default();
            let mut state = TranscriptState {
                open: true,
                title: "Song".to_string(),
                picking: true,
                tracks: vec![
                    TranscriptTrack {
                        lang: "en".to_string(),
                        manual: true,
                    },
                    TranscriptTrack {
                        lang: "fr".to_string(),
                        manual: false,
                    },
                ],
                ..Default::default()
            };
            state.render(f, f.area(), &theme);
            // Filter down to French only.
            state.filter = "fr".to_string();
            assert_eq!(state.visible_tracks().len(), 1);
            assert_eq!(state.visible_tracks()[0].lang, "fr");
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let all = panel_rows(&buf, 0);
        assert!(
            all.contains("auto-generated"),
            "track list missing:\n{all}"
        );
    }

    #[test]
    fn transcript_reader_paginates_long_scripts() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let lines: Vec<String> = (0..50_000).map(|i| format!("line {i}")).collect();
        term.draw(|f| {
            let theme = Theme::default();
            let state = TranscriptState {
                open: true,
                title: "Long".to_string(),
                lines,
                scroll: 100,
                ..Default::default()
            };
            state.render(f, f.area(), &theme);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let all = panel_rows(&buf, 0);
        // Window around scroll=100 is visible…
        assert!(all.contains("line 100"), "window start missing:\n{all}");
        assert!(all.contains("[101/50000]"), "position indicator missing:\n{all}");
        // …but far-away lines are not rendered.
        assert!(
            !all.contains("line 49999"),
            "rendering escaped the window:\n{all}"
        );
    }

    #[test]
    fn stream_panel_renders_for_music_track() {
        // `TrackItem` is non_exhaustive: build one through JSON, like the
        // real Music search results that reach `Media::Stream`.
        let track: rustypipe::model::TrackItem = serde_json::from_value(serde_json::json!({
            "id": "AnBBinewQcQ",
            "name": "Strange World",
            "duration": 194,
            "cover": [],
            "artists": [],
            "track_type": "track",
            "by_va": false,
        }))
        .expect("test track must deserialize");
        let response = YoutubeResponse::Track(track);
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let mut loader = Loader::default();
            let theme = Theme::default();
            let mut sidebar = Sidebar::new();
            let mut list_state = ListState::default();
            let search = SearchView {
                results: &[],
                selected: &mut list_state,
                query: "",
                api: None,
                searching: false,
                notice: "",
            };
            let transcript = TranscriptState::default();
            let setup = SetupState::default();
            let playlist = Playlist::default();
            let mut suggestion = SuggestionState::default();
            let mut ctx = DrawCtx {
                playback: Playback {
                    time: 10.0,
                    started: true,
                    volume: 50.0,
                },
                loader: &mut loader,
                search_open: false,
                search,
                transcript: &transcript,
                setup: &setup,
                suggestion: &mut suggestion,
                playlist: &playlist,
                media: Media::Stream(&response),
                artwork: &mut None,
                theme: &theme,
                sidebar: &mut sidebar,
            };
            draw_screen(&mut ctx, f);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let bottom = panel_rows(&buf, 14);
        assert!(
            bottom.contains("Strange World"),
            "stream title missing from panel:\n{bottom}"
        );
        assert!(
            bottom.contains('╭') || bottom.contains('┌') || bottom.contains('+'),
            "stream panel has no visible block:\n{bottom}"
        );
    }

    #[test]
    fn grid_move_navigates_two_columns() {
        // 5 items: rows [0,1] [2,3] [4]
        assert_eq!(grid_move(0, 5, 1, 0), 1); // l
        assert_eq!(grid_move(1, 5, 1, 0), 1); // clamp row end
        assert_eq!(grid_move(1, 5, 0, 1), 3); // j
        assert_eq!(grid_move(3, 5, 0, 1), 4); // j into short row, col clamped
        assert_eq!(grid_move(4, 5, 0, 1), 4); // clamp bottom
        assert_eq!(grid_move(4, 5, 0, -1), 2); // k keeps col
        assert_eq!(grid_move(0, 5, -1, 0), 0); // clamp left
        assert_eq!(grid_move(0, 5, 0, -1), 0); // clamp top
        assert_eq!(grid_move(0, 0, 1, 1), 0); // empty
        assert_eq!(grid_move(9, 5, 0, 0), 4); // out of range clamps
    }

    #[test]
    fn suggestion_grid_renders_titles_and_thumbs() {
        use ratatui_image::picker::Picker;

        let track: rustypipe::model::TrackItem = serde_json::from_value(serde_json::json!({
            "id": "track1",
            "name": "First Song",
            "duration": 200,
            "cover": [],
            "artists": [],
            "track_type": "track",
            "by_va": false,
        }))
        .expect("test track must deserialize");
        let video: rustypipe::model::VideoItem = serde_json::from_value(serde_json::json!({
            "id": "vid2",
            "name": "Second Video",
            "duration": null,
            "thumbnail": [],
            "channel": null,
            "publish_date": null,
            "publish_date_txt": null,
            "view_count": null,
            "is_live": false,
            "is_short": false,
            "is_upcoming": false,
            "short_description": null,
        }))
        .expect("test video must deserialize");
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let theme = Theme::default();
            let picker = Picker::halfblocks();
            let mut state = SuggestionState {
                open: true,
                api: Some(YoutubeAPI::Music),
                title: "Seed".to_string(),
                items: vec![
                    ("First Song".to_string(), YoutubeResponse::Track(track)),
                    ("Second Video".to_string(), YoutubeResponse::Video(video)),
                    ("Third Song".to_string(), YoutubeResponse::Track(
                        serde_json::from_value(serde_json::json!({
                            "id": "track3",
                            "name": "Third Song",
                            "duration": 180,
                            "cover": [],
                            "artists": [],
                            "track_type": "track",
                            "by_va": false,
                        }))
                        .unwrap(),
                    )),
                ],
                selected: 1,
                ..Default::default()
            };
            // One cached thumb: first card shows image, others placeholder.
            let tiny = image::DynamicImage::new_rgb8(8, 8);
            state
                .thumbs
                .insert("track1".to_string(), picker.new_resize_protocol(tiny));
            state.render(f, f.area(), &theme);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let all = panel_rows(&buf, 0);
        assert!(all.contains("Suggestions (Music): Seed"), "header missing:\n{all}");
        assert!(all.contains("First Song"), "title 1 missing:\n{all}");
        assert!(all.contains("Second Video"), "title 2 missing:\n{all}");
        // Two uncached cards show the placeholder.
        assert!(all.contains('…'), "placeholder missing:\n{all}");
    }

    #[test]
    fn suggestion_loading_spinner_shows() {
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let theme = Theme::default();
            let mut state = SuggestionState {
                open: true,
                loading: true,
                ..Default::default()
            };
            state.render(f, f.area(), &theme);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let all = panel_rows(&buf, 0);
        assert!(all.contains("Loading"), "loading status missing:\n{all}");
    }
}
