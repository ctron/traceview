use std::{cmp, collections::VecDeque, io::Write, process::ExitStatus};

use anyhow::Result;
use crossterm::{
    cursor::{Hide, MoveTo},
    event::{KeyCode, KeyEvent, KeyModifiers},
    queue,
    style::{
        Attribute, Color, Print, PrintStyledContent, ResetColor, SetAttribute, SetBackgroundColor,
        SetForegroundColor, StyledContent, Stylize, style,
    },
    terminal::{self, Clear, ClearType},
};

use crate::model::{Level, LogEntry, MessagePart, Stream, TraceValue, TraceValueField};

#[derive(Debug, Default)]
pub(crate) struct ViewState {
    pub(crate) x_offset: usize,
    pub(crate) first_visible: usize,
    pub(crate) selected: Option<usize>,
    pub(crate) help_visible: bool,
    pub(crate) values: ValuesPaneState,
    pub(crate) show_spans: bool,
    pub(crate) show_raw: bool,
    pub(crate) search_query: String,
    pub(crate) search_editing: bool,
    pub(crate) search_wrapped: bool,
    pub(crate) focus_target: Option<String>,
    pub(crate) level_filter: LevelFilter,
}

#[derive(Debug, Default)]
pub(crate) struct ValuesPaneState {
    pub(crate) mode: ValuesPaneMode,
    pub(crate) selected: Option<usize>,
    pub(crate) x_offset: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ValuesPaneMode {
    #[default]
    Closed,
    Sidebar,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum LevelFilter {
    #[default]
    All,
    AtLeast(Level),
}

impl LevelFilter {
    fn includes(self, level: Level) -> bool {
        match self {
            Self::All => true,
            Self::AtLeast(minimum) => level.severity() >= minimum.severity(),
        }
    }

    fn status_label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::AtLeast(Level::Debug) => "DEBUG+",
            Self::AtLeast(Level::Info) => "INFO+",
            Self::AtLeast(Level::Warn) => "WARN+",
            Self::AtLeast(Level::Error) => "ERROR",
            Self::AtLeast(Level::Trace) => "TRACE+",
            Self::AtLeast(Level::Unknown) => "UNKNOWN+",
        }
    }

    fn status_color(self) -> Color {
        match self {
            Self::All => status_bar_foreground(),
            Self::AtLeast(level) => level_color(level),
        }
    }
}

impl ViewState {
    pub(crate) fn new() -> Self {
        Self {
            show_spans: true,
            ..Self::default()
        }
    }

    pub(crate) fn follow_latest(&mut self, entries: &VecDeque<LogEntry>, page_size: usize) {
        let visible = visible_indices(entries, self);
        self.selected = visible.last().copied();
        self.scroll_selected_into_view(entries, page_size);
    }

    pub(crate) fn remove_first_line(&mut self) {
        self.first_visible = self.first_visible.saturating_sub(1);
        self.selected = self.selected.map(|selected| selected.saturating_sub(1));
    }

    fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_editing = false;
        self.search_wrapped = false;
    }

    fn set_level_filter(
        &mut self,
        entries: &VecDeque<LogEntry>,
        filter: LevelFilter,
        page_size: usize,
    ) {
        self.level_filter = filter;
        self.search_wrapped = false;
        self.scroll_selected_into_view(entries, page_size);
    }

    fn toggle_focus(&mut self, entries: &VecDeque<LogEntry>, page_size: usize) {
        self.focus_target = if self.focus_target.is_some() {
            None
        } else {
            self.selected
                .and_then(|selected| entries.get(selected))
                .and_then(|entry| entry.target.clone())
        };
        self.scroll_selected_into_view(entries, page_size);
    }

    fn start_search(&mut self) {
        self.close_values_pane();
        self.search_editing = true;
        self.search_wrapped = false;
    }

    fn open_values_sidebar(&mut self, entries: &VecDeque<LogEntry>) {
        self.clear_search();
        self.values.mode = ValuesPaneMode::Sidebar;
        self.sync_values_selection(entries);
    }

    fn open_values_fullscreen(&mut self, entries: &VecDeque<LogEntry>) {
        self.clear_search();
        self.values.mode = ValuesPaneMode::Fullscreen;
        self.sync_values_selection(entries);
    }

    fn toggle_values_sidebar(&mut self, entries: &VecDeque<LogEntry>) {
        if self.values.mode == ValuesPaneMode::Sidebar {
            self.close_values_pane();
        } else {
            self.open_values_sidebar(entries);
        }
    }

    fn toggle_values_fullscreen(&mut self, entries: &VecDeque<LogEntry>) {
        if self.values.mode == ValuesPaneMode::Fullscreen {
            self.close_values_pane();
        } else {
            self.open_values_fullscreen(entries);
        }
    }

    fn close_values_pane(&mut self) {
        self.values.mode = ValuesPaneMode::Closed;
        self.values.selected = None;
        self.values.x_offset = 0;
    }

    fn sync_values_selection(&mut self, entries: &VecDeque<LogEntry>) {
        let value_count = selected_values_len(entries, self);
        self.values.selected = match (self.values.selected, value_count) {
            (_, 0) => None,
            (Some(selected), count) => Some(cmp::min(selected, count - 1)),
            (None, _) => Some(0),
        };
    }

    fn move_values_selection(&mut self, entries: &VecDeque<LogEntry>, delta: isize) {
        let value_count = selected_values_len(entries, self);
        if value_count == 0 {
            self.values.selected = None;
            return;
        }

        let selected = self.values.selected.unwrap_or(0);
        self.values.selected = Some(if delta.is_negative() {
            selected.saturating_sub(delta.unsigned_abs())
        } else {
            cmp::min(selected.saturating_add(delta as usize), value_count - 1)
        });
    }

    fn move_selected_to(&mut self, visible: &[usize], selected_visible: usize, page_size: usize) {
        self.selected = visible.get(selected_visible).copied();
        self.scroll_selected_into_visible_slice(visible, page_size);
    }

    fn move_selected_by(&mut self, visible: &[usize], delta: isize, page_size: usize) {
        let Some(selected_pos) = selected_visible_pos(visible, self.selected) else {
            return;
        };
        let selected_pos = if delta.is_negative() {
            selected_pos.saturating_sub(delta.unsigned_abs())
        } else {
            cmp::min(
                selected_pos.saturating_add(delta as usize),
                visible.len().saturating_sub(1),
            )
        };

        self.move_selected_to(visible, selected_pos, page_size);
    }

    fn scroll_selected_into_view(&mut self, entries: &VecDeque<LogEntry>, page_size: usize) {
        let visible = visible_indices(entries, self);
        self.scroll_selected_into_visible_slice(&visible, page_size);
    }

    fn scroll_selected_into_visible_slice(&mut self, visible: &[usize], page_size: usize) {
        let Some(selected) = self.selected else {
            self.first_visible = 0;
            return;
        };
        let Some(selected_pos) = visible.iter().position(|idx| *idx == selected) else {
            self.first_visible = visible.first().copied().unwrap_or(0);
            self.selected = visible.last().copied();
            return;
        };
        if visible.is_empty() {
            self.first_visible = 0;
            self.selected = None;
            return;
        }

        let page_size = cmp::max(1, page_size);
        let first_pos = visible
            .iter()
            .position(|idx| *idx == self.first_visible)
            .unwrap_or_else(|| {
                visible
                    .iter()
                    .position(|idx| *idx >= self.first_visible)
                    .unwrap_or_else(|| visible.len().saturating_sub(1))
            });
        let max_first_pos = visible.len().saturating_sub(page_size);
        let mut first_pos = cmp::min(first_pos, max_first_pos);

        if selected_pos < first_pos {
            first_pos = selected_pos;
        } else if selected_pos >= first_pos.saturating_add(page_size) {
            first_pos = selected_pos.saturating_add(1).saturating_sub(page_size);
        }
        self.first_visible = visible[first_pos];
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyAction {
    Continue,
    CopySelected,
    Quit,
}

pub(crate) fn handle_key(
    key: KeyEvent,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    process_exited: bool,
    page_size: usize,
) -> KeyAction {
    if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::Quit;
    }

    if state.search_editing {
        return handle_search_key(key, entries, state, page_size);
    }

    if state.help_visible {
        return handle_help_key(key, state);
    }

    handle_normal_key(key, entries, state, process_exited, page_size)
}

fn handle_search_key(
    key: KeyEvent,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    page_size: usize,
) -> KeyAction {
    match key.code {
        KeyCode::Esc => {
            state.clear_search();
        }
        KeyCode::Enter => {
            state.search_editing = false;
            state.search_wrapped = false;
            jump_to_search_match(entries, state, page_size, SearchDirection::Next);
        }
        KeyCode::Backspace => {
            state.search_query.pop();
            state.search_wrapped = false;
            jump_to_search_match(entries, state, page_size, SearchDirection::CurrentOrNext);
        }
        KeyCode::Char(ch) => {
            state.search_query.push(ch);
            state.search_wrapped = false;
            jump_to_search_match(entries, state, page_size, SearchDirection::CurrentOrNext);
        }
        _ => {}
    }

    KeyAction::Continue
}

fn handle_help_key(key: KeyEvent, state: &mut ViewState) -> KeyAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
            state.help_visible = false;
        }
        _ => {}
    }

    KeyAction::Continue
}

fn handle_values_key(
    key: KeyEvent,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
) -> KeyAction {
    const PANE_SCROLL_STEP: usize = 16;
    state.sync_values_selection(entries);

    match key.code {
        KeyCode::Esc => {
            state.close_values_pane();
        }
        KeyCode::Char('v') => {
            state.toggle_values_sidebar(entries);
        }
        KeyCode::Char('V') => {
            state.toggle_values_fullscreen(entries);
        }
        KeyCode::Char('/') => {
            state.start_search();
        }
        KeyCode::Up => state.move_values_selection(entries, -1),
        KeyCode::Down => state.move_values_selection(entries, 1),
        KeyCode::Left => {
            state.values.x_offset = state.values.x_offset.saturating_sub(PANE_SCROLL_STEP);
        }
        KeyCode::Right => {
            state.values.x_offset = state.values.x_offset.saturating_add(PANE_SCROLL_STEP);
        }
        KeyCode::Char('y') => return KeyAction::CopySelected,
        _ => {}
    }

    KeyAction::Continue
}

fn selected_values_len(entries: &VecDeque<LogEntry>, state: &ViewState) -> usize {
    selected_entry_for_values(entries, state)
        .map(|entry| {
            entry
                .values
                .iter()
                .map(|section| section.fields.len())
                .sum()
        })
        .unwrap_or(0)
}

fn handle_normal_key(
    key: KeyEvent,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    process_exited: bool,
    page_size: usize,
) -> KeyAction {
    let visible = visible_indices(entries, state);
    let page_step = cmp::max(1, page_size.saturating_sub(1));
    const HORIZONTAL_SCROLL_STEP: usize = 16;

    if state.values.mode != ValuesPaneMode::Closed {
        return handle_values_key(key, entries, state);
    }

    match key.code {
        KeyCode::Char('?') => {
            state.help_visible = !state.help_visible;
            return KeyAction::Continue;
        }
        KeyCode::Char('/') => {
            state.start_search();
            return KeyAction::Continue;
        }
        KeyCode::Char('n') => {
            jump_to_search_match(entries, state, page_size, SearchDirection::Next);
            return KeyAction::Continue;
        }
        KeyCode::Char('b') => {
            jump_to_search_match(entries, state, page_size, SearchDirection::Previous);
            return KeyAction::Continue;
        }
        KeyCode::Char('s') => {
            state.show_spans = !state.show_spans;
            return KeyAction::Continue;
        }
        KeyCode::Char('r') => {
            state.show_raw = !state.show_raw;
            return KeyAction::Continue;
        }
        KeyCode::Char('1') => {
            state.set_level_filter(entries, LevelFilter::All, page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('2') => {
            state.set_level_filter(entries, LevelFilter::AtLeast(Level::Debug), page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('3') => {
            state.set_level_filter(entries, LevelFilter::AtLeast(Level::Info), page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('4') => {
            state.set_level_filter(entries, LevelFilter::AtLeast(Level::Warn), page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('5') => {
            state.set_level_filter(entries, LevelFilter::AtLeast(Level::Error), page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('v') => {
            state.toggle_values_sidebar(entries);
            return KeyAction::Continue;
        }
        KeyCode::Char('V') => {
            state.toggle_values_fullscreen(entries);
            return KeyAction::Continue;
        }
        KeyCode::Char('f') => {
            state.toggle_focus(entries, page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('y') => return KeyAction::CopySelected,
        KeyCode::Esc if !state.search_query.is_empty() => {
            state.clear_search();
            return KeyAction::Continue;
        }
        KeyCode::Char('q') => {
            if process_exited {
                return KeyAction::Quit;
            }
        }
        KeyCode::Left => {
            state.x_offset = state.x_offset.saturating_sub(HORIZONTAL_SCROLL_STEP);
        }
        KeyCode::Right => {
            state.x_offset = state.x_offset.saturating_add(HORIZONTAL_SCROLL_STEP);
        }
        KeyCode::Home => state.move_selected_to(&visible, 0, page_size),
        KeyCode::End => {
            state.move_selected_to(&visible, visible.len().saturating_sub(1), page_size)
        }
        KeyCode::Up => state.move_selected_by(&visible, -1, page_size),
        KeyCode::Down => state.move_selected_by(&visible, 1, page_size),
        KeyCode::PageUp => state.move_selected_by(&visible, -(page_step as isize), page_size),
        KeyCode::PageDown => state.move_selected_by(&visible, page_step as isize, page_size),
        _ => {}
    }

    KeyAction::Continue
}

pub(crate) fn draw(
    stdout: &mut impl Write,
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
    exit_status: Option<ExitStatus>,
    input_finished: bool,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let content_rows = content_rows(rows, state);
    if state.help_visible {
        queue!(stdout, Hide, Clear(ClearType::All))?;
        draw_help_page(stdout, cols, content_rows)?;
        draw_status_line(
            stdout,
            entries,
            state,
            exit_status,
            input_finished,
            cols as usize,
            rows.saturating_sub(1),
        )?;
        stdout.flush()?;
        return Ok(());
    }

    let top_bar_rows = usize::from(search_bar_visible(state));
    let scrollbar_width = usize::from(cols > 1 && content_rows > 0);
    let pane_width = values_pane_width(cols as usize, &state.values);
    let pane_gap = usize::from(pane_width > 0);
    let log_width = if state.values.mode == ValuesPaneMode::Fullscreen {
        0
    } else {
        (cols as usize)
            .saturating_sub(scrollbar_width)
            .saturating_sub(pane_width)
            .saturating_sub(pane_gap)
    };
    let visible = visible_indices(entries, state);
    let selected = state
        .selected
        .filter(|selected| visible.contains(selected))
        .or_else(|| visible.last().copied());
    let start_pos = visible
        .iter()
        .position(|idx| *idx == state.first_visible)
        .unwrap_or_else(|| {
            visible
                .iter()
                .position(|idx| *idx >= state.first_visible)
                .unwrap_or(0)
        });
    let end_pos = cmp::min(start_pos + content_rows, visible.len());

    queue!(stdout, Hide, Clear(ClearType::All))?;

    if search_bar_visible(state) {
        draw_search_bar(stdout, entries, state, cols as usize)?;
    }

    if state.values.mode != ValuesPaneMode::Fullscreen {
        for (screen_row, idx) in visible[start_pos..end_pos].iter().copied().enumerate() {
            let Some(entry) = entries.get(idx) else {
                continue;
            };
            queue!(stdout, MoveTo(0, (screen_row + top_bar_rows) as u16))?;
            EntryRenderer::from(state).draw(
                stdout,
                entry,
                state.x_offset,
                log_width,
                Some(idx) == selected,
            )?;
        }
    }

    if scrollbar_width > 0 && state.values.mode != ValuesPaneMode::Fullscreen {
        draw_scrollbar(
            stdout,
            entries,
            &visible,
            ScrollbarViewport {
                visible_start: start_pos,
                visible_end: end_pos,
                height: content_rows,
                top_row: top_bar_rows,
                column: cols
                    .saturating_sub(1)
                    .saturating_sub(pane_width as u16)
                    .saturating_sub(pane_gap as u16),
            },
        )?;
    }

    if pane_width > 0 {
        let pane_left = if state.values.mode == ValuesPaneMode::Fullscreen {
            0
        } else {
            cols as usize - pane_width
        };
        let selected_entry = selected.and_then(|idx| entries.get(idx));
        draw_values_pane(
            stdout,
            selected_entry,
            &state.values,
            PaneViewport {
                left: pane_left,
                top: top_bar_rows,
                width: pane_width,
                height: content_rows,
            },
        )?;
    }

    draw_status_line(
        stdout,
        entries,
        state,
        exit_status,
        input_finished,
        cols as usize,
        rows.saturating_sub(1),
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn content_rows(rows: u16, state: &ViewState) -> usize {
    rows.saturating_sub(1 + u16::from(search_bar_visible(state))) as usize
}

fn search_bar_visible(state: &ViewState) -> bool {
    state.search_editing || !state.search_query.is_empty()
}

fn draw_search_bar(
    stdout: &mut impl Write,
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
    width: usize,
) -> Result<()> {
    let matches = search_match_indices(entries, state);
    let results = search_result_summary(&matches, state.selected, state.search_wrapped);
    let (prompt, cursor, help) = if state.search_editing {
        ("Search(*)", "_", "Enter accept  Esc clear")
    } else {
        ("Search", "", "/ edit  Esc clear")
    };
    let bar = format!(
        " {prompt}: {}{cursor}  {help}  n next  b previous  {results} ",
        state.search_query
    );
    let (foreground, background) = search_bar_colors(state.search_editing);
    queue!(
        stdout,
        MoveTo(0, 0),
        SetForegroundColor(foreground),
        SetBackgroundColor(background),
        Print(visible_slice(&format!("{bar:<width$}"), 0, width)),
        ResetColor
    )?;
    Ok(())
}

fn search_bar_colors(editing: bool) -> (Color, Color) {
    if editing {
        (
            Color::Black,
            Color::Rgb {
                r: 150,
                g: 205,
                b: 255,
            },
        )
    } else {
        (Color::Black, Color::White)
    }
}

fn status_bar_foreground() -> Color {
    Color::Black
}

fn status_bar_background() -> Color {
    Color::White
}

fn search_result_summary(matches: &[usize], selected: Option<usize>, wrapped: bool) -> String {
    if matches.is_empty() {
        return "0 results".to_string();
    }

    let label = result_count_label(matches.len());
    let Some(position) =
        selected.and_then(|selected| matches.iter().position(|idx| *idx == selected))
    else {
        return label;
    };

    let result_number = position + 1;
    let note = if wrapped { "  wrapped" } else { "" };

    format!("{label}  {result_number}/{}{note}", matches.len())
}

fn result_count_label(count: usize) -> String {
    if count == 1 {
        "1 result".to_string()
    } else {
        format!("{count} results")
    }
}

#[derive(Clone, Copy, Debug)]
struct ScrollbarViewport {
    visible_start: usize,
    visible_end: usize,
    height: usize,
    top_row: usize,
    column: u16,
}

fn draw_scrollbar(
    stdout: &mut impl Write,
    entries: &VecDeque<LogEntry>,
    visible_indices: &[usize],
    viewport: ScrollbarViewport,
) -> Result<()> {
    for row in 0..viewport.height {
        let slice = ScrollbarSlice::new(row, viewport.height, visible_indices.len());
        let in_view = slice.start < viewport.visible_end && slice.end > viewport.visible_start;
        let color = scrollbar_slice_color(entries, visible_indices, slice.start, slice.end);
        let marker = if in_view { "#" } else { "|" };
        let mut styled = style(marker).with(color);
        if in_view {
            styled = styled.attribute(Attribute::Bold);
        }

        queue!(
            stdout,
            MoveTo(viewport.column, (row + viewport.top_row) as u16),
            PrintStyledContent(styled)
        )?;
    }

    Ok(())
}

fn draw_help_page(stdout: &mut impl Write, cols: u16, content_rows: usize) -> Result<()> {
    let lines = [
        "tv help",
        "",
        "Actions",
        "  f               focus selected target, or clear focus",
        "  s               toggle span information",
        "  r               toggle raw log line display",
        "  1..5            filter all, debug+, info+, warn+, error",
        "  v / V           toggle tracing values pane or fullscreen",
        "  /               search raw log lines",
        "  n / b           jump to next or previous search result",
        "  Esc             clear search, close help, or close values pane",
        "  y               copy selected line or value to clipboard",
        "  ?               toggle this help page",
        "  q               exit after the process ends",
        "  Ctrl-C          kill process and exit",
        "",
        "Navigation",
        "  Up / Down       move cursor one line",
        "  PgUp / PgDown   move cursor one page",
        "  Home / Pos1     move cursor to first retained line",
        "  End             move cursor to last retained line",
        "  Left / Right    scroll horizontally, or scroll values pane",
    ];

    for (row, line) in lines.iter().take(content_rows).enumerate() {
        let color = match *line {
            "tv help" | "Navigation" | "Actions" => Color::Cyan,
            _ => Color::White,
        };
        queue!(
            stdout,
            MoveTo(0, row as u16),
            PrintStyledContent(visible_slice(line, 0, cols as usize).with(color))
        )?;
    }

    Ok(())
}

pub(crate) fn selected_line_text(
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
) -> Option<String> {
    if state.values.mode != ValuesPaneMode::Closed {
        return selected_value_text(entries, state);
    }

    entries
        .get(state.selected?)
        .map(|entry| EntryRenderer::from(state).plain_text(entry))
}

fn selected_value_text(entries: &VecDeque<LogEntry>, state: &ViewState) -> Option<String> {
    let entry = selected_entry_for_values(entries, state)?;
    let selected = state.values.selected?;
    let value = nth_value_field(entry, selected)?;
    Some(format!("{} = {}", value.key, value.value.render_text()))
}

#[derive(Clone, Copy, Debug)]
struct PaneViewport {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

fn values_pane_width(cols: usize, state: &ValuesPaneState) -> usize {
    match state.mode {
        ValuesPaneMode::Closed => 0,
        ValuesPaneMode::Fullscreen => cols,
        ValuesPaneMode::Sidebar if cols < 60 => 0,
        ValuesPaneMode::Sidebar => (cols / 3).clamp(24, 42),
    }
}

fn draw_values_pane(
    stdout: &mut impl Write,
    entry: Option<&LogEntry>,
    state: &ValuesPaneState,
    viewport: PaneViewport,
) -> Result<()> {
    if viewport.width == 0 || viewport.height == 0 {
        return Ok(());
    }

    for row in 0..viewport.height {
        let text_width = viewport.width.saturating_sub(1);
        queue!(
            stdout,
            MoveTo(viewport.left as u16, (viewport.top + row) as u16),
            PrintStyledContent("|".with(Color::DarkGrey))
        )?;

        match row {
            0 => print_padded_segment(
                stdout,
                "Tracing values",
                0,
                text_width,
                Color::Cyan,
                false,
                true,
            )?,
            row => match entry.and_then(|entry| values_pane_row(entry, row - 1)) {
                Some(ValuesPaneRow::Section(title)) => {
                    print_padded_segment(stdout, title, 0, text_width, Color::Cyan, false, true)?;
                }
                Some(ValuesPaneRow::Field { index, field }) => {
                    let selected = state.selected == Some(index);
                    print_value_row(
                        stdout,
                        &field.key,
                        &field.value.render_text(),
                        trace_value_color(&field.value),
                        state.x_offset,
                        text_width,
                        selected,
                    )?;
                }
                Some(ValuesPaneRow::Spacer) => {
                    print_padded_segment(stdout, "", 0, text_width, Color::White, false, false)?;
                }
                None => {
                    let empty_label =
                        if row == 1 && entry.is_none_or(|entry| value_field_count(entry) == 0) {
                            "No values"
                        } else {
                            ""
                        };
                    print_padded_segment(
                        stdout,
                        empty_label,
                        0,
                        text_width,
                        Color::White,
                        false,
                        false,
                    )?;
                }
            },
        }
        queue!(stdout, ResetColor)?;
    }

    Ok(())
}

enum ValuesPaneRow<'a> {
    Section(&'a str),
    Field {
        index: usize,
        field: &'a TraceValueField,
    },
    Spacer,
}

fn values_pane_row(entry: &LogEntry, row: usize) -> Option<ValuesPaneRow<'_>> {
    let mut row = row;
    let mut field_index = 0usize;
    let mut needs_spacer = false;

    for section in &entry.values {
        if section.fields.is_empty() {
            continue;
        }
        if needs_spacer {
            if row == 0 {
                return Some(ValuesPaneRow::Spacer);
            }
            row -= 1;
        }
        if row == 0 {
            return Some(ValuesPaneRow::Section(&section.title));
        }
        row -= 1;

        if row < section.fields.len() {
            return Some(ValuesPaneRow::Field {
                index: field_index + row,
                field: &section.fields[row],
            });
        }
        row -= section.fields.len();
        field_index += section.fields.len();
        needs_spacer = true;
    }

    None
}

fn nth_value_field(entry: &LogEntry, selected: usize) -> Option<&TraceValueField> {
    let mut selected = selected;
    for section in &entry.values {
        if selected < section.fields.len() {
            return section.fields.get(selected);
        }
        selected -= section.fields.len();
    }
    None
}

fn value_field_count(entry: &LogEntry) -> usize {
    entry
        .values
        .iter()
        .map(|section| section.fields.len())
        .sum()
}

fn print_value_row(
    stdout: &mut impl Write,
    key: &str,
    value: &str,
    value_color: Color,
    x_offset: usize,
    width: usize,
    selected: bool,
) -> Result<()> {
    let prefix = format!("{key} = ");
    let prefix_width = prefix.chars().count();

    if x_offset < prefix_width {
        let prefix_width = cmp::min(prefix_width - x_offset, width);
        print_padded_segment(
            stdout,
            &prefix,
            x_offset,
            prefix_width,
            Color::White,
            selected,
            false,
        )?;
        print_padded_segment(
            stdout,
            value,
            0,
            width.saturating_sub(prefix_width),
            value_color,
            selected,
            false,
        )?;
    } else {
        print_padded_segment(
            stdout,
            value,
            x_offset - prefix_width,
            width,
            value_color,
            selected,
            false,
        )?;
    }

    Ok(())
}

fn print_padded_segment(
    stdout: &mut impl Write,
    text: &str,
    x_offset: usize,
    width: usize,
    color: Color,
    selected: bool,
    bold: bool,
) -> Result<()> {
    let text = visible_slice(text, x_offset, width);
    let background = if selected {
        selected_background()
    } else {
        Color::Reset
    };
    queue!(
        stdout,
        SetBackgroundColor(background),
        PrintStyledContent(apply_style(format!("{text:<width$}"), color, bold))
    )?;
    Ok(())
}

fn trace_value_color(value: &TraceValue) -> Color {
    match value {
        TraceValue::Bool(_) | TraceValue::Number(_) => Color::Reset,
        TraceValue::String(_) => Color::Green,
        TraceValue::Null => Color::DarkGrey,
        TraceValue::Object(_) | TraceValue::Array(_) => Color::Reset,
        TraceValue::Other(_) => Color::White,
    }
}

fn visible_indices(entries: &VecDeque<LogEntry>, state: &ViewState) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| entry_visible(entry, state).then_some(idx))
        .collect()
}

fn selected_entry_for_values<'a>(
    entries: &'a VecDeque<LogEntry>,
    state: &ViewState,
) -> Option<&'a LogEntry> {
    let visible = visible_indices(entries, state);
    let selected = state
        .selected
        .filter(|selected| visible.contains(selected))
        .or_else(|| visible.last().copied())?;
    entries.get(selected)
}

fn entry_visible(entry: &LogEntry, state: &ViewState) -> bool {
    state.level_filter.includes(entry.level)
        && state
            .focus_target
            .as_deref()
            .is_none_or(|target| entry.target.as_deref() == Some(target))
}

fn selected_visible_pos(visible: &[usize], selected: Option<usize>) -> Option<usize> {
    selected
        .and_then(|selected| visible.iter().position(|idx| *idx == selected))
        .or_else(|| visible.len().checked_sub(1))
}

#[derive(Clone, Copy, Debug)]
enum SearchDirection {
    CurrentOrNext,
    Next,
    Previous,
}

fn jump_to_search_match(
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    page_size: usize,
    direction: SearchDirection,
) {
    let matches = search_match_indices(entries, state);
    if matches.is_empty() {
        return;
    }

    let selected = state.selected;
    let (next, wrapped) = match direction {
        SearchDirection::CurrentOrNext => (
            matches
                .iter()
                .copied()
                .find(|idx| Some(*idx) == selected)
                .or_else(|| next_match_after(&matches, selected))
                .unwrap_or(matches[0]),
            false,
        ),
        SearchDirection::Next => match next_match_after(&matches, selected) {
            Some(next) => (next, false),
            None => (matches[0], selected.is_some()),
        },
        SearchDirection::Previous => match previous_match_before(&matches, selected) {
            Some(previous) => (previous, false),
            None => {
                let Some(previous) = matches.last().copied() else {
                    return;
                };
                (previous, selected.is_some())
            }
        },
    };

    state.search_wrapped = wrapped;
    state.selected = Some(next);
    state.scroll_selected_into_view(entries, page_size);
}

fn next_match_after(matches: &[usize], selected: Option<usize>) -> Option<usize> {
    let selected = selected?;
    matches.iter().copied().find(|idx| *idx > selected)
}

fn previous_match_before(matches: &[usize], selected: Option<usize>) -> Option<usize> {
    let selected = selected?;
    matches.iter().rev().copied().find(|idx| *idx < selected)
}

fn search_match_indices(entries: &VecDeque<LogEntry>, state: &ViewState) -> Vec<usize> {
    if state.search_query.is_empty() {
        return Vec::new();
    }

    visible_indices(entries, state)
        .into_iter()
        .filter(|idx| entries[*idx].raw.contains(&state.search_query))
        .collect()
}

#[derive(Clone, Debug)]
struct RenderOptions {
    show_spans: bool,
    show_raw: bool,
    search_query: String,
}

impl From<&ViewState> for RenderOptions {
    fn from(state: &ViewState) -> Self {
        Self {
            show_spans: state.show_spans,
            show_raw: state.show_raw,
            search_query: state.search_query.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Part {
    text: String,
    color: Color,
    bold: bool,
    highlighted: bool,
}

impl Part {
    fn new(text: impl Into<String>, color: Color, bold: bool) -> Self {
        Self {
            text: text.into(),
            color,
            bold,
            highlighted: false,
        }
    }
}

fn apply_search_highlights(parts: Vec<Part>, query: &str) -> Vec<Part> {
    if query.is_empty() {
        return parts;
    }

    let Some(highlighter) = SearchHighlighter::new(&parts, query) else {
        return parts;
    };
    highlighter.apply(parts)
}

#[derive(Debug)]
struct SearchHighlighter {
    ranges: Vec<(usize, usize)>,
    range_idx: usize,
    cursor: usize,
    highlighted: Vec<Part>,
}

impl SearchHighlighter {
    fn new(parts: &[Part], query: &str) -> Option<Self> {
        let text = EntryRenderer::plain_text_from_parts(parts);
        let ranges = text
            .match_indices(query)
            .map(|(idx, found)| (idx, idx + found.len()))
            .fold(Vec::new(), |mut ranges: Vec<(usize, usize)>, range| {
                if let Some(previous) = ranges.last_mut()
                    && range.0 <= previous.1
                {
                    previous.1 = cmp::max(previous.1, range.1);
                    return ranges;
                }
                ranges.push(range);
                ranges
            });

        (!ranges.is_empty()).then_some(Self {
            ranges,
            range_idx: 0,
            cursor: 0,
            highlighted: Vec::new(),
        })
    }

    fn apply(mut self, parts: Vec<Part>) -> Vec<Part> {
        for part in parts {
            self.push_part(part);
        }
        self.highlighted
    }

    fn push_part(&mut self, part: Part) {
        let part_len = part.text.len();
        let part_start = self.cursor;
        let part_end = part_start + part_len;
        self.cursor = part_end;

        while self.range_idx < self.ranges.len() && self.ranges[self.range_idx].1 <= part_start {
            self.range_idx += 1;
        }

        let mut local_start = 0usize;
        let mut current_range_idx = self.range_idx;
        while local_start < part_len {
            if current_range_idx >= self.ranges.len()
                || self.ranges[current_range_idx].0 >= part_end
            {
                self.push_segment(&part, local_start, part_len, false);
                break;
            }

            let (range_start, range_end) = self.ranges[current_range_idx];
            if part_start + local_start < range_start {
                let local_end = range_start - part_start;
                self.push_segment(&part, local_start, local_end, false);
                local_start = local_end;
                continue;
            }

            let local_end = cmp::min(range_end, part_end) - part_start;
            self.push_segment(&part, local_start, local_end, true);
            local_start = local_end;
            if range_end <= part_start + local_start {
                current_range_idx += 1;
            }
        }
    }

    fn push_segment(
        &mut self,
        part: &Part,
        local_start: usize,
        local_end: usize,
        highlighted_segment: bool,
    ) {
        if local_start == local_end {
            return;
        }

        self.highlighted.push(Part {
            text: part.text[local_start..local_end].to_string(),
            color: part.color,
            bold: part.bold,
            highlighted: part.highlighted || highlighted_segment,
        });
    }
}

#[derive(Clone, Debug)]
struct EntryRenderer {
    options: RenderOptions,
}

impl EntryRenderer {
    fn from(state: &ViewState) -> Self {
        Self {
            options: RenderOptions::from(state),
        }
    }

    fn draw(
        self,
        stdout: &mut impl Write,
        entry: &LogEntry,
        x_offset: usize,
        width: usize,
        selected: bool,
    ) -> Result<()> {
        let parts = self.parts(entry);
        let rendered = Self::plain_text_from_parts(&parts);
        let rendered_width = rendered.chars().count();
        if width == 0 {
            return Ok(());
        }

        let viewport = LineViewport::new(rendered_width, x_offset, width);
        let content_width = viewport.content_width;
        let visible = visible_slice(&rendered, x_offset, content_width);
        let mut cursor = 0usize;

        if viewport.show_left_marker {
            print_segment(
                stdout,
                "<".to_string(),
                Color::DarkGrey,
                true,
                false,
                selected,
            )?;
        }

        for part in parts {
            let part_start = cursor;
            let part_end = cursor + part.text.chars().count();
            cursor = part_end;

            let overlap_start = cmp::max(part_start, x_offset);
            let overlap_end = cmp::min(part_end, x_offset.saturating_add(content_width));
            if overlap_start >= overlap_end {
                continue;
            }

            let local_start = overlap_start - part_start;
            let local_len = overlap_end - overlap_start;
            let segment: String = part
                .text
                .chars()
                .skip(local_start)
                .take(local_len)
                .collect();
            print_segment(
                stdout,
                segment,
                part.color,
                part.bold,
                part.highlighted,
                selected,
            )?;
        }

        let remaining = content_width.saturating_sub(visible.chars().count());
        if remaining > 0 {
            print_segment(
                stdout,
                " ".repeat(remaining),
                Color::White,
                false,
                false,
                selected,
            )?;
        }

        if viewport.show_right_marker {
            print_segment(
                stdout,
                ">".to_string(),
                Color::DarkGrey,
                true,
                false,
                selected,
            )?;
        }

        if selected {
            queue!(stdout, ResetColor)?;
        }
        Ok(())
    }

    fn plain_text(&self, entry: &LogEntry) -> String {
        Self::plain_text_from_parts(&self.parts(entry))
    }

    fn plain_text_from_parts(parts: &[Part]) -> String {
        parts.iter().map(|part| part.text.as_str()).collect()
    }

    fn parts(&self, entry: &LogEntry) -> Vec<Part> {
        let mut parts = Vec::new();
        parts.push(Part::new(
            format!("{} ", entry.stream.indicator()),
            stream_color(entry.stream),
            true,
        ));
        if self.options.show_raw {
            parts.push(Part::new(&entry.raw, message_color(entry), false));
            return apply_search_highlights(parts, self.search_highlight_query(entry));
        }
        if let Some(timestamp) = &entry.timestamp {
            parts.push(Part::new(format!("{timestamp} "), Color::DarkGrey, false));
        }
        if entry.parsed {
            parts.push(Part::new(
                format!("{:<5} ", entry.level.label()),
                level_color(entry.level),
                true,
            ));
        }
        if let Some(target) = &entry.target {
            self.push_target_parts(&mut parts, target);
        }
        if self.options.show_spans {
            self.push_span_parts(&mut parts, &entry.spans);
        }
        self.push_message_parts(
            &mut parts,
            &entry.message,
            &entry.message_parts,
            message_color(entry),
        );
        apply_search_highlights(parts, self.search_highlight_query(entry))
    }

    fn search_highlight_query<'a>(&'a self, entry: &LogEntry) -> &'a str {
        if !self.options.search_query.is_empty() && entry.raw.contains(&self.options.search_query) {
            &self.options.search_query
        } else {
            ""
        }
    }

    fn push_target_parts(&self, parts: &mut Vec<Part>, target: &str) {
        let split_at = target
            .char_indices()
            .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx))
            .unwrap_or(target.len());
        let (module_path, suffix) = target.split_at(split_at);

        let mut modules = module_path.split("::").peekable();
        while let Some(module) = modules.next() {
            parts.push(Part::new(module, target_module_color(module), false));
            if modules.peek().is_some() {
                parts.push(Part::new("::", Color::DarkGrey, false));
            }
        }
        if !suffix.is_empty() {
            parts.push(Part::new(suffix, Color::DarkGrey, false));
        }
        parts.push(Part::new(": ", Color::DarkGrey, false));
    }

    fn push_span_parts(&self, parts: &mut Vec<Part>, spans: &[String]) {
        for span in spans {
            self.push_span_part(parts, span);
            parts.push(Part::new(": ", Color::DarkGrey, false));
        }
    }

    fn push_span_part(&self, parts: &mut Vec<Part>, span: &str) {
        if let Some(open) = span.find('{') {
            let (name, fields) = span.split_at(open);
            parts.push(Part::new(name, span_name_color(name), false));
            self.push_span_fields(parts, fields);
        } else {
            parts.push(Part::new(span, span_name_color(span), false));
        }
    }

    fn push_span_fields(&self, parts: &mut Vec<Part>, fields: &str) {
        let mut current = String::new();
        let mut token = String::new();
        let mut chars = fields.chars().peekable();
        let mut in_string = false;
        let mut expecting_key = true;
        let mut expecting_value = false;

        while let Some(ch) = chars.next() {
            if in_string {
                token.push(ch);
                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                } else if ch == '"' {
                    parts.push(Part::new(std::mem::take(&mut token), string_color(), false));
                    in_string = false;
                    expecting_value = false;
                }
                continue;
            }

            match ch {
                '"' => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    token.push(ch);
                    in_string = true;
                }
                '=' | ':' => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    parts.push(Part::new(ch.to_string(), span_punctuation_color(), false));
                    expecting_key = false;
                    expecting_value = true;
                }
                '{' | '}' | '(' | ')' | '[' | ']' | ',' => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    parts.push(Part::new(ch.to_string(), span_punctuation_color(), false));
                    expecting_key = matches!(ch, '{' | ',' | '(');
                    expecting_value = false;
                }
                ch if ch.is_whitespace() => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    current.push(ch);
                    if !current.is_empty() {
                        parts.push(Part::new(std::mem::take(&mut current), Color::Reset, false));
                    }
                    expecting_key = !expecting_value;
                }
                _ => token.push(ch),
            }
        }

        if in_string {
            parts.push(Part::new(token, string_color(), false));
        } else {
            flush_span_token(parts, &mut token, expecting_key, expecting_value);
        }
    }
}

fn flush_span_token(
    parts: &mut Vec<Part>,
    token: &mut String,
    expecting_key: bool,
    expecting_value: bool,
) {
    if token.is_empty() {
        return;
    }

    let color = if expecting_key {
        span_key_color()
    } else if expecting_value {
        span_value_color(token)
    } else {
        Color::Reset
    };
    parts.push(Part::new(std::mem::take(token), color, false));
}

impl EntryRenderer {
    fn push_message_parts(
        &self,
        parts: &mut Vec<Part>,
        message: &str,
        message_parts: &[MessagePart],
        base_color: Color,
    ) {
        if !message_parts.is_empty() {
            let mut rendered_text = false;
            for part in message_parts {
                self.push_message_part(parts, part, base_color, rendered_text);
                if matches!(part, MessagePart::Text(_)) {
                    rendered_text = true;
                }
            }
            return;
        }

        let mut current = String::new();
        let mut chars = message.chars().peekable();
        let mut in_string = false;

        while let Some(ch) = chars.next() {
            current.push(ch);

            if ch == '\\' && in_string {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
                continue;
            }

            if ch != '"' {
                continue;
            }

            if in_string {
                parts.push(Part::new(
                    std::mem::take(&mut current),
                    string_color(),
                    false,
                ));
                in_string = false;
            } else {
                if current.len() > ch.len_utf8() {
                    let quote = current.split_off(current.len() - ch.len_utf8());
                    parts.push(Part::new(std::mem::take(&mut current), base_color, false));
                    current = quote;
                }
                in_string = true;
            }
        }

        if !current.is_empty() {
            let color = if in_string {
                string_color()
            } else {
                base_color
            };
            parts.push(Part::new(current, color, false));
        }
    }

    fn push_message_part(
        &self,
        parts: &mut Vec<Part>,
        part: &MessagePart,
        base_color: Color,
        after_text: bool,
    ) {
        match part {
            MessagePart::Text(text) => parts.push(Part::new(text, base_color, false)),
            MessagePart::Fields(fields) => {
                parts.push(Part::new(
                    if after_text { " (" } else { "(" },
                    Color::DarkGrey,
                    false,
                ));
                self.push_trace_fields(parts, fields, " ");
                parts.push(Part::new(")", Color::DarkGrey, false));
            }
        }
    }

    fn push_trace_fields(
        &self,
        parts: &mut Vec<Part>,
        fields: &[TraceValueField],
        separator: &str,
    ) {
        for (idx, field) in fields.iter().enumerate() {
            if idx > 0 {
                parts.push(Part::new(separator, Color::DarkGrey, false));
            }
            parts.push(Part::new(&field.key, Color::Blue, true));
            parts.push(Part::new("=", Color::DarkGrey, false));
            self.push_trace_value(parts, &field.value);
        }
    }

    fn push_trace_value(&self, parts: &mut Vec<Part>, value: &TraceValue) {
        match value {
            TraceValue::Bool(value) => {
                parts.push(Part::new(value.to_string(), Color::Reset, false))
            }
            TraceValue::Null => parts.push(Part::new("null", Color::DarkGrey, false)),
            TraceValue::Number(value) => parts.push(Part::new(value, Color::Reset, false)),
            TraceValue::String(value) => parts.push(Part::new(
                serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
                Color::Green,
                false,
            )),
            TraceValue::Other(value) => parts.push(Part::new(value, Color::White, false)),
            TraceValue::Array(values) => {
                parts.push(Part::new("[", Color::Reset, true));
                for (idx, value) in values.iter().enumerate() {
                    if idx > 0 {
                        parts.push(Part::new(",", Color::DarkGrey, false));
                    }
                    self.push_trace_value(parts, value);
                }
                parts.push(Part::new("]", Color::Reset, true));
            }
            TraceValue::Object(fields) => {
                parts.push(Part::new("{", Color::Reset, true));
                for (idx, (key, value)) in fields.iter().enumerate() {
                    if idx > 0 {
                        parts.push(Part::new(",", Color::DarkGrey, false));
                    }
                    parts.push(Part::new(
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        Color::Blue,
                        true,
                    ));
                    parts.push(Part::new(":", Color::DarkGrey, false));
                    self.push_trace_value(parts, value);
                }
                parts.push(Part::new("}", Color::Reset, true));
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct LineViewport {
    content_width: usize,
    show_left_marker: bool,
    show_right_marker: bool,
}

impl LineViewport {
    fn new(line_width: usize, x_offset: usize, terminal_width: usize) -> Self {
        let show_left_marker = x_offset > 0 && terminal_width > 1;
        let mut content_width = terminal_width.saturating_sub(usize::from(show_left_marker));
        let show_right_marker =
            line_width > x_offset.saturating_add(content_width) && content_width > 0;
        content_width = content_width.saturating_sub(usize::from(show_right_marker));

        Self {
            content_width,
            show_left_marker,
            show_right_marker,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ScrollbarSlice {
    start: usize,
    end: usize,
}

impl ScrollbarSlice {
    fn new(row: usize, height: usize, entries: usize) -> Self {
        if height == 0 || entries == 0 {
            return Self { start: 0, end: 0 };
        }

        let start = row.saturating_mul(entries) / height;
        let mut end = (row.saturating_add(1))
            .saturating_mul(entries)
            .div_ceil(height);
        end = cmp::min(cmp::max(end, start.saturating_add(1)), entries);

        Self { start, end }
    }
}

fn print_segment(
    stdout: &mut impl Write,
    text: String,
    color: Color,
    bold: bool,
    highlighted: bool,
    selected: bool,
) -> Result<()> {
    if highlighted {
        queue!(
            stdout,
            SetBackgroundColor(search_match_background()),
            SetForegroundColor(search_match_foreground())
        )?;
        if bold {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(text), ResetColor)?;
        if bold {
            queue!(stdout, SetAttribute(Attribute::NormalIntensity))?;
        }
    } else if selected {
        queue!(
            stdout,
            SetBackgroundColor(selected_background()),
            SetForegroundColor(selected_foreground(color))
        )?;
        if bold {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(text))?;
        if bold {
            queue!(stdout, SetAttribute(Attribute::NormalIntensity))?;
        }
    } else {
        queue!(stdout, PrintStyledContent(apply_style(text, color, bold)))?;
    }

    Ok(())
}

fn apply_style(text: String, color: Color, bold: bool) -> StyledContent<String> {
    let mut styled = style(text).with(color);
    if bold {
        styled = styled.attribute(Attribute::Bold);
    }
    styled
}

fn selected_background() -> Color {
    Color::Rgb {
        r: 64,
        g: 64,
        b: 64,
    }
}

fn search_match_background() -> Color {
    Color::Yellow
}

fn search_match_foreground() -> Color {
    Color::Black
}

fn selected_foreground(color: Color) -> Color {
    match color {
        Color::Red => Color::DarkRed,
        Color::Yellow => Color::DarkYellow,
        Color::Green => Color::DarkGreen,
        Color::Blue => Color::DarkBlue,
        Color::Cyan => Color::DarkCyan,
        Color::White | Color::Grey => Color::Reset,
        other => other,
    }
}

fn draw_status_line(
    stdout: &mut impl Write,
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
    exit_status: Option<ExitStatus>,
    input_finished: bool,
    width: usize,
    row: u16,
) -> Result<()> {
    let status = status_line(entries, state, exit_status, input_finished, width);
    let filter = state.level_filter.status_label();

    queue!(
        stdout,
        MoveTo(0, row),
        SetForegroundColor(status_bar_foreground()),
        SetBackgroundColor(status_bar_background()),
    )?;

    if let Some(start) = status.find(filter) {
        let end = start + filter.len();
        queue!(
            stdout,
            Print(&status[..start]),
            SetForegroundColor(state.level_filter.status_color()),
            Print(&status[start..end]),
            SetForegroundColor(status_bar_foreground()),
            Print(&status[end..]),
            ResetColor
        )?;
    } else {
        queue!(stdout, Print(status), ResetColor)?;
    }

    Ok(())
}

fn status_line(
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
    exit_status: Option<ExitStatus>,
    input_finished: bool,
    width: usize,
) -> String {
    let entry_count = entries.len();
    let selected = state.selected.map(|idx| idx + 1).unwrap_or(0);
    let follow = if state.selected.is_some_and(|idx| idx + 1 == entry_count) {
        " | auto-scroll"
    } else {
        ""
    };
    let process = match exit_status {
        Some(status) => format!("exited {status}"),
        None if input_finished => "loaded".to_string(),
        None => "running".to_string(),
    };
    let focus = state
        .focus_target
        .as_deref()
        .map(|target| focus_status(entries, target))
        .unwrap_or_default();
    let levels = LevelCounts::from_entries(entries, state).summary();
    let search = search_status(entries, state);

    let status = format!(
        " {process} | line {selected}/{entries}{follow}{focus}{search} | lvl {levels} | {} | x={} | spans {} | raw {} | ? help ",
        state.level_filter.status_label(),
        state.x_offset,
        if state.show_spans { "on" } else { "off" },
        if state.show_raw { "on" } else { "off" },
        entries = entry_count
    );
    visible_slice(&format!("{status:<width$}"), 0, width)
}

fn search_status(entries: &VecDeque<LogEntry>, state: &ViewState) -> String {
    if state.search_query.is_empty() && !state.search_editing {
        return String::new();
    }

    let matches = search_match_indices(entries, state).len();
    let prefix = if state.search_editing {
        "search /"
    } else {
        "search "
    };
    format!(" | {prefix}{} ({matches} results)", state.search_query)
}

#[derive(Default)]
struct LevelCounts {
    error: usize,
    warn: usize,
    info: usize,
    debug: usize,
    trace: usize,
    unknown: usize,
}

impl LevelCounts {
    fn from_entries(entries: &VecDeque<LogEntry>, state: &ViewState) -> Self {
        let mut counts = Self::default();
        for entry in entries.iter().filter(|entry| {
            state
                .focus_target
                .as_deref()
                .is_none_or(|target| entry.target.as_deref() == Some(target))
        }) {
            counts.add(entry.level);
        }
        counts
    }

    fn add(&mut self, level: Level) {
        match level {
            Level::Error => self.error += 1,
            Level::Warn => self.warn += 1,
            Level::Info => self.info += 1,
            Level::Debug => self.debug += 1,
            Level::Trace => self.trace += 1,
            Level::Unknown => self.unknown += 1,
        }
    }

    fn summary(&self) -> String {
        format!(
            "E{} W{} I{} D{} T{} U{}",
            self.error, self.warn, self.info, self.debug, self.trace, self.unknown
        )
    }
}

fn focus_status(entries: &VecDeque<LogEntry>, target: &str) -> String {
    let hidden = entries
        .iter()
        .filter(|entry| entry.target.as_deref() != Some(target))
        .count();
    let percent = hidden_percentage(hidden, entries.len());
    format!(" | focus {target} ({hidden} hidden, {percent}%)")
}

fn hidden_percentage(hidden: usize, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    (hidden * 100 + total / 2) / total
}

fn level_color(level: Level) -> Color {
    match level {
        Level::Trace => Color::DarkGrey,
        Level::Debug => Color::Cyan,
        Level::Info => Color::Green,
        Level::Warn => Color::Yellow,
        Level::Error => Color::Red,
        Level::Unknown => Color::White,
    }
}

fn stream_color(stream: Stream) -> Color {
    match stream {
        Stream::Stdout => Color::DarkGrey,
        Stream::Stderr => Color::Yellow,
    }
}

fn string_color() -> Color {
    Color::Rgb {
        r: 206,
        g: 145,
        b: 120,
    }
}

fn span_name_color(span: &str) -> Color {
    span_palette_color(stable_hash(span) % SPAN_PALETTE_SIZE)
}

const SPAN_PALETTE_SIZE: usize = 64;

fn span_palette_color(index: usize) -> Color {
    let hue = (index as f32 * 360.0 / SPAN_PALETTE_SIZE as f32 + 18.0) % 360.0;
    let (r, g, b) = hsl_to_rgb(hue, 0.34, 0.62);

    Color::Rgb { r, g, b }
}

fn span_key_color() -> Color {
    Color::Rgb {
        r: 156,
        g: 220,
        b: 254,
    }
}

fn span_punctuation_color() -> Color {
    Color::Rgb {
        r: 150,
        g: 150,
        b: 150,
    }
}

fn span_value_color(value: &str) -> Color {
    if matches!(value, "true" | "false") {
        Color::Rgb {
            r: 86,
            g: 156,
            b: 214,
        }
    } else if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
        Color::Rgb {
            r: 181,
            g: 206,
            b: 168,
        }
    } else {
        Color::Reset
    }
}

fn target_module_color(module: &str) -> Color {
    target_palette_color(stable_hash(module) % TARGET_PALETTE_SIZE)
}

const TARGET_PALETTE_SIZE: usize = 128;

fn target_palette_color(index: usize) -> Color {
    let hue = (index as f32 * 360.0 / TARGET_PALETTE_SIZE as f32) % 360.0;
    let saturation = 0.48 + ((index / 32) as f32 * 0.09);
    let lightness = 0.58 + ((index / 16) % 2) as f32 * 0.10;
    let (r, g, b) = hsl_to_rgb(hue, saturation.min(0.78), lightness.min(0.72));

    Color::Rgb { r, g, b }
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_sector = hue / 60.0;
    let x = chroma * (1.0 - (hue_sector % 2.0 - 1.0).abs());

    let (r1, g1, b1) = match hue_sector as u8 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };

    let m = lightness - chroma / 2.0;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

fn stable_hash(value: &str) -> usize {
    value.bytes().fold(2_166_136_261usize, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ byte as usize
    })
}

fn message_color(entry: &LogEntry) -> Color {
    match (entry.level, entry.stream) {
        (Level::Error, _) => Color::Red,
        (Level::Warn, _) => Color::Yellow,
        (Level::Unknown, Stream::Stderr) => Color::Yellow,
        _ => Color::White,
    }
}

fn scrollbar_slice_color(
    entries: &VecDeque<LogEntry>,
    visible_indices: &[usize],
    start: usize,
    end: usize,
) -> Color {
    visible_indices
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .filter_map(|idx| entries.get(*idx))
        .map(|entry| entry.level)
        .max_by_key(|level| level.severity())
        .map(level_scrollbar_color)
        .unwrap_or(Color::DarkGrey)
}

fn level_scrollbar_color(level: Level) -> Color {
    match level {
        Level::Error => Color::Red,
        Level::Warn => Color::Yellow,
        Level::Info => Color::Green,
        Level::Debug => Color::Cyan,
        Level::Trace => Color::DarkGrey,
        Level::Unknown => Color::DarkGrey,
    }
}

fn visible_slice(input: &str, offset: usize, width: usize) -> String {
    input.chars().skip(offset).take(width).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TraceValueSection;

    fn entries(count: usize) -> VecDeque<LogEntry> {
        (0..count)
            .map(|idx| LogEntry {
                raw: format!("raw line {idx}"),
                timestamp: None,
                level: Level::Info,
                parsed: true,
                target: None,
                spans: Vec::new(),
                values: Vec::new(),
                message: format!("line {idx}"),
                message_parts: Vec::new(),
                stream: Stream::Stdout,
            })
            .collect()
    }

    fn entry_with_level(level: Level) -> LogEntry {
        LogEntry {
            raw: "raw line".to_string(),
            timestamp: None,
            level,
            parsed: true,
            target: None,
            spans: Vec::new(),
            values: Vec::new(),
            message: "line".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }
    }

    fn entry_with_target(target: Option<&str>, message: &str) -> LogEntry {
        LogEntry {
            raw: format!("raw {message}"),
            timestamp: None,
            level: Level::Info,
            parsed: true,
            target: target.map(str::to_string),
            spans: Vec::new(),
            values: Vec::new(),
            message: message.to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }
    }

    fn entry_with_values(values: Vec<TraceValueField>) -> LogEntry {
        entry_with_value_sections(vec![TraceValueSection::new("event", values)])
    }

    fn entry_with_value_sections(values: Vec<TraceValueSection>) -> LogEntry {
        LogEntry {
            raw: "raw line".to_string(),
            timestamp: None,
            level: Level::Info,
            parsed: true,
            target: None,
            spans: Vec::new(),
            values,
            message: "line".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn renderer() -> EntryRenderer {
        EntryRenderer {
            options: RenderOptions {
                show_spans: true,
                show_raw: false,
                search_query: String::new(),
            },
        }
    }

    #[test]
    fn home_and_end_move_to_first_and_last_lines() {
        let entries = entries(10);
        let mut state = ViewState::new();

        handle_key(key(KeyCode::Home), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(0));

        handle_key(key(KeyCode::End), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(9));
    }

    #[test]
    fn page_keys_move_by_visible_page() {
        let entries = entries(20);
        let mut state = ViewState::new();

        assert_eq!(
            handle_key(key(KeyCode::PageUp), &entries, &mut state, false, 6),
            KeyAction::Continue
        );
        assert_eq!(state.selected, Some(14));

        handle_key(key(KeyCode::PageDown), &entries, &mut state, false, 6);
        assert_eq!(state.selected, Some(19));
    }

    #[test]
    fn number_keys_filter_by_minimum_level() {
        let entries = VecDeque::from([
            entry_with_level(Level::Trace),
            entry_with_level(Level::Debug),
            entry_with_level(Level::Info),
            entry_with_level(Level::Warn),
            entry_with_level(Level::Error),
        ]);
        let mut state = ViewState {
            selected: Some(0),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('3')), &entries, &mut state, false, 5);
        assert_eq!(state.level_filter, LevelFilter::AtLeast(Level::Info));
        assert_eq!(visible_indices(&entries, &state), vec![2, 3, 4]);
        assert_eq!(state.selected, Some(4));

        handle_key(key(KeyCode::Char('5')), &entries, &mut state, false, 5);
        assert_eq!(visible_indices(&entries, &state), vec![4]);
        assert_eq!(state.selected, Some(4));

        handle_key(key(KeyCode::Char('1')), &entries, &mut state, false, 5);
        assert_eq!(state.level_filter, LevelFilter::All);
        assert_eq!(visible_indices(&entries, &state), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn y_requests_copy_selected_line() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert_eq!(
            handle_key(key(KeyCode::Char('y')), &entries, &mut state, false, 5),
            KeyAction::CopySelected
        );
    }

    #[test]
    fn question_mark_toggles_help_page() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert_eq!(
            handle_key(key(KeyCode::Char('?')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(state.help_visible);

        assert_eq!(
            handle_key(key(KeyCode::Char('?')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(!state.help_visible);
    }

    #[test]
    fn compact_help_page_shows_raw_toggle() {
        let mut output = Vec::new();

        draw_help_page(&mut output, 80, 6).expect("draw help");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("r               toggle raw log line display"));
    }

    #[test]
    fn help_page_shows_level_filter_shortcuts() {
        let mut output = Vec::new();

        draw_help_page(&mut output, 80, 8).expect("draw help");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("1..5            filter all, debug+, info+, warn+, error"));
    }

    #[test]
    fn s_toggles_span_information() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert!(state.show_spans);
        assert_eq!(
            handle_key(key(KeyCode::Char('s')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(!state.show_spans);

        handle_key(key(KeyCode::Char('s')), &entries, &mut state, false, 5);
        assert!(state.show_spans);
    }

    #[test]
    fn r_toggles_raw_log_line_display() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert!(!state.show_raw);
        assert_eq!(
            handle_key(key(KeyCode::Char('r')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(state.show_raw);

        handle_key(key(KeyCode::Char('r')), &entries, &mut state, false, 5);
        assert!(!state.show_raw);
    }

    #[test]
    fn v_toggles_sidebar_and_uppercase_v_toggles_fullscreen() {
        let entries = VecDeque::from([entry_with_values(vec![TraceValueField::new(
            "id",
            TraceValue::Number("7".to_string()),
        )])]);
        let mut state = ViewState::new();

        assert_eq!(state.values.mode, ValuesPaneMode::Closed);
        assert_eq!(
            handle_key(key(KeyCode::Char('v')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert_eq!(state.values.mode, ValuesPaneMode::Sidebar);
        assert_eq!(state.values.selected, Some(0));

        handle_key(key(KeyCode::Char('V')), &entries, &mut state, false, 5);
        assert_eq!(state.values.mode, ValuesPaneMode::Fullscreen);

        handle_key(key(KeyCode::Char('v')), &entries, &mut state, false, 5);
        assert_eq!(state.values.mode, ValuesPaneMode::Sidebar);
        assert_eq!(state.values.selected, Some(0));

        handle_key(key(KeyCode::Char('v')), &entries, &mut state, false, 5);
        assert_eq!(state.values.mode, ValuesPaneMode::Closed);
        assert_eq!(state.values.selected, None);

        handle_key(key(KeyCode::Char('V')), &entries, &mut state, false, 5);
        assert_eq!(state.values.mode, ValuesPaneMode::Fullscreen);

        handle_key(key(KeyCode::Char('V')), &entries, &mut state, false, 5);
        assert_eq!(state.values.mode, ValuesPaneMode::Closed);
    }

    #[test]
    fn escape_closes_tracing_values_pane() {
        let entries = entries(1);
        let mut state = ViewState {
            values: ValuesPaneState {
                mode: ValuesPaneMode::Fullscreen,
                selected: Some(0),
                x_offset: 16,
            },
            search_query: "line".to_string(),
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Esc), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert_eq!(state.values.mode, ValuesPaneMode::Closed);
        assert_eq!(state.values.selected, None);
        assert_eq!(state.values.x_offset, 0);
        assert_eq!(state.search_query, "line");
    }

    #[test]
    fn arrow_keys_select_and_scroll_values_when_pane_is_open() {
        let entries = VecDeque::from([entry_with_values(vec![
            TraceValueField::new("id", TraceValue::Number("7".to_string())),
            TraceValueField::new("tag", TraceValue::String("admin".to_string())),
        ])]);
        let mut state = ViewState {
            selected: Some(0),
            values: ValuesPaneState {
                mode: ValuesPaneMode::Sidebar,
                ..ValuesPaneState::default()
            },
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.values.selected, Some(1));

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.values.selected, Some(0));

        handle_key(key(KeyCode::Right), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 0);
        assert_eq!(state.values.x_offset, 16);

        handle_key(key(KeyCode::Left), &entries, &mut state, false, 5);
        assert_eq!(state.values.x_offset, 0);
    }

    #[test]
    fn y_copies_selected_value_when_values_pane_is_open() {
        let entries = VecDeque::from([entry_with_values(vec![
            TraceValueField::new("id", TraceValue::Number("7".to_string())),
            TraceValueField::new("tag", TraceValue::String("admin".to_string())),
        ])]);
        let mut state = ViewState {
            selected: Some(0),
            values: ValuesPaneState {
                mode: ValuesPaneMode::Sidebar,
                selected: Some(1),
                ..ValuesPaneState::default()
            },
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Char('y')), &entries, &mut state, false, 5),
            KeyAction::CopySelected
        );
        assert_eq!(
            selected_line_text(&entries, &state).as_deref(),
            Some(r#"tag = "admin""#)
        );
    }

    #[test]
    fn slash_closes_values_pane_and_starts_search() {
        let entries = entries(1);
        let mut state = ViewState {
            values: ValuesPaneState {
                mode: ValuesPaneMode::Fullscreen,
                selected: Some(0),
                x_offset: 16,
            },
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('/')), &entries, &mut state, false, 5);

        assert_eq!(state.values.mode, ValuesPaneMode::Closed);
        assert!(state.search_editing);
    }

    #[test]
    fn values_pane_draws_stored_tracing_values() {
        let entry = entry_with_value_sections(vec![
            TraceValueSection::new(
                "scope: request",
                vec![TraceValueField::new(
                    "id",
                    TraceValue::Number("7".to_string()),
                )],
            ),
            TraceValueSection::new(
                "event",
                vec![TraceValueField::new(
                    "tag",
                    TraceValue::String("admin".to_string()),
                )],
            ),
        ]);
        let mut output = Vec::new();

        draw_values_pane(
            &mut output,
            Some(&entry),
            &ValuesPaneState::default(),
            PaneViewport {
                left: 0,
                top: 0,
                width: 40,
                height: 6,
            },
        )
        .expect("draw pane");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("Tracing values"));
        assert!(text.contains("scope: request"));
        assert!(text.contains("event"));
        assert!(text.contains("id = "));
        assert!(text.contains("7"));
        assert!(text.contains("tag = "));
        assert!(text.contains("\"admin\""));
        assert!(text.find("scope: request").unwrap() < text.find("event").unwrap());
        assert!(matches!(
            values_pane_row(&entry, 2),
            Some(ValuesPaneRow::Spacer)
        ));
    }

    #[test]
    fn slash_starts_raw_search_and_typing_updates_matches() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "apple"),
            entry_with_target(Some("beta"), "berry"),
            entry_with_target(Some("gamma"), "cherry"),
        ]);
        let mut state = ViewState {
            selected: Some(0),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('/')), &entries, &mut state, false, 5);
        assert!(state.search_editing);
        assert_eq!(state.search_query, "");

        handle_key(key(KeyCode::Char('e')), &entries, &mut state, false, 5);
        assert_eq!(state.search_query, "e");
        assert_eq!(state.selected, Some(0));

        handle_key(key(KeyCode::Char('r')), &entries, &mut state, false, 5);
        assert_eq!(state.search_query, "er");
        assert_eq!(state.selected, Some(1));
        assert_eq!(search_match_indices(&entries, &state), vec![1, 2]);

        let status = status_line(&entries, &state, None, false, 120);
        assert!(status.contains("search /er (2 results)"));
    }

    #[test]
    fn search_bar_shows_input_state_and_result_count() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "miss"),
            entry_with_target(Some("beta"), "hit one"),
            entry_with_target(Some("gamma"), "hit two"),
        ]);
        let state = ViewState {
            search_editing: true,
            search_query: "hit".to_string(),
            ..ViewState::new()
        };
        let mut output = Vec::new();

        draw_search_bar(&mut output, &entries, &state, 100).expect("draw search bar");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("Search(*): hit_"));
        assert!(text.contains("2 results"));
        assert!(text.contains("Enter accept"));
        assert!(text.contains("Esc clear"));
        assert!(text.contains("n next"));
        assert!(text.contains("b previous"));
        assert!(text.find("Enter accept").unwrap() < text.find("2 results").unwrap());
    }

    #[test]
    fn search_bar_colors_distinguish_editing_from_locked() {
        assert_eq!(
            search_bar_colors(true),
            (
                Color::Black,
                Color::Rgb {
                    r: 150,
                    g: 205,
                    b: 255
                }
            )
        );
        assert_eq!(search_bar_colors(false), (Color::Black, Color::White));
    }

    #[test]
    fn status_bar_uses_high_contrast_neutral_tone() {
        assert_eq!(status_bar_foreground(), Color::Black);
        assert_eq!(status_bar_background(), Color::White);
    }

    #[test]
    fn locked_search_bar_only_shows_normal_mode_shortcuts() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "miss"),
            entry_with_target(Some("beta"), "hit one"),
        ]);
        let state = ViewState {
            search_query: "hit".to_string(),
            ..ViewState::new()
        };
        let mut output = Vec::new();

        draw_search_bar(&mut output, &entries, &state, 100).expect("draw search bar");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("Search: hit"));
        assert!(text.contains("/ edit"));
        assert!(text.contains("n next"));
        assert!(text.contains("b previous"));
        assert!(text.contains("Esc clear"));
        assert!(!text.contains("Enter accept"));
        assert!(text.find("/ edit").unwrap() < text.find("1 result").unwrap());
    }

    #[test]
    fn search_bar_does_not_predict_wraps_at_first_or_last_result() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "hit one"),
            entry_with_target(Some("beta"), "miss"),
            entry_with_target(Some("gamma"), "hit two"),
        ]);

        let first = ViewState {
            selected: Some(0),
            search_query: "hit".to_string(),
            ..ViewState::new()
        };
        let mut first_output = Vec::new();
        draw_search_bar(&mut first_output, &entries, &first, 120).expect("draw search bar");
        let first_text = String::from_utf8(first_output).expect("utf8");
        assert!(first_text.contains("2 results  1/2"));
        assert!(!first_text.contains("wrap"));

        let last = ViewState {
            selected: Some(2),
            search_query: "hit".to_string(),
            ..ViewState::new()
        };
        let mut last_output = Vec::new();
        draw_search_bar(&mut last_output, &entries, &last, 120).expect("draw search bar");
        let last_text = String::from_utf8(last_output).expect("utf8");
        assert!(last_text.contains("2 results  2/2"));
        assert!(!last_text.contains("wrap"));
    }

    #[test]
    fn search_bar_shows_wrapped_after_actual_wrap() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "hit one"),
            entry_with_target(Some("beta"), "miss"),
            entry_with_target(Some("gamma"), "hit two"),
        ]);
        let mut state = ViewState {
            selected: Some(2),
            search_query: "hit".to_string(),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('n')), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(0));
        assert!(state.search_wrapped);

        let mut output = Vec::new();
        draw_search_bar(&mut output, &entries, &state, 120).expect("draw search bar");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("2 results  1/2  wrapped"));
    }

    #[test]
    fn search_bar_does_not_note_single_result_until_wrap_happens() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "miss"),
            entry_with_target(Some("beta"), "hit one"),
        ]);
        let mut state = ViewState {
            selected: Some(1),
            search_query: "hit".to_string(),
            ..ViewState::new()
        };
        let mut output = Vec::new();

        draw_search_bar(&mut output, &entries, &state, 120).expect("draw search bar");
        let text = String::from_utf8(output).expect("utf8");

        assert!(text.contains("1 result  1/1"));
        assert!(!text.contains("wrapped"));

        handle_key(key(KeyCode::Char('n')), &entries, &mut state, false, 5);
        let mut wrapped_output = Vec::new();
        draw_search_bar(&mut wrapped_output, &entries, &state, 120).expect("draw search bar");
        let wrapped_text = String::from_utf8(wrapped_output).expect("utf8");

        assert!(wrapped_text.contains("1 result  1/1  wrapped"));
    }

    #[test]
    fn search_bar_reduces_log_content_rows() {
        let inactive = ViewState::new();
        let active = ViewState {
            search_query: "hit".to_string(),
            ..ViewState::new()
        };

        assert_eq!(content_rows(10, &inactive), 9);
        assert_eq!(content_rows(10, &active), 8);
    }

    #[test]
    fn enter_finishes_search_input_and_escape_clears_search() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "apple"),
            entry_with_target(Some("beta"), "berry"),
        ]);
        let mut state = ViewState::new();

        handle_key(key(KeyCode::Char('/')), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Char('b')), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Enter), &entries, &mut state, false, 5);

        assert!(!state.search_editing);
        assert_eq!(state.search_query, "b");
        assert_eq!(state.selected, Some(1));

        handle_key(key(KeyCode::Char('/')), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Char('a')), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Esc), &entries, &mut state, false, 5);

        assert!(!state.search_editing);
        assert_eq!(state.search_query, "");
    }

    #[test]
    fn slash_edits_existing_locked_search() {
        let entries = VecDeque::from([entry_with_target(Some("alpha"), "apple")]);
        let mut state = ViewState {
            search_query: "app".to_string(),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('/')), &entries, &mut state, false, 5);
        assert!(state.search_editing);
        assert_eq!(state.search_query, "app");

        handle_key(key(KeyCode::Char('l')), &entries, &mut state, false, 5);
        assert_eq!(state.search_query, "appl");
    }

    #[test]
    fn escape_clears_locked_search_without_quitting() {
        let entries = VecDeque::from([entry_with_target(Some("alpha"), "apple")]);
        let mut state = ViewState {
            search_query: "apple".to_string(),
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Esc), &entries, &mut state, true, 5),
            KeyAction::Continue
        );
        assert_eq!(state.search_query, "");
        assert!(!state.search_editing);
    }

    #[test]
    fn n_and_b_jump_between_search_results_with_wraparound() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "miss"),
            entry_with_target(Some("beta"), "hit one"),
            entry_with_target(Some("gamma"), "miss again"),
            entry_with_target(Some("delta"), "hit two"),
        ]);
        let mut state = ViewState {
            selected: Some(0),
            search_query: "hit".to_string(),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('n')), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(1));
        assert!(!state.search_wrapped);

        handle_key(key(KeyCode::Char('n')), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(3));
        assert!(!state.search_wrapped);

        handle_key(key(KeyCode::Char('n')), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(1));
        assert!(state.search_wrapped);

        handle_key(key(KeyCode::Char('b')), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(3));
        assert!(state.search_wrapped);
    }

    #[test]
    fn search_uses_raw_line_not_rendered_message() {
        let entries = VecDeque::from([LogEntry {
            raw: "2026-06-15T12:01:02Z INFO svc: original raw text".to_string(),
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("svc".to_string()),
            spans: Vec::new(),
            values: Vec::new(),
            message: "parsed message".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }]);
        let state = ViewState {
            search_query: "original raw".to_string(),
            ..ViewState::new()
        };

        assert_eq!(search_match_indices(&entries, &state), vec![0]);
    }

    #[test]
    fn raw_search_match_is_highlighted_in_raw_display() {
        let entry = LogEntry {
            raw: "2026-06-15T12:01:02Z INFO svc: original raw text".to_string(),
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("svc".to_string()),
            spans: Vec::new(),
            values: Vec::new(),
            message: "parsed message".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        };
        let state = ViewState {
            show_raw: true,
            search_query: "original raw".to_string(),
            ..ViewState::new()
        };

        let parts = EntryRenderer::from(&state).parts(&entry);
        let highlighted_text: String = parts
            .iter()
            .filter(|part| part.highlighted)
            .map(|part| part.text.as_str())
            .collect();

        assert!(highlighted_text.contains("original raw"));
    }

    #[test]
    fn raw_search_highlights_default_sized_line_with_many_matches_compactly() {
        let entry = LogEntry {
            raw: "a".repeat(65_536),
            timestamp: None,
            level: Level::Unknown,
            parsed: false,
            target: None,
            spans: Vec::new(),
            values: Vec::new(),
            message: "a".repeat(65_536),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        };
        let state = ViewState {
            show_raw: true,
            search_query: "a".to_string(),
            ..ViewState::new()
        };

        let parts = EntryRenderer::from(&state).parts(&entry);
        let highlighted: Vec<_> = parts.iter().filter(|part| part.highlighted).collect();

        assert_eq!(highlighted.len(), 1);
        assert_eq!(highlighted[0].text.len(), 65_536);
    }

    #[test]
    fn visible_search_match_is_highlighted_in_parsed_display() {
        let entry = entry_with_target(Some("svc"), "loaded widgets");
        let state = ViewState {
            search_query: "widgets".to_string(),
            ..ViewState::new()
        };

        let parts = EntryRenderer::from(&state).parts(&entry);

        assert!(
            parts
                .iter()
                .any(|part| part.text == "widgets" && part.highlighted)
        );
    }

    #[test]
    fn f_focuses_selected_target_and_clears_focus() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "one"),
            entry_with_target(Some("beta"), "two"),
            entry_with_target(Some("alpha"), "three"),
        ]);
        let mut state = ViewState {
            selected: Some(2),
            first_visible: 2,
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Char('f')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert_eq!(state.focus_target.as_deref(), Some("alpha"));
        assert_eq!(visible_indices(&entries, &state), vec![0, 2]);
        assert_eq!(state.selected, Some(2));

        handle_key(key(KeyCode::Char('f')), &entries, &mut state, false, 5);
        assert_eq!(state.focus_target, None);
        assert_eq!(visible_indices(&entries, &state), vec![0, 1, 2]);
    }

    #[test]
    fn focused_navigation_skips_other_targets() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "one"),
            entry_with_target(Some("beta"), "two"),
            entry_with_target(Some("alpha"), "three"),
            entry_with_target(Some("beta"), "four"),
        ]);
        let mut state = ViewState {
            selected: Some(0),
            focus_target: Some("alpha".to_string()),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(2));

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn focus_status_shows_hidden_line_count_and_percentage() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "one"),
            entry_with_target(Some("beta"), "two"),
            entry_with_target(Some("alpha"), "three"),
            entry_with_target(None, "plain"),
        ]);
        let state = ViewState {
            selected: Some(2),
            focus_target: Some("alpha".to_string()),
            ..ViewState::new()
        };

        let status = status_line(&entries, &state, None, false, 120);

        assert!(status.contains("focus alpha (2 hidden, 50%)"));
    }

    #[test]
    fn status_shows_level_distribution_and_filter() {
        let entries = VecDeque::from([
            entry_with_level(Level::Error),
            entry_with_level(Level::Warn),
            entry_with_level(Level::Info),
            entry_with_level(Level::Debug),
            entry_with_level(Level::Trace),
            entry_with_level(Level::Unknown),
        ]);
        let state = ViewState {
            level_filter: LevelFilter::AtLeast(Level::Warn),
            ..ViewState::new()
        };

        let status = status_line(&entries, &state, None, false, 160);

        assert!(status.contains("lvl E1 W1 I1 D1 T1 U1"));
        assert!(status.contains("| WARN+ |"));
    }

    #[test]
    fn status_shows_loaded_after_file_input_finishes() {
        let entries = entries(1);
        let state = ViewState::new();

        let status = status_line(&entries, &state, None, true, 120);

        assert!(status.contains(" loaded |"));
    }

    #[test]
    fn f_without_selected_target_keeps_focus_clear() {
        let entries = VecDeque::from([entry_with_target(None, "plain")]);
        let mut state = ViewState {
            selected: Some(0),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('f')), &entries, &mut state, false, 5);
        assert_eq!(state.focus_target, None);
    }

    #[test]
    fn left_and_right_scroll_horizontally_by_sixteen_columns() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert_eq!(state.x_offset, 0);

        handle_key(key(KeyCode::Right), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 16);

        handle_key(key(KeyCode::Right), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 32);

        handle_key(key(KeyCode::Left), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 16);
    }

    #[test]
    fn help_page_ignores_navigation_until_closed() {
        let entries = entries(10);
        let mut state = ViewState {
            help_visible: true,
            selected: Some(9),
            first_visible: 5,
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Up), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert_eq!(state.selected, Some(9));
        assert!(state.help_visible);

        handle_key(key(KeyCode::Esc), &entries, &mut state, false, 5);
        assert!(!state.help_visible);
    }

    #[test]
    fn selected_line_text_matches_rendered_plain_text() {
        let entries = VecDeque::from([LogEntry {
            raw: "2026-06-15T12:01:02Z INFO my_crate::worker: request{id=7}: loaded \"user\""
                .to_string(),
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("my_crate::worker".to_string()),
            spans: vec!["request{id=7}".to_string()],
            values: Vec::new(),
            message: "loaded \"user\"".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }]);
        let state = ViewState {
            selected: Some(0),
            ..ViewState::new()
        };

        assert_eq!(
            selected_line_text(&entries, &state).as_deref(),
            Some("| 2026-06-15T12:01:02Z INFO  my_crate::worker: request{id=7}: loaded \"user\"")
        );
    }

    #[test]
    fn selected_line_text_omits_spans_when_hidden() {
        let entries = VecDeque::from([LogEntry {
            raw: "2026-06-15T12:01:02Z INFO my_crate::worker: request{id=7}: loaded \"user\""
                .to_string(),
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("my_crate::worker".to_string()),
            spans: vec!["request{id=7}".to_string()],
            values: Vec::new(),
            message: "loaded \"user\"".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }]);
        let state = ViewState {
            selected: Some(0),
            show_spans: false,
            ..ViewState::new()
        };

        assert_eq!(
            selected_line_text(&entries, &state).as_deref(),
            Some("| 2026-06-15T12:01:02Z INFO  my_crate::worker: loaded \"user\"")
        );
    }

    #[test]
    fn selected_line_text_uses_raw_line_when_enabled() {
        let entries = VecDeque::from([LogEntry {
            raw: "2026-06-15T12:01:02Z INFO my_crate::worker: request{id=7}: loaded \"user\""
                .to_string(),
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("my_crate::worker".to_string()),
            spans: vec!["request{id=7}".to_string()],
            values: Vec::new(),
            message: "loaded \"user\"".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }]);
        let state = ViewState {
            selected: Some(0),
            show_raw: true,
            ..ViewState::new()
        };

        assert_eq!(
            selected_line_text(&entries, &state).as_deref(),
            Some("| 2026-06-15T12:01:02Z INFO my_crate::worker: request{id=7}: loaded \"user\"")
        );
    }

    #[test]
    fn cursor_moves_on_screen_before_scrolling_up() {
        let entries = entries(10);
        let mut state = ViewState::new();
        state.follow_latest(&entries, 5);

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(8));
        assert_eq!(state.first_visible, 5);

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.first_visible, 5);

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(4));
        assert_eq!(state.first_visible, 4);
    }

    #[test]
    fn cursor_moves_on_screen_before_scrolling_down() {
        let entries = entries(10);
        let mut state = ViewState {
            first_visible: 2,
            selected: Some(2),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(3));
        assert_eq!(state.first_visible, 2);

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(6));
        assert_eq!(state.first_visible, 2);

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(7));
        assert_eq!(state.first_visible, 3);
    }

    #[test]
    fn line_viewport_marks_hidden_content_on_the_right() {
        assert_eq!(
            LineViewport::new(20, 0, 10),
            LineViewport {
                content_width: 9,
                show_left_marker: false,
                show_right_marker: true,
            }
        );
    }

    #[test]
    fn line_viewport_marks_hidden_content_on_both_sides() {
        assert_eq!(
            LineViewport::new(20, 5, 10),
            LineViewport {
                content_width: 8,
                show_left_marker: true,
                show_right_marker: true,
            }
        );
    }

    #[test]
    fn line_viewport_omits_markers_when_line_fits() {
        assert_eq!(
            LineViewport::new(8, 0, 10),
            LineViewport {
                content_width: 10,
                show_left_marker: false,
                show_right_marker: false,
            }
        );
    }

    #[test]
    fn scrollbar_slice_maps_row_to_entry_range() {
        assert_eq!(
            ScrollbarSlice::new(0, 5, 10),
            ScrollbarSlice { start: 0, end: 2 }
        );
        assert_eq!(
            ScrollbarSlice::new(2, 5, 10),
            ScrollbarSlice { start: 4, end: 6 }
        );
        assert_eq!(
            ScrollbarSlice::new(4, 5, 10),
            ScrollbarSlice { start: 8, end: 10 }
        );
    }

    #[test]
    fn scrollbar_slice_color_uses_highest_severity() {
        let entries = VecDeque::from([
            entry_with_level(Level::Info),
            entry_with_level(Level::Debug),
            entry_with_level(Level::Warn),
            entry_with_level(Level::Error),
        ]);

        let visible = [0, 1, 2, 3];

        assert_eq!(
            scrollbar_slice_color(&entries, &visible, 0, 2),
            Color::Green
        );
        assert_eq!(
            scrollbar_slice_color(&entries, &visible, 0, 3),
            Color::Yellow
        );
        assert_eq!(scrollbar_slice_color(&entries, &visible, 0, 4), Color::Red);
    }

    #[test]
    fn target_module_colors_are_stable_for_same_module() {
        assert_eq!(target_module_color("worker"), target_module_color("worker"));
    }

    #[test]
    fn target_palette_has_128_slots() {
        assert_eq!(TARGET_PALETTE_SIZE, 128);
        assert_ne!(target_palette_color(0), target_palette_color(127));
    }

    #[test]
    fn target_parts_split_rust_modules_and_keep_separators_neutral() {
        let mut parts = Vec::new();
        renderer().push_target_parts(&mut parts, "my_crate::worker::db");

        let text = EntryRenderer::plain_text_from_parts(&parts);
        assert_eq!(text, "my_crate::worker::db: ");
        assert_eq!(parts[1], Part::new("::", Color::DarkGrey, false));
        assert_eq!(parts[3], Part::new("::", Color::DarkGrey, false));
        assert_eq!(parts[5], Part::new(": ", Color::DarkGrey, false));
    }

    #[test]
    fn target_parts_do_not_split_modules_after_first_whitespace() {
        let mut parts = Vec::new();
        renderer().push_target_parts(&mut parts, "my_crate::worker span{path=other::module}");

        let text = EntryRenderer::plain_text_from_parts(&parts);
        assert_eq!(text, "my_crate::worker span{path=other::module}: ");
        assert_eq!(
            parts[3],
            Part::new(
                " span{path=other::module}".to_string(),
                Color::DarkGrey,
                false
            )
        );
    }

    #[test]
    fn message_parts_highlight_quoted_strings() {
        let mut parts = Vec::new();
        renderer().push_message_parts(
            &mut parts,
            "loaded \"user 42\" from cache",
            &[],
            Color::White,
        );

        assert_eq!(parts[0], Part::new("loaded ", Color::White, false));
        assert_eq!(parts[1], Part::new("\"user 42\"", string_color(), false));
        assert_eq!(parts[2], Part::new(" from cache", Color::White, false));
    }

    #[test]
    fn message_parts_keep_escaped_quotes_inside_string() {
        let mut parts = Vec::new();
        renderer().push_message_parts(
            &mut parts,
            "loaded \"user \\\"jonas\\\"\"",
            &[],
            Color::White,
        );

        assert_eq!(parts[0], Part::new("loaded ", Color::White, false));
        assert_eq!(
            parts[1],
            Part::new("\"user \\\"jonas\\\"\"", string_color(), false)
        );
    }

    #[test]
    fn structured_message_parts_use_jq_style_colors() {
        let message_parts = vec![
            MessagePart::text("au revoir"),
            MessagePart::fields(vec![
                TraceValueField::new("lang", TraceValue::String("fr".to_string())),
                TraceValueField::new("ok", TraceValue::Bool(true)),
                TraceValueField::new("count", TraceValue::Number("7".to_string())),
                TraceValueField::new("none", TraceValue::Null),
            ]),
        ];
        let mut parts = Vec::new();

        renderer().push_message_parts(&mut parts, "", &message_parts, Color::White);

        assert_eq!(
            EntryRenderer::plain_text_from_parts(&parts),
            r#"au revoir (lang="fr" ok=true count=7 none=null)"#
        );
        assert_eq!(parts[0], Part::new("au revoir", Color::White, false));
        assert_eq!(parts[2], Part::new("lang", Color::Blue, true));
        assert_eq!(parts[4], Part::new("\"fr\"", Color::Green, false));
        assert_eq!(parts[8], Part::new("true", Color::Reset, false));
        assert_eq!(parts[12], Part::new("7", Color::Reset, false));
        assert_eq!(parts[16], Part::new("null", Color::DarkGrey, false));
    }

    #[test]
    fn span_parts_use_span_specific_colors() {
        let spans = vec![
            "request{id=7}".to_string(),
            "db{query=\"select\"}".to_string(),
        ];
        let mut parts = Vec::new();

        renderer().push_span_parts(&mut parts, &spans);

        assert_eq!(
            parts[0],
            Part::new("request", span_name_color("request"), false)
        );
        assert_eq!(parts[1], Part::new("{", span_punctuation_color(), false));
        assert_eq!(parts[2], Part::new("id", span_key_color(), false));
        assert_eq!(parts[3], Part::new("=", span_punctuation_color(), false));
        assert_eq!(parts[4], Part::new("7", span_value_color("7"), false));
        assert_eq!(parts[6], Part::new(": ", Color::DarkGrey, false));
        assert_eq!(parts[7], Part::new("db", span_name_color("db"), false));
        assert_eq!(parts[10], Part::new("=", span_punctuation_color(), false));
        assert_eq!(parts[11], Part::new("\"select\"", string_color(), false));
    }

    #[test]
    fn span_parts_render_bare_spans_as_span_names() {
        let spans = vec!["load_graphs".to_string(), "load_graphs_inner".to_string()];
        let mut parts = Vec::new();

        renderer().push_span_parts(&mut parts, &spans);

        assert_eq!(
            parts[0],
            Part::new(
                "load_graphs".to_string(),
                span_name_color("load_graphs"),
                false
            )
        );
        assert_eq!(parts[1], Part::new(": ", Color::DarkGrey, false));
        assert_eq!(
            parts[2],
            Part::new(
                "load_graphs_inner".to_string(),
                span_name_color("load_graphs_inner"),
                false
            )
        );
    }

    #[test]
    fn span_palette_is_separate_from_target_palette() {
        assert_eq!(SPAN_PALETTE_SIZE, 64);
        assert_ne!(span_name_color("request"), target_module_color("request"));
    }
}
