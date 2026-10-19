use std::ops::Range;

use unicode_width::UnicodeWidthStr;

use crate::agents::{self, AgentRow, AgentSet, Status};
use crate::checkouts::Checkout;
use crate::dirs::{short_cwd, DirRow, DirSet, Listing};
use crate::fetch::Fetch;
use crate::layout::{
    anchor, truncate, truncate_left, Border, Ink, Line, Rect, Screen, PAD, VERTICAL,
};
use crate::menu::{Entry, Keys, Menu, Shown, SidebarMenu};
use crate::paint::{self, Colour, Painted};
use crate::panes::{self, Peek, Peeks};
use crate::sessions::{Contents, Kind, MatchSet, Row};
use crate::tree::{Node, PaneNode, SessionNode, Summary, Tree};

const TAG: Colour = Colour::Level0;
const NAME: Colour = Colour::Level1;
const LABEL: Colour = Colour::Level2;
const ACCENT: Colour = Colour::Level3;

const TITLE: &str = "luneta";

const CARET: usize = 2;

fn gutter(line: &mut Line, selected: bool) {
    line.push(if selected { ">" } else { " " }, ACCENT);
    line.gap(1);
}

const GAP: usize = 2;

struct Note {
    text: String,
    error: bool,
}

impl Note {
    fn dim(text: impl Into<String>) -> Self {
        Self { text: text.into(), error: false }
    }

    fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), error: true }
    }
}

pub fn render_search(
    state: &MatchSet,
    peeks: &Peeks,
    error: Option<&str>,
    rows: usize,
    cols: usize,
) {
    let screen = Screen::new(rows, cols);
    let notes = note_texts(error);

    if let Some(rect) = &screen.results {
        let body = search_body(state, rect, notes.len());
        let right = count(state.rows.selected(), state.rows.len());
        draw(rect, TITLE, &right, interior(rect, &notes, body));
    }
    if let Some(rect) = &screen.preview {
        let (title, right, lines) = session_preview(state, peeks, rect);
        draw_preview(rect, &title, &right, lines);
    }
    draw_input(&screen, "Sessions", prompt_text(state));
    draw_help(&screen, &search_help(help_width(cols)));
}

pub fn render_dirs(dirs: &DirSet, term: &str, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);
    let notes = dir_note_texts(dirs);

    if let Some(rect) = &screen.results {
        let body = dir_body(dirs, term, rect, notes.len());
        let right = count(dirs.rows.selected(), dirs.rows.len());
        draw(rect, TITLE, &right, interior(rect, &notes, body));
    }
    if let Some(rect) = &screen.preview {
        let (title, right, lines) = dir_preview(dirs, rect);
        draw_preview(rect, &title, &right, lines);
    }
    draw_input(&screen, "Directories", dir_prompt(dirs, term));
    draw_help(&screen, &dirs_help(help_width(cols)));
}

pub fn render_agents(
    agents: &AgentSet,
    peeks: &Peeks,
    term: &str,
    error: Option<&str>,
    rows: usize,
    cols: usize,
    frame: u64,
) {
    let screen = Screen::new(rows, cols);
    let mut notes = note_texts(error);
    notes.extend(agent_note_texts(agents, help_width(cols)));

    if let Some(rect) = &screen.results {
        let body = agent_body(agents, term, rect, notes.len(), frame);
        let right = count(agents.rows.selected(), agents.rows.len());
        draw(rect, TITLE, &right, interior(rect, &notes, body));
    }
    if let Some(rect) = &screen.preview {
        let (title, lines) = agent_preview(agents, peeks, rect);
        draw_preview(rect, &title, "", lines);
    }
    draw_input(&screen, "Agents", agent_prompt(agents, term));
    draw_help(&screen, &agents_help(help_width(cols)));
}

pub fn render_rename(current: &str, input: &str, error: Option<&str>, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);

    if let Some(rect) = &screen.full {
        let notes = vec![Note::dim(format!("renaming \"{current}\" — the session you are in"))];
        draw(rect, "Rename", "", interior(rect, &notes, Vec::new()));
    }
    let (action, is_error) = match error {
        Some(error) => (error, true),
        None => ("Rename", false),
    };
    draw_input(&screen, "Rename", (input.to_owned(), Some(action.to_owned()), is_error));
    draw_help(
        &screen,
        &keys_text(
            help_width(cols),
            &[("<ENTER>", "Rename", "Rename"), ("<ESC>", "Cancel", "Cancel")],
        ),
    );
}

type Prompt = (String, Option<String>, bool);

fn help_width(cols: usize) -> usize {
    cols.saturating_sub(PAD * 2)
}

fn print_at(text: &Painted, x: usize, y: usize, width: usize) {
    paint::print_at(text, x, y, width);
}

fn draw(rect: &Rect, title: &str, right: &str, interior: Vec<Painted>) {
    print_at(&border_text(rect.top(title, right)), rect.x, rect.y, rect.width);
    for (i, line) in interior.into_iter().enumerate() {
        draw_row(rect, rect.inner_y() + i, &line);
    }
    print_at(&Painted::new(rect.bottom()).dim_all(), rect.x, rect.bottom_y(), rect.width);
}

fn draw_row(rect: &Rect, y: usize, row: &Painted) {
    let Some(inner) = rect.width.checked_sub(2) else {
        return;
    };
    let edge = || Painted::new(VERTICAL.to_string()).dim_all();
    print_at(&edge(), rect.x, y, 1);
    print_at(row, rect.x + 1, y, inner);
    print_at(&edge(), rect.x + rect.width - 1, y, 1);
}

fn border_text(border: Border) -> Painted {
    let rule = border.rule_indices();
    let Border { line, title, right } = border;
    let mut text = Painted::new(line).dim_indices(rule);
    if let Some(range) = title {
        text = text.color_range(LABEL, range);
    }
    if let Some(range) = right {
        text = text.unbold_range(range.clone()).color_range(TAG, range);
    }
    text
}

fn interior(rect: &Rect, notes: &[Note], body: Vec<Painted>) -> Vec<Painted> {
    let block = anchor(rect, notes.len(), body.len());
    let mut lines: Vec<Painted> = (rect.inner_y()..block.y).map(|_| blank_line(rect)).collect();
    lines.extend(notes.iter().take(block.notes).map(|note| note_line(rect, note)));
    lines.extend(body.into_iter().take(block.rows));
    lines
}

fn note_line(rect: &Rect, note: &Note) -> Painted {
    note_within(rect.inner_width(), note)
}

fn note_within(inner: usize, note: &Note) -> Painted {
    let mut line = Line::new();
    let text = truncate(&note.text, inner);
    if note.error {
        line.push_error(&text);
    } else {
        line.push(&text, TAG);
    }
    line.finish(inner)
}

fn input_line(rect: &Rect, prompt: Prompt) -> Painted {
    let (input, action, is_error) = prompt;
    let inner = rect.inner_width();
    let mut line = Line::new();
    line.push("> ", TAG);

    let (typed, _) = truncate_left(&format!("{input}_"), inner.saturating_sub(2));
    line.push(&typed, ACCENT);

    let room = inner.saturating_sub(line.columns() + GAP);
    if let Some(action) = action.filter(|_| room >= MIN_ACTION) {
        let action = truncate(&format!("<ENTER> {action}"), room);
        line.pad_to(inner - action.width());
        if is_error {
            line.push_error(&action);
        } else {
            line.push(&action, NAME);
        }
    }
    line.finish(inner)
}

const MIN_ACTION: usize = 12;

fn draw_input(screen: &Screen, title: &str, prompt: Prompt) {
    let rect = &screen.input;
    if !screen.bordered {
        print_at(&input_line(rect, prompt), rect.x, rect.y, rect.width);
        return;
    }
    draw(rect, title, "", vec![input_line(rect, prompt)]);
}

fn draw_help(screen: &Screen, help: &Painted) {
    if let Some(y) = screen.help_y {
        print_at(help, PAD, y, screen.input.width.saturating_sub(PAD));
    }
}

fn count(selected: Option<usize>, total: usize) -> String {
    match selected {
        Some(index) if total > 0 => format!("{}/{}", index + 1, total),
        _ if total > 0 => total.to_string(),
        _ => String::new(),
    }
}

fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = if selected < visible { 0 } else { (selected + 1).saturating_sub(visible) };
    (start, (start + visible).min(total))
}

fn blank_line(rect: &Rect) -> Painted {
    // Spaces only: dimming them costs an index per column and shows nothing.
    Painted::new(rect.blank())
}

enum PreviewRow {
    Own(Painted),
    Pane(String),
}

impl From<Painted> for PreviewRow {
    fn from(text: Painted) -> Self {
        PreviewRow::Own(text)
    }
}

impl PreviewRow {
    #[cfg(test)]
    fn content(&self) -> &str {
        match self {
            PreviewRow::Own(text) => text.content(),
            PreviewRow::Pane(line) => line,
        }
    }
}

/// Fits a dumped line to the interior of `rect` and pads it out to exactly the
/// full width of the row, margins included. `draw_pane_row` prints the result
/// verbatim — this is the only place the line is measured or cut.
fn pane_row(rect: &Rect, line: &str) -> String {
    let inner = rect.inner_width();
    let (line, used) = panes::fitted(line, inner);
    let mut out = String::with_capacity(line.len() + (inner - used) + 2);
    out.push(' ');
    out.push_str(&line);
    out.extend(std::iter::repeat_n(' ', inner - used));
    out.push(' ');
    out
}

fn draw_preview(rect: &Rect, title: &str, right: &str, lines: Vec<PreviewRow>) {
    print_at(&border_text(rect.top(title, right)), rect.x, rect.y, rect.width);
    for (i, row) in filled(rect, lines).into_iter().enumerate() {
        let y = rect.inner_y() + i;
        match row {
            PreviewRow::Own(text) => draw_row(rect, y, &text),
            PreviewRow::Pane(line) => draw_pane_row(rect, y, &line),
        }
    }
    print_at(&Painted::new(rect.bottom()).dim_all(), rect.x, rect.bottom_y(), rect.width);
}

fn draw_pane_row(rect: &Rect, y: usize, line: &str) {
    if rect.width < 2 {
        return;
    }
    let edge = || Painted::new(VERTICAL.to_string()).dim_all();
    print_at(&edge(), rect.x, y, 1);
    print!("\u{1b}[{};{}H\u{1b}[m{}\u{1b}[m", y + 1, rect.x + 2, line);
    print_at(&edge(), rect.x + rect.width - 1, y, 1);
}

fn filled(rect: &Rect, mut lines: Vec<PreviewRow>) -> Vec<PreviewRow> {
    let height = rect.inner_height();
    if lines.len() > height {
        let hidden = lines.len() - height + 1;
        lines.truncate(height.saturating_sub(1));
        lines.push(note_line(rect, &Note::dim(format!("… {hidden} more"))).into());
    }
    lines.resize_with(height, || blank_line(rect).into());
    lines
}

fn preview_line(inner: usize, text: &str, colour: Colour) -> PreviewRow {
    let mut line = Line::new();
    line.push(&truncate(text, inner), colour);
    line.finish(inner).into()
}

fn wrapped_lines(inner: usize, text: &str, colour: Colour) -> Vec<PreviewRow> {
    wrap(text, inner).iter().map(|line| preview_line(inner, line, colour)).collect()
}

fn error_lines(inner: usize, text: &str) -> Vec<PreviewRow> {
    wrap(text, inner)
        .iter()
        .map(|text| {
            let mut line = Line::new();
            line.push_error(text);
            line.finish(inner).into()
        })
        .collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = truncate(word, width);
        match lines.last_mut() {
            Some(line) if line.width() + 1 + word.width() <= width => {
                line.push(' ');
                line.push_str(&word);
            },
            _ => lines.push(word),
        }
    }
    lines
}

fn nothing_highlighted(rect: &Rect) -> (String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    (
        "Preview".to_owned(),
        vec![
            preview_line(inner, "nothing highlighted", TAG),
            blank_line(rect).into(),
            preview_line(inner, "Enter takes what you type", TAG),
        ],
    )
}

fn session_preview(
    state: &MatchSet,
    peeks: &Peeks,
    rect: &Rect,
) -> (String, String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    let Some(row) = state.rows.selected_row() else {
        let (title, lines) = nothing_highlighted(rect);
        return (title, String::new(), lines);
    };
    let contents = state.contents.get(&row.name);
    let right = match (row.kind, contents) {
        (Kind::Live, Some(contents)) => plural(contents.panes, "pane"),
        _ => String::new(),
    };
    let lines = match (row.kind, contents) {
        (Kind::Live, Some(contents)) => live_preview(rect, peeks, &row.name, contents),
        (Kind::Live, None) => {
            wrapped_lines(inner, "no detail yet — the session has not reported what is in it", TAG)
        },
        (Kind::Resurrectable, _) => dead_preview(rect),
    };
    (row.name.clone(), right, lines)
}

fn live_preview(rect: &Rect, peeks: &Peeks, name: &str, contents: &Contents) -> Vec<PreviewRow> {
    let inner = rect.inner_width();
    let Some(focus) = &contents.focus else {
        return wrapped_lines(inner, "nothing but plugin panes — no screen to show", TAG);
    };
    let mut lines = vec![caption(inner, &focus.tab, &focus.title).into(), blank_line(rect).into()];
    lines.extend(screen_lines(rect, peeks, name, focus.pane, lines.len()));
    lines
}

fn caption(inner: usize, tab: &str, title: &str) -> Painted {
    let mut line = Line::new();
    let room = inner.saturating_sub(title.width() + 3).max(MIN_TAB);
    line.push(&truncate(tab, room), LABEL);
    line.push(" · ", TAG);
    line.push(&truncate(title, inner.saturating_sub(line.columns())), TAG);
    line.finish(inner)
}

const MIN_TAB: usize = 6;

fn screen_lines(
    rect: &Rect,
    peeks: &Peeks,
    session: &str,
    pane: u32,
    used: usize,
) -> Vec<PreviewRow> {
    let inner = rect.inner_width();
    let rows = rect.inner_height().saturating_sub(used);
    match peeks.get(session, pane) {
        None | Some(Peek::Reading) => vec![preview_line(inner, "reading…", TAG)],
        Some(Peek::Failed(reason)) => error_lines(inner, reason),
        Some(Peek::Ready(screen)) if screen.is_empty() => {
            vec![preview_line(inner, "nothing on this screen", TAG)]
        },
        Some(Peek::Ready(screen)) => {
            let shown = screen.len().min(rows);
            let mut lines: Vec<PreviewRow> =
                (shown..rows).map(|_| blank_line(rect).into()).collect();
            lines.extend(
                screen[screen.len() - shown..]
                    .iter()
                    .map(|line| PreviewRow::Pane(pane_row(rect, line))),
            );
            lines
        },
    }
}

fn plural(n: usize, thing: &str) -> String {
    match n {
        1 => format!("1 {thing}"),
        n => format!("{n} {thing}s"),
    }
}

fn dead_preview(rect: &Rect) -> Vec<PreviewRow> {
    let inner = rect.inner_width();
    let mut lines = vec![preview_line(inner, "not running", TAG), blank_line(rect).into()];
    lines.extend(wrapped_lines(
        inner,
        "there is a saved layout to bring it back from, and nothing running to look inside",
        TAG,
    ));
    lines
}

fn dir_preview(dirs: &DirSet, rect: &Rect) -> (String, String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    let Some(row) = dirs.selected_row() else {
        let (title, lines) = nothing_highlighted(rect);
        return (title, String::new(), lines);
    };
    let (path, _) = truncate_left(&row.path, inner);
    let mut lines = vec![preview_line(inner, &path, LABEL), blank_line(rect).into()];
    let mut right = String::new();
    match dirs.listing(&row.path) {
        None | Some(Listing::Reading) => lines.push(preview_line(inner, "reading…", TAG)),
        Some(Listing::Failed(reason)) => lines.extend(error_lines(inner, reason)),
        Some(Listing::Ready { entries, total }) => {
            right = plural(*total, "item");
            if entries.is_empty() {
                lines.push(preview_line(inner, "empty", TAG));
            }
            lines.extend(entries.iter().map(|entry| entry_line(rect, entry)));
        },
    }
    (row.name.as_str().to_owned(), right, lines)
}

fn entry_line(rect: &Rect, entry: &str) -> PreviewRow {
    PreviewRow::Pane(pane_row(rect, entry))
}

fn agent_preview(agents: &AgentSet, peeks: &Peeks, rect: &Rect) -> (String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    let Some(row) = agents.selected_row() else {
        return nothing_highlighted(rect);
    };
    let level = if row.status.is_waiting() { ACCENT } else { LABEL };
    let mut line = Line::new();
    line.push(&truncate(row.status.word(), inner), level);
    line.push(" · ", TAG);
    line.push(&row.age.label(), TAG);
    let mut lines = vec![line.finish(inner).into(), blank_line(rect).into()];
    lines.extend(screen_lines(rect, peeks, row.seat.session(), row.pane, lines.len()));
    (row.label(), lines)
}

const MIN_PATH: usize = 12;

fn search_body(state: &MatchSet, rect: &Rect, notes: usize) -> Vec<Painted> {
    let inner = rect.inner_width();
    let mut capacity = rect.inner_height().saturating_sub(notes);
    let mut body = Vec::new();

    if let Some(current) = state.current_session.as_deref() {
        if capacity == 0 {
            return body;
        }
        body.push(here_line(current, inner));
        capacity -= 1;
    }
    if capacity == 0 {
        return body;
    }
    if state.rows.is_empty() {
        body.push(note_line(rect, &Note::dim(empty_text(state))));
        return body;
    }

    let dead_at = dead_from(&state.rows);
    let line_of = |row: usize| row + usize::from(dead_at.is_some_and(|at| row >= at));
    let lines = state.rows.len() + usize::from(dead_at.is_some());
    let selected_line = state.rows.selected().map_or(0, line_of);
    let (start, end) = viewport(selected_line, lines, capacity);

    let visible = |line: usize| {
        dead_at.map_or(Some(line), |at| match line.cmp(&at) {
            std::cmp::Ordering::Less => Some(line),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(line - 1),
        })
    };
    let window: Vec<usize> = (start..end).filter_map(visible).collect();
    let age_width = window
        .iter()
        .map(|i| state.rows[*i].age.label().width())
        .max()
        .unwrap_or(0);
    let name_budget = inner.saturating_sub(CARET + GAP + age_width).max(4);

    body.extend((start..end).map(|line| match visible(line) {
        None => separator(inner, "🪦 Dead sessions"),
        Some(i) => {
            let selected = state.rows.selected() == Some(i);
            result_line(&state.rows[i], selected, name_budget, inner)
        },
    }));
    body
}

const HERE: &str = "🏠 Current";

fn here_line(current: &str, inner: usize) -> Painted {
    let mut line = Line::new();
    line.gap(CARET);
    line.push(&truncate(current, inner.saturating_sub(CARET)), NAME);
    let room = inner.saturating_sub(line.columns());
    if room >= HERE.width() + 3 {
        line.gap(1);
        line.push(&"─".repeat(room - HERE.width() - 2), TAG);
        line.gap(1);
        line.push(HERE, ACCENT);
    } else if room > 1 {
        line.gap(1);
        line.push(&"─".repeat(room - 1), TAG);
    }
    line.finish(inner)
}

fn dead_from(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|row| row.kind == Kind::Resurrectable)
}

fn separator(inner: usize, label: &str) -> Painted {
    let mut line = Line::new();
    line.gap(CARET);
    let label = truncate(label, inner.saturating_sub(CARET));
    let room = inner.saturating_sub(line.columns());
    if room >= label.width() + 2 {
        line.push(&"─".repeat(room - label.width() - 1), TAG);
        line.gap(1);
    }
    line.push(&label, LABEL);
    line.finish(inner)
}

fn result_line(row: &Row, selected: bool, name_budget: usize, inner: usize) -> Painted {
    let name = truncate(&row.name, name_budget);
    let visible = name.chars().count();
    let hits: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible).collect();

    let mut line = Line::new();
    gutter(&mut line, selected);
    line.push_hits(&name, NAME, ACCENT, &hits);
    let age = row.age.label();
    line.pad_to(inner.saturating_sub(age.width()));
    line.push(&age, LABEL);

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

fn prompt_text(state: &MatchSet) -> Prompt {
    let (action, is_error) = enter_action(state);
    (state.search_term.clone(), action, is_error)
}

fn enter_action(state: &MatchSet) -> (Option<String>, bool) {
    if let Some(row) = state.rows.selected_row() {
        return match row.kind {
            Kind::Live => (Some("Attach".to_owned()), false),
            Kind::Resurrectable => (Some("Resurrect".to_owned()), false),
        };
    }
    if state.is_own_name() {
        return (Some("already attached".to_owned()), false);
    }
    if let Some(reason) = state.name_error() {
        return (Some(reason.to_owned()), true);
    }
    if state.search_term.is_empty() {
        (Some("New session".to_owned()), false)
    } else {
        (Some(format!("Create \"{}\"", state.search_term)), false)
    }
}

fn note_texts(error: Option<&str>) -> Vec<Note> {
    match error {
        Some(error) => vec![Note::error(error)],
        None => Vec::new(),
    }
}

fn empty_text(state: &MatchSet) -> String {
    if !state.search_term.is_empty() {
        return format!("no match for \"{}\"", state.search_term);
    }
    if state.current_session.is_some() {
        "no other sessions".to_owned()
    } else {
        "no sessions".to_owned()
    }
}

fn dir_body(dirs: &DirSet, term: &str, rect: &Rect, notes: usize) -> Vec<Painted> {
    if dirs.rows.is_empty() {
        return vec![note_line(rect, &Note::dim(dir_empty_text(dirs, term)))];
    }
    let inner = rect.inner_width();
    let capacity = rect.inner_height().saturating_sub(notes);
    if capacity == 0 {
        return Vec::new();
    }
    let (start, end) = viewport(dirs.rows.selected().unwrap_or(0), dirs.rows.len(), capacity);
    let window = &dirs.rows[start..end];

    let name_column =
        window.iter().map(|r| r.name.width()).max().unwrap_or(0).min(inner / 3).max(4);
    let path_budget = inner.saturating_sub(CARET + name_column + GAP);

    window
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected = dirs.rows.selected() == Some(start + offset);
            dir_line(row, selected, name_column, path_budget, inner)
        })
        .collect()
}

fn dir_line(
    row: &DirRow,
    selected: bool,
    name_column: usize,
    path_budget: usize,
    inner: usize,
) -> Painted {
    let mut line = Line::new();
    gutter(&mut line, selected);
    line.push(&truncate(&row.name, name_column), NAME);
    line.pad_to(CARET + name_column);
    line.gap(GAP);

    let (path, dropped) = truncate_left(&row.path, path_budget);
    line.pad_to(inner.saturating_sub(path.width()));
    let shift = usize::from(dropped > 0);
    let hits: Vec<usize> =
        row.indices.iter().filter(|i| **i >= dropped).map(|i| i - dropped + shift).collect();
    line.push_hits(&path, LABEL, ACCENT, &hits);

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

fn dir_prompt(dirs: &DirSet, term: &str) -> Prompt {
    let Some(row) = dirs.selected_row() else {
        return (term.to_owned(), None, false);
    };
    (term.to_owned(), Some(format!("Create \"{}\"", row.name.as_str())), false)
}

fn dir_note_texts(dirs: &DirSet) -> Vec<Note> {
    match &dirs.status {
        Fetch::Failed(reason) => vec![Note::error(reason)],
        _ => Vec::new(),
    }
}

fn dir_empty_text(dirs: &DirSet, term: &str) -> String {
    match &dirs.status {
        Fetch::Waiting => "asking zoxide…".to_owned(),
        Fetch::Failed(_) => "no directories".to_owned(),
        Fetch::Ready(_) if term.is_empty() => "zoxide knows nowhere yet".to_owned(),
        Fetch::Ready(_) => format!("no match for \"{term}\""),
    }
}

#[derive(PartialEq, Eq)]
enum AgentFit {
    Full,
    AbbrTag,
    NoCwd,
    NoAge,
}

fn agent_body(
    agents: &AgentSet,
    term: &str,
    rect: &Rect,
    notes: usize,
    frame: u64,
) -> Vec<Painted> {
    let inner = rect.inner_width();
    let mut capacity = rect.inner_height().saturating_sub(notes);
    let mut body = Vec::new();

    if let Some(current) = &agents.current {
        if capacity == 0 {
            return body;
        }
        body.push(here_line(&current.label(), inner));
        capacity -= 1;
    }
    if capacity == 0 {
        return body;
    }
    if agents.rows.is_empty() {
        body.push(note_line(rect, &Note::dim(agent_empty_text(agents, term))));
        return body;
    }
    let (start, end) = viewport(agents.rows.selected().unwrap_or(0), agents.rows.len(), capacity);
    let window = &agents.rows[start..end];

    let full_tag =
        window.iter().map(|r| agents::full_tag(&r.status, frame).width()).max().unwrap_or(0);
    let abbr_width =
        window.iter().map(|r| agents::abbr_tag(&r.status, frame).width()).max().unwrap_or(0);
    let age_width =
        window.iter().map(|r| r.age.label().width()).max().unwrap_or(0);
    let name_column =
        window.iter().map(|r| r.label().width()).max().unwrap_or(0).min(inner / 3).max(4);

    let fixed = CARET + name_column + GAP;
    let fit = if fixed + full_tag + GAP + age_width + GAP + MIN_PATH <= inner {
        AgentFit::Full
    } else if fixed + abbr_width + GAP + age_width + GAP + MIN_PATH <= inner {
        AgentFit::AbbrTag
    } else if fixed + abbr_width + GAP + age_width <= inner {
        AgentFit::NoCwd
    } else {
        AgentFit::NoAge
    };

    let abbr = !matches!(fit, AgentFit::Full);
    let tag_column = if abbr { abbr_width } else { full_tag };
    let cwd_budget = match fit {
        AgentFit::Full | AgentFit::AbbrTag => {
            Some(inner.saturating_sub(fixed + tag_column + GAP + age_width + GAP))
        },
        _ => None,
    };

    body.extend(window.iter().enumerate().map(|(offset, row)| {
        let selected = agents.rows.selected() == Some(start + offset);
        agent_line(row, selected, &fit, name_column, tag_column, cwd_budget, inner, frame)
    }));
    body
}

fn agent_line(
    row: &AgentRow,
    selected: bool,
    fit: &AgentFit,
    name_column: usize,
    tag_column: usize,
    cwd_budget: Option<usize>,
    inner: usize,
    frame: u64,
) -> Painted {
    let label = truncate(&row.label(), name_column);
    let visible = label.chars().count();
    let hits: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible).collect();

    let mut line = Line::new();
    gutter(&mut line, selected);
    line.push_hits(&label, NAME, ACCENT, &hits);
    line.pad_to(CARET + name_column);
    line.gap(GAP);
    let tag_level = if row.status.is_waiting() { ACCENT } else { TAG };
    let tag = if matches!(fit, AgentFit::Full) {
        agents::full_tag(&row.status, frame)
    } else {
        agents::abbr_tag(&row.status, frame)
    };
    line.push(&tag, tag_level);

    let age = row.age.label();
    match cwd_budget {
        Some(cwd_budget) => {
            line.pad_to(CARET + name_column + GAP + tag_column);
            line.gap(GAP);
            line.push(&age, LABEL);
            let cwd = row.cwd.as_deref().map(short_cwd).unwrap_or_default();
            let (cwd, _) = truncate_left(&cwd, cwd_budget);
            line.pad_to(inner.saturating_sub(cwd.width()));
            line.push(&cwd, LABEL);
        },
        None if !matches!(fit, AgentFit::NoAge) => {
            line.pad_to(inner.saturating_sub(age.width()));
            line.push(&age, LABEL);
        },
        None => {},
    }

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

fn agent_prompt(agents: &AgentSet, term: &str) -> Prompt {
    match agents.selected_row() {
        Some(row) => (term.to_owned(), Some(format!("Go to \"{}\"", row.label())), false),
        None => (term.to_owned(), None, false),
    }
}

fn agent_note_texts(agents: &AgentSet, width: usize) -> Vec<Note> {
    let mut notes = match &agents.status {
        Fetch::Failed(reason) => vec![Note::error(truncate(reason, width))],
        _ => Vec::new(),
    };
    if agents.unplaced > 0 {
        notes.push(Note::dim(truncate(&unplaced_text(agents.unplaced), width)));
    }
    notes
}

fn unplaced_text(unplaced: usize) -> String {
    match unplaced {
        1 => "1 agent sits in a pane luneta cannot find".to_owned(),
        n => format!("{n} agents sit in panes luneta cannot find"),
    }
}

fn agent_empty_text(agents: &AgentSet, term: &str) -> String {
    match &agents.status {
        Fetch::Waiting => "looking for agents…".to_owned(),
        Fetch::Failed(_) => "no agents".to_owned(),
        Fetch::Ready(_) if term.is_empty() && agents.current.is_some() => {
            "no other agents".to_owned()
        },
        Fetch::Ready(_) if term.is_empty() => "no agents running".to_owned(),
        Fetch::Ready(_) => format!("no match for \"{term}\""),
    }
}

fn search_help(width: usize) -> Painted {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Select", "Select"),
            ("<TAB>", "Agents", "Agents"),
            ("<Ctrl r>", "Rename", "Rename"),
            ("<Del>", "Kill or delete", "Delete"),
            ("<ESC>", "Close", "Close"),
        ],
    )
}

fn dirs_help(width: usize) -> Painted {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Go there", "Go"),
            ("<TAB>", "Sessions", "Sessions"),
            ("<ESC>", "Close", "Close"),
        ],
    )
}

fn agents_help(width: usize) -> Painted {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Go to agent", "Go"),
            ("<TAB>", "Directories", "Dirs"),
            ("<Del>", "Kill its pane", "Kill"),
            ("<ESC>", "Close", "Close"),
        ],
    )
}

type Key<'a> = (&'a str, &'a str, &'a str);

fn keys_text(width: usize, keys: &[Key]) -> Painted {
    let spelled = |sep: &str, joiner: &str, short: bool| {
        keys.iter()
            .map(|(k, long, brief)| format!("{}{}{}", k, sep, if short { brief } else { long }))
            .collect::<Vec<_>>()
            .join(joiner)
    };
    let bare = |joiner: &str| keys.iter().map(|(k, _, _)| *k).collect::<Vec<_>>().join(joiner);
    let line = [
        spelled(" - ", ", ", false),
        spelled(" ", "  ", false),
        spelled(" ", "  ", true),
        spelled(" ", " ", true),
        bare("/"),
        bare(""),
    ]
    .into_iter()
    .find(|candidate| candidate.width() <= width)
    .unwrap_or_else(|| bare("/"));

    let mut text = Painted::new(&line).dim_all();
    for (key, _, _) in keys {
        text = text.color_substring(ACCENT, key);
    }
    text
}

/// One line saying what is wrong, for a pane that has nothing else to show.
pub fn render_notice(text: &str, rows: usize, cols: usize) {
    if rows > 0 {
        print_at(&note_within(cols.saturating_sub(2), &Note::error(text)), 0, 0, cols);
    }
}

/// What follows a group's name in the menu: it opens, as a folded session
/// does in the sidebar.
const GROUP: &str = "▸";

/// The rows of the menu that are not its entries: the rule on top, a blank
/// line and the keys under them, and the bottom.
const MENU_CHROME: usize = 4;

/// What the rule on top takes besides its title and its right: the corners,
/// and the dashes and spaces around the two.
const MENU_RULE: usize = 9;

/// Room for the keys under the entries.
const MIN_MENU: usize = 28;

/// Room for the keys under what a delete key asks, spelled out.
const MIN_ASKING: usize = 35;

/// How wide the menu draws what is wrong with it.
const MENU_NOTICE: usize = 48;

/// What a menu draws: the title its rule names, whether the rule says on its
/// right where an agent starts, the lines under the rule, and the keys under
/// them.
struct MenuParts {
    title: String,
    right: bool,
    body: Vec<Painted>,
    help: Painted,
}

/// The parts of a menu `inner` wide: the keys of the group open and what each
/// does, what a delete key asks before it goes ahead, where you are going on
/// your way out of the session, or what is wrong.
fn menu_parts(shown: Shown, inner: usize) -> MenuParts {
    let lines = |text: &str, line: fn(&str, usize) -> Painted| -> Vec<Painted> {
        wrap(text, inner).iter().map(|text| line(text, inner)).collect()
    };
    match shown {
        Shown::Keys(keys) => MenuParts {
            title: menu_title(keys),
            right: true,
            body: keys.entries().iter().map(|entry| menu_line(entry, inner)).collect(),
            help: menu_help(inner, !keys.names().is_empty()),
        },
        Shown::Asking(keys, question) => MenuParts {
            title: menu_title(keys),
            right: true,
            body: lines(question, menu_note),
            help: keys_text(
                inner,
                &[("<y>", "Yes", "Yes"), ("<BS>", "Back", "Back"), ("<ESC>", "Close", "Close")],
            ),
        },
        Shown::Leaving(to) => MenuParts {
            title: TITLE.to_owned(),
            right: false,
            body: lines(&format!("Going to {to}; this session ends once you are there"), menu_note),
            help: keys_text(inner, &[]),
        },
        Shown::Wrong(reason) => MenuParts {
            title: TITLE.to_owned(),
            right: false,
            body: lines(reason, menu_error),
            help: menu_help(inner, false),
        },
    }
}

/// The menu: the keys of the group open and what each does, under a rule that
/// names the groups opened and says where an agent starts, and the keys that
/// go back and close under them. Nothing once it asked zellij to close it.
pub fn render_menu(menu: &Menu, rows: usize, cols: usize) {
    let Some(shown) = menu.shown() else {
        return;
    };
    let rect = Rect { x: 0, y: 0, width: cols, height: rows };
    let Some(room) = rect.inner_height().checked_sub(1) else {
        return;
    };
    let inner = rect.inner_width();
    let MenuParts { title, right, body, help } = menu_parts(shown, inner);
    let right = if right { menu_where(menu.cwd()) } else { String::new() };
    let mut lines: Vec<Painted> = body;
    lines.truncate(room.saturating_sub(1));
    lines.resize_with(room + 1, || blank_line(&rect));
    draw(&rect, &title, &right, lines);
    print_at(&help, PAD, rect.inner_y() + room, inner);
}

/// How big the menu draws: tall enough for what it shows, and wide enough for
/// the entries of the group open and its rule, whatever a delete key asks. No
/// size once it asked zellij to close it.
pub fn menu_size(menu: &Menu) -> Option<(usize, usize)> {
    let shown = menu.shown()?;
    let width = match shown {
        Shown::Keys(keys) | Shown::Asking(keys, _) => {
            let widest = keys.entries().iter().map(menu_entry_width).max().unwrap_or(0);
            let rule = menu_title(keys).width() + menu_where(menu.cwd()).width() + MENU_RULE;
            let least = if matches!(shown, Shown::Asking(..)) { MIN_ASKING } else { MIN_MENU };
            (widest + PAD * 2).max(rule).max(least)
        },
        Shown::Leaving(_) | Shown::Wrong(_) => MENU_NOTICE,
    };
    let body = menu_parts(shown, width - PAD * 2).body.len();
    Some((width, body + MENU_CHROME))
}

fn menu_title(keys: &Keys) -> String {
    let names = keys.names();
    if names.is_empty() { TITLE.to_owned() } else { names.join(" › ") }
}

/// Where an agent the menu starts works: the directory of the pane it is for,
/// once the menu knows.
fn menu_where(cwd: Option<&str>) -> String {
    cwd.map(short_cwd).unwrap_or_default()
}

fn menu_entry_width(entry: &Entry) -> usize {
    let does = match entry {
        Entry::Group { name, .. } => name.width() + 1 + GROUP.width(),
        Entry::Start { prompt, .. } => prompt.width(),
        Entry::Close { scope, .. } => scope.does().width(),
        Entry::Command(command) => command.does().width(),
    };
    entry.key().to_string().width() + GAP + does
}

fn menu_line(entry: &Entry, inner: usize) -> Painted {
    let mut line = Line::new();
    line.push(&entry.key().to_string(), ACCENT);
    line.gap(GAP);
    let room = inner.saturating_sub(line.columns());
    match entry {
        Entry::Group { name, .. } => {
            line.push(&truncate(name, room.saturating_sub(1 + GROUP.width())), LABEL);
            line.gap(1);
            line.push(GROUP, TAG);
        },
        Entry::Start { prompt, .. } => line.push(&truncate(prompt, room), NAME),
        Entry::Close { scope, .. } => line.push(&truncate(scope.does(), room), NAME),
        Entry::Command(command) => line.push(&truncate(command.does(), room), NAME),
    }
    line.finish(inner)
}

fn menu_note(text: &str, inner: usize) -> Painted {
    let mut line = Line::new();
    line.push(text, NAME);
    line.finish(inner)
}

fn menu_error(text: &str, inner: usize) -> Painted {
    let mut line = Line::new();
    line.push_error(text);
    line.finish(inner)
}

fn menu_help(width: usize, opened: bool) -> Painted {
    let close = ("<ESC>", "Close", "Close");
    if opened {
        keys_text(width, &[("<BS>", "Back", "Back"), close])
    } else {
        keys_text(width, &[close])
    }
}

/// How many lines the menu takes at the bottom of a sidebar `cols` wide, with
/// the room it wants: its rule, what it shows, and the keys under them. No
/// line once it shows nothing.
fn sidebar_menu_height(menu: &SidebarMenu, cols: usize) -> usize {
    let Some(shown) = menu.shown() else {
        return 0;
    };
    menu_parts(shown, cols.saturating_sub(2)).body.len() + 2
}

/// The menu at the bottom of the sidebar, in `lines` lines: the lines from
/// its top, each as wide as the sidebar, and the keys under them. A rule
/// names the groups opened and says where an agent starts, as the border of
/// the floating menu does, over what the menu shows. Short of room, what it
/// shows goes first, then the rule.
fn sidebar_menu_lines(
    menu: &SidebarMenu,
    lines: usize,
    cols: usize,
) -> (Vec<Painted>, Option<Painted>) {
    let Some(shown) = menu.shown() else {
        return (Vec::new(), None);
    };
    let MenuParts { title, right, body, help } = menu_parts(shown, cols.saturating_sub(2));
    let Some(room) = lines.checked_sub(1) else {
        return (Vec::new(), None);
    };
    let mut drawn: Vec<Painted> = Vec::with_capacity(room);
    if room > 0 {
        let right = if right { menu_where(menu.cwd()) } else { String::new() };
        drawn.push(menu_rule(&title, &right, cols));
    }
    drawn.extend(body.into_iter().take(room.saturating_sub(1)));
    (drawn, Some(help))
}

/// The rule over the menu in the sidebar: the groups opened, and where an
/// agent starts while the rule has room for all of both.
fn menu_rule(title: &str, right: &str, cols: usize) -> Painted {
    let mut line = Line::new();
    line.push(RULE, Ink::Dim);
    line.gap(1);
    // A space, the right, a space and a last stretch of the rule.
    let tail = right.width() + 2 + RULE.width();
    let fits =
        !right.is_empty() && line.columns() + title.width() + 1 + RULE.width() + tail <= cols;
    let tail = if fits { tail } else { 0 };
    let room = cols.saturating_sub(line.columns() + 1 + RULE.width() + tail);
    line.push(&truncate(title, room), Ink::Bold(LABEL));
    line.gap(1);
    line.push(&RULE.repeat(cols.saturating_sub(line.columns() + tail)), Ink::Dim);
    if fits {
        line.gap(1);
        line.push(right, TAG);
        line.gap(1);
        line.push(RULE, Ink::Dim);
    }
    line.fill(cols)
}

/// How a sidebar `rows` tall is cut up: the header and a blank line under it
/// at the top, the tree under them, the notes under it, the search box while
/// it is open, and a line of keys at the bottom while the sidebar has the
/// focus. The box opens at the bottom so that no row moves. The header goes
/// before it would leave the tree no row.
///
/// While the menu is open, it takes the place of the box and the keys, with
/// as many of the `menu` lines it wants as leave the tree a row, and the notes
/// leave the tree that row too: the menu is for the row the cursor is on.
struct Shelf {
    header: usize,
    tree: usize,
    notes: usize,
    search_y: Option<usize>,
    help_y: Option<usize>,
    menu_y: Option<usize>,
}

impl Shelf {
    fn new(rows: usize, notes: usize, focused: bool, searching: bool, menu: usize) -> Self {
        let menu = menu.min(rows.saturating_sub(1));
        let search = usize::from(menu == 0 && searching && rows > 0);
        let help = usize::from(menu == 0 && focused && rows > search + 1);
        let below = menu + search + help;
        let header = if rows > below + notes + HEADER { HEADER } else { 0 };
        let kept = usize::from(menu > 0);
        let notes = notes.min((rows - below - header).saturating_sub(kept));
        Shelf {
            header,
            tree: rows - below - header - notes,
            notes,
            search_y: (search == 1).then(|| rows - help - 1),
            help_y: (help == 1).then(|| rows - 1),
            menu_y: (menu > 0).then(|| rows - menu),
        }
    }
}

/// The header's rule, and the blank line under it.
pub const HEADER: usize = 2;

pub fn render_sidebar(
    tree: &Tree,
    agents: &AgentSet,
    focused: bool,
    rows: usize,
    cols: usize,
    frame: u64,
) {
    let inner = cols.saturating_sub(2);
    let notes = agent_note_texts(agents, inner);
    let term = tree.search();
    let menu = tree.menu.as_ref().map_or(0, |menu| sidebar_menu_height(menu, cols));
    let shelf = Shelf::new(rows, notes.len(), focused, term.is_some(), menu);
    for (y, line) in sidebar_lines(tree, &notes, &shelf, focused, cols, frame).iter().enumerate() {
        print_at(line, 0, y, cols);
    }
    if let Some((y, term)) = shelf.search_y.zip(term) {
        print_at(&search_line(term, inner), 0, y, cols);
    }
    if let Some(y) = shelf.help_y {
        let help = if term.is_some() { search_help_line(inner) } else { sidebar_help(inner) };
        print_at(&help, 1, y, inner);
    }
    if let Some((y, menu)) = shelf.menu_y.zip(tree.menu.as_ref()) {
        let (lines, help) = sidebar_menu_lines(menu, rows - y, cols);
        for (at, line) in lines.iter().enumerate() {
            print_at(line, 0, y + at, cols);
        }
        if let Some(help) = help {
            print_at(&help, 1, rows - 1, inner);
        }
    }
}

/// The row of the tree drawn on `line` of the sidebar, if one is.
pub fn sidebar_row_at(
    tree: &Tree,
    agents: &AgentSet,
    focused: bool,
    rows: usize,
    cols: usize,
    line: usize,
) -> Option<usize> {
    let shelf = sidebar_shelf(tree, agents, focused, rows, cols);
    let mut line = line.checked_sub(shelf.header).filter(|line| *line < shelf.tree)?;
    let (shown, _) = on_screen(tree, focused, shelf.tree);
    for index in shown {
        let span = span(&tree.rows[index]);
        if line < span {
            return Some(index);
        }
        line -= span;
    }
    None
}

/// Moves the view of the sidebar `delta` lines down, or up, as far as there
/// are rows to show, and says whether it moved.
pub fn scroll_sidebar(
    tree: &mut Tree,
    agents: &AgentSet,
    focused: bool,
    rows: usize,
    cols: usize,
    delta: isize,
) -> bool {
    let height = sidebar_shelf(tree, agents, focused, rows, cols).tree;
    let (Range { start, .. }, _) = on_screen(tree, focused, height);
    let last = top_ending_at(&tree.rows, tree.rows.len().saturating_sub(1), height);
    let top = top_after(&tree.rows, start, delta).min(last);
    if top == start {
        return false;
    }
    tree.scroll_to(top);
    true
}

/// The top row of a view moved `delta` lines from row `top`: a row the move
/// takes any of goes out of the view whole, or comes into it whole.
fn top_after(rows: &[Node], mut top: usize, delta: isize) -> usize {
    let mut lines = delta.unsigned_abs();
    while lines > 0 {
        if delta > 0 && top + 1 < rows.len() {
            lines = lines.saturating_sub(span(&rows[top]));
            top += 1;
        } else if delta < 0 && top > 0 {
            top -= 1;
            lines = lines.saturating_sub(span(&rows[top]));
        } else {
            break;
        }
    }
    top
}

/// Puts the cursor on `row`, as a click does, and says whether it can be
/// there. A view the wheel moved stays where it is while the row is in it, so
/// that the row stays under the pointer.
pub fn select_sidebar_row(
    tree: &mut Tree,
    agents: &AgentSet,
    focused: bool,
    rows: usize,
    cols: usize,
    row: usize,
) -> bool {
    let height = sidebar_shelf(tree, agents, focused, rows, cols).tree;
    let (shown, _) = on_screen(tree, focused, height);
    let wheeled = tree.top().is_some();
    let selected = tree.rows.select(row);
    if wheeled && shown.contains(&row) {
        tree.scroll_to(shown.start);
    }
    selected
}

/// How a sidebar `rows` tall cuts itself up, as `render_sidebar` draws it.
fn sidebar_shelf(tree: &Tree, agents: &AgentSet, focused: bool, rows: usize, cols: usize) -> Shelf {
    let notes = agent_note_texts(agents, cols.saturating_sub(2)).len();
    let menu = tree.menu.as_ref().map_or(0, |menu| sidebar_menu_height(menu, cols));
    Shelf::new(rows, notes, focused, tree.search().is_some(), menu)
}

/// Whether a click on `column` of a row folds it: a session's, on its fold.
pub fn folds_at(node: &Node, column: usize) -> bool {
    matches!(node, Node::Session(_)) && column < FOLD
}

/// The rows of the tree a sidebar shows in `height` lines, and the one of them
/// that is lit: the cursor while the sidebar has the focus, and the pane you
/// are in while it does not. The rows shown are the ones around the lit row,
/// or around the cursor while none is, so a lit row is never out of sight
/// until the wheel moves the view off it. The last row shown can be cut short
/// by the bottom of the view.
fn on_screen(tree: &Tree, focused: bool, height: usize) -> (Range<usize>, Option<usize>) {
    let lit = if focused {
        tree.rows.selected()
    } else {
        tree.rows.iter().position(|node| matches!(node, Node::Pane(pane) if pane.here))
    };
    let last = tree.rows.len().saturating_sub(1);
    let start = match tree.top() {
        Some(top) => top.min(top_ending_at(&tree.rows, last, height)),
        None => top_ending_at(&tree.rows, lit.or(tree.rows.selected()).unwrap_or(0), height),
    };
    let mut end = start;
    let mut used = 0;
    while end < tree.rows.len() && used < height {
        used += span(&tree.rows[end]);
        end += 1;
    }
    (start..end, lit)
}

/// The top row of a view `height` lines tall that ends with all of row
/// `last`: as many of the rows before it as fit with it. A row taller than
/// the view is the top of its own.
fn top_ending_at(rows: &[Node], last: usize, height: usize) -> usize {
    let mut top = last;
    let mut used = rows.get(last).map_or(0, span);
    while top > 0 && used + span(&rows[top - 1]) <= height {
        top -= 1;
        used += span(&rows[top]);
    }
    top
}

/// How many lines a row takes: a pane's takes one more for its branch, where
/// it has one.
fn span(node: &Node) -> usize {
    match node {
        Node::Pane(PaneNode { checkout: Some(_), .. }) => 2,
        Node::Pane(_) | Node::Session(_) => 1,
    }
}

const UNFOLDED: &str = "▾";

const FOLDED: &str = "▸";

/// The columns of a session's row a click folds it by: the `▾` or `▸`, and
/// the column either side of it.
const FOLD: usize = 3;

/// What marks the pane you are in, in its first column, whether the sidebar
/// has the focus or not.
const HERE_MARK: &str = "▌";

const RULE: &str = "─";

/// The column every row leaves empty on its right.
const MARGIN: usize = 1;

const GLYPH: usize = 1;

/// How wide the dots and the ages are across the rows on screen, so that
/// every label is cut at the same column.
struct Columns {
    glyph: usize,
    age: usize,
}

fn sidebar_lines(
    tree: &Tree,
    notes: &[Note],
    shelf: &Shelf,
    focused: bool,
    cols: usize,
    frame: u64,
) -> Vec<Painted> {
    let inner = cols.saturating_sub(2);
    let mut lines = Vec::with_capacity(shelf.header + shelf.tree + shelf.notes);
    if shelf.header > 0 {
        lines.extend(header_lines(&tree.summary, cols, frame));
    }
    if tree.rows.is_empty() {
        if shelf.tree > 0 {
            let empty = if tree.narrowed() { "no match" } else { "no sessions" };
            lines.push(note_within(inner, &Note::dim(empty)));
        }
    } else {
        let (Range { start, end }, lit) = on_screen(tree, focused, shelf.tree);
        let window = &tree.rows[start..end];
        let marks = || {
            window.iter().filter_map(|node| match node {
                Node::Pane(PaneNode { agent: Some(mark), .. }) => Some(mark),
                _ => None,
            })
        };
        let columns = Columns {
            glyph: marks()
                .map(|mark| agents::dot(&mark.status, frame).width())
                .max()
                .unwrap_or(0)
                .max(GLYPH),
            age: marks().map(|mark| mark.age.label().width()).max().unwrap_or(0),
        };
        for (offset, node) in window.iter().enumerate() {
            let lit = lit == Some(start + offset);
            let light = |line: Painted| if lit { line.selected() } else { line };
            match node {
                Node::Session(node) => lines.push(light(branch_line(node, cols, frame))),
                Node::Pane(node) => {
                    lines.push(light(leaf_line(node, &columns, cols, frame)));
                    if let Some(checkout) = &node.checkout {
                        lines.push(light(checkout_line(node, checkout, &columns, cols)));
                    }
                },
            }
        }
    }
    // The last row on screen can run past the bottom of the tree.
    lines.resize_with(shelf.header + shelf.tree, || Painted::new(" ".repeat(cols)));
    lines.extend(notes.iter().take(shelf.notes).map(|note| note_within(inner, note)));
    lines
}

/// The lines the header takes, as many as `HEADER` says: its rule, and a
/// blank line under it.
fn header_lines(summary: &Summary, cols: usize, frame: u64) -> [Painted; HEADER] {
    [header_line(summary, cols, frame), Painted::new(" ".repeat(cols))]
}

/// The rule along the top of the sidebar: how many sessions are live, and how
/// many agents across them wait for you, are done, and are at work. A count
/// of none is left out, and so is the word for the sessions where the counts
/// need its room.
fn header_line(summary: &Summary, cols: usize, frame: u64) -> Painted {
    let counts: Vec<(Status, usize)> =
        summary.counts().into_iter().filter(|(_, n)| *n > 0).collect();
    let counted: usize = counts
        .iter()
        .map(|(status, n)| agents::dot(status, frame).width() + n.to_string().len())
        .sum::<usize>()
        + counts.len().saturating_sub(1);
    // A space, the counts, a space and a last stretch of the rule.
    let tail = if counted > 0 { counted + 2 + RULE.width() } else { 0 };
    let word = (summary.sessions > 0)
        .then(|| plural(summary.sessions, "session"))
        .filter(|word| RULE.width() + word.width() + 2 + RULE.width() + tail <= cols);

    let mut line = Line::new();
    if let Some(word) = &word {
        line.push(RULE, Ink::Dim);
        line.gap(1);
        line.push(word, Ink::Dim);
        line.gap(1);
    }
    if counted > 0 && line.columns() + RULE.width() + tail <= cols {
        line.push(&RULE.repeat(cols - line.columns() - tail), Ink::Dim);
        line.gap(1);
        for (nth, (status, n)) in counts.iter().enumerate() {
            if nth > 0 {
                line.gap(1);
            }
            line.push(&agents::dot(status, frame), dot_ink(status));
            let ink = if status.is_waiting() { Ink::Bold(ACCENT) } else { Ink::Plain };
            line.push(&n.to_string(), ink);
        }
        line.gap(1);
        line.push(RULE, Ink::Dim);
    } else {
        line.push(&RULE.repeat(cols.saturating_sub(line.columns())), Ink::Dim);
    }
    line.fill(cols)
}

/// A session's row: its name, and a rule out to what it says about its
/// agents. The session you are in has its name in a colour and its rule in
/// the accent.
fn branch_line(node: &SessionNode, cols: usize, frame: u64) -> Painted {
    let badge = branch_badge(node, frame);
    let reserve = badge.as_ref().map_or(0, |(badge, _)| badge.width() + 1);
    let mut line = Line::new();
    line.gap(1);
    line.push(if node.folded { FOLDED } else { UNFOLDED }, Ink::Dim);
    line.gap(1);
    let room = cols.saturating_sub(line.columns() + reserve + MARGIN);
    let name = if node.current { Ink::Bold(NAME) } else { Ink::Strong };
    let (text, hits) = lit(&node.name, room, &node.hits);
    line.push_hits(&text, name, Ink::Bold(ACCENT), &hits);
    let rule = cols.saturating_sub(line.columns() + 1 + reserve + MARGIN);
    if rule > 0 {
        line.gap(1);
        let ink = if node.current { Ink::Level(ACCENT) } else { Ink::Dim };
        line.push(&RULE.repeat(rule), ink);
    }
    if let Some((badge, ink)) = badge {
        line.pad_to(cols.saturating_sub(badge.width() + MARGIN));
        line.push(&badge, ink);
    }
    line.fill(cols)
}

/// What a session says on its own row: how many of its agents wait for you,
/// and once it is folded, the dot of the one that wants you most.
fn branch_badge(node: &SessionNode, frame: u64) -> Option<(String, Ink)> {
    if node.waiting > 0 {
        let dot = agents::dot(&Status::Waiting, frame);
        return Some((format!("{dot}{}", node.waiting), Ink::Bold(ACCENT)));
    }
    match &node.loudest {
        Some(status) if node.folded => Some((agents::dot(status, frame), dot_ink(status))),
        _ => None,
    }
}

/// The colour of a status: the accent for an agent that waits for you, the
/// success colour for one that is done, the tag for one at work. A shell and
/// an agent that says nothing are dimmed.
fn dot_ink(status: &Status) -> Ink {
    match status {
        Status::Waiting => Ink::Bold(ACCENT),
        Status::Idle => Ink::Bold(Colour::Success),
        Status::Busy | Status::Unknown(_) => Ink::Level(TAG),
        Status::Shell | Status::Unreported => Ink::Dim,
    }
}

/// A pane's row: its dot, its label, and how long its agent has been in its
/// state. The label is plain, in the accent while its agent waits for you,
/// and dimmed with no agent. The pane you are in has the mark and a bold one.
fn leaf_line(node: &PaneNode, columns: &Columns, cols: usize, frame: u64) -> Painted {
    let mut line = pane_margin(node);
    let start = line.columns();
    if let Some(mark) = &node.agent {
        line.push(&agents::dot(&mark.status, frame), dot_ink(&mark.status));
    }
    line.pad_to(start + columns.glyph);
    line.gap(1);
    // A label in the accent shows the letters a search lands on in a colour.
    let (label, hit) = match &node.agent {
        _ if node.here => (Ink::Strong, Ink::Bold(ACCENT)),
        Some(mark) if mark.status.is_waiting() => (Ink::Bold(ACCENT), Ink::Bold(NAME)),
        Some(_) => (Ink::Plain, Ink::Bold(ACCENT)),
        None => (Ink::Dim, Ink::Bold(ACCENT)),
    };
    let reserve = if columns.age > 0 { columns.age + 1 } else { 0 };
    let room = cols.saturating_sub(line.columns() + reserve + MARGIN);
    let (text, hits) = lit(&node.label, room, &node.hits);
    line.push_hits(&text, label, hit, &hits);
    if let Some(mark) = &node.agent {
        let age = mark.age.label();
        line.pad_to(cols.saturating_sub(age.width() + MARGIN));
        line.push(&age, Ink::Dim);
    }
    line.fill(cols)
}

/// The line under a pane's row: the branch checked out where the pane works,
/// in gray, under its label, and how its work tree stands, under the age. The
/// branch is cut to leave the status room. The mark of the pane you are in
/// runs down this line too.
fn checkout_line(node: &PaneNode, checkout: &Checkout, columns: &Columns, cols: usize) -> Painted {
    let mut line = pane_margin(node);
    line.gap(columns.glyph + 1);
    let status = checkout.status.as_str();
    let reserve = if status.is_empty() { 0 } else { status.width() + 1 };
    let room = cols.saturating_sub(line.columns() + reserve + MARGIN);
    line.push(&truncate(&checkout.branch, room), Ink::Gray);
    if !status.is_empty() {
        line.pad_to(cols.saturating_sub(status.width() + MARGIN));
        line.push(status, Ink::Plain);
    }
    line.fill(cols)
}

/// The columns of a pane's lines before its dot: the mark of the pane you are
/// in, or a blank where it goes, and the indent under its session.
fn pane_margin(node: &PaneNode) -> Line {
    let mut line = Line::new();
    if node.here {
        line.push(HERE_MARK, Ink::Bold(ACCENT));
    } else {
        line.gap(1);
    }
    line.gap(2);
    line
}

/// `text` cut to `room`, and the characters a search lit that are left in it.
/// The `…` that ends a cut stands for what it took, and is not one of them.
fn lit(text: &str, room: usize, hits: &[usize]) -> (String, Vec<usize>) {
    let cut = truncate(text, room);
    let count = cut.chars().count();
    let kept = if cut == text { count } else { count.saturating_sub(1) };
    let hits = hits.iter().copied().filter(|hit| *hit < kept).collect();
    (cut, hits)
}

/// The search box: what is typed, its tail when it is too long to fit.
fn search_line(term: &str, inner: usize) -> Painted {
    let mut line = Line::new();
    line.push("/", TAG);
    let (typed, _) = truncate_left(&format!("{term}_"), inner.saturating_sub(1));
    line.push(&typed, ACCENT);
    line.finish(inner)
}

/// The keys that move, and `?` for the rest: the sidebar's commands and the
/// menu's keys.
fn sidebar_help(width: usize) -> Painted {
    keys_text(
        width,
        &[
            ("<↓↑>", "Move", "Move"),
            ("<ENTER>", "Go", "Go"),
            ("<←→>", "Fold", "Fold"),
            ("<ESC>", "Back", "Back"),
            ("<?>", "Help", "Help"),
        ],
    )
}

fn search_help_line(width: usize) -> Painted {
    keys_text(
        width,
        &[("<↓↑>", "Move", "Move"), ("<ENTER>", "Go", "Go"), ("<ESC>", "Clear", "Clear")],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Cursor;
    use crate::places::Places;
    use crate::elapsed::Age;
    use crate::sessions::{Focus, Selection, Session, Sessions};
    use crate::elapsed::Held;
    use crate::tree::Mark;

    fn picture<R: Into<PreviewRow>>(
        rect: &Rect,
        title: &str,
        right: &str,
        interior: Vec<R>,
    ) -> Vec<String> {
        let mut lines = vec![rect.top(title, right).line];
        lines.extend(interior.into_iter().map(|line| {
            format!("{}{}{}", VERTICAL, line.into().content(), VERTICAL)
        }));
        lines.push(rect.bottom());
        lines
    }

    fn session(name: &str, kind: Kind, age: u64) -> Row {
        Row::new(name.to_owned(), kind, Age::from_secs(age), 0, vec![], false)
    }

    fn matches(rows: Vec<Row>, selected: Option<usize>) -> MatchSet {
        let mut state = MatchSet::default();
        state.rows = Cursor::seeded(rows, selected.unwrap_or(0));
        state
    }

    fn attached(rows: Vec<Row>, selected: Option<usize>, current: &str) -> MatchSet {
        let mut state = matches(rows, selected);
        state.current_session = Some(current.to_owned());
        state
    }

    #[test]
    fn the_current_session_is_pinned_above_the_list() {
        let rect = Rect { x: 0, y: 0, width: 42, height: 8 };
        let state = attached(
            vec![
                session("luneta", Kind::Live, 2 * HOUR),
                session("dotfiles", Kind::Live, 5 * HOUR),
                session("api-spike", Kind::Resurrectable, 40 * DAY),
            ],
            Some(1),
            "notes",
        );
        let body = search_body(&state, &rect, 0);
        let right = count(state.rows.selected(), state.rows.len());
        assert_eq!(
            picture(&rect, TITLE, &right, interior(&rect, &[], body)),
            vec![
                "╭─ luneta ───────────────────────── 2/3 ─╮",
                "│                                        │",
                "│   notes ─────────────────── 🏠 Current │",
                "│   luneta                        2h ago │",
                "│ > dotfiles                      5h ago │",
                "│   ─────────────────── 🪦 Dead sessions │",
                "│   api-spike                     5w ago │",
                "╰────────────────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn the_banner_gives_up_its_label_before_it_gives_up_the_name() {
        let banner = |width: usize, name: &str| {
            let rect = Rect { x: 0, y: 0, width, height: 5 };
            search_body(&attached(Vec::new(), None, name), &rect, 0)[0].content().to_owned()
        };
        assert_eq!(banner(24, "notes"), "   notes ─ 🏠 Current ");
        assert_eq!(banner(22, "notes"), "   notes ────────── ");
        assert_eq!(
            banner(42, "a-very-long-session-name-indeed"),
            "   a-very-long-session-name-indeed ──── "
        );
        assert_eq!(
            banner(42, "a-very-long-session-name-indeed-and-then-some"),
            "   a-very-long-session-name-indeed-and… "
        );
    }

    #[test]
    fn the_only_session_left_is_the_one_you_are_in() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 5 };
        let body = search_body(&attached(Vec::new(), None, "notes"), &rect, 0);
        assert_eq!(
            picture(&rect, TITLE, "", interior(&rect, &[], body)),
            vec![
                "╭─ luneta ───────────────────╮",
                "│                            │",
                "│   notes ─────── 🏠 Current │",
                "│ no other sessions          │",
                "╰────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn the_banner_keeps_the_top_line_however_far_the_list_scrolls() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 7 };
        let names: Vec<String> = (0..10).map(|i| format!("s{i}")).collect();
        for selected in 0..10 {
            let rows: Vec<Row> = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let kind = if i < 5 { Kind::Live } else { Kind::Resurrectable };
                    session(name, kind, HOUR)
                })
                .collect();
            let body = search_body(&attached(rows, Some(selected), "notes"), &rect, 0);
            assert_eq!(body.len(), rect.inner_height());
            let shown: Vec<&str> = body.iter().map(super::super::paint::Painted::content).collect();
            assert!(shown[0].starts_with("   notes "), "selected {selected}: {shown:?}");
            let carets: Vec<&&str> = shown.iter().filter(|l| l.starts_with(" > ")).collect();
            assert_eq!(carets.len(), 1, "selected {selected} fell off: {shown:?}");
            assert!(
                carets[0].starts_with(&format!(" > {} ", names[selected])),
                "selected {selected}: {shown:?}"
            );
        }
    }

    #[test]
    fn the_banner_takes_the_last_line_before_the_rows_do() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 3 };
        assert_eq!(rect.inner_height(), 1);
        let state = attached(vec![session("luneta", Kind::Live, HOUR)], Some(0), "notes");

        let body = search_body(&state, &rect, 0);
        assert_eq!(body.len(), 1);
        assert!(body[0].content().starts_with("   notes"));

        assert!(search_body(&state, &rect, 1).is_empty());
    }

    fn contents(panes: usize, tab: &str, title: &str) -> Contents {
        Contents {
            panes,
            focus: Some(Focus { pane: 7, tab: tab.to_owned(), title: title.to_owned() }),
        }
    }

    fn peeked(session: &str, pane: u32, screen: &str) -> Peeks {
        let mut peeks = Peeks::default();
        peeks.ingest((session.to_owned(), pane), Some(0), screen.as_bytes(), b"");
        peeks
    }

    fn beside(left: Vec<String>, right: Vec<String>) -> Vec<String> {
        left.into_iter().zip(right).map(|(left, right)| format!("{left}{right}")).collect()
    }

    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;

    #[test]
    fn the_list_hugs_the_bottom_of_its_box() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 8 };
        let state = matches(
            vec![
                session("luneta", Kind::Live, 2 * HOUR),
                session("old", Kind::Resurrectable, 3 * HOUR),
            ],
            Some(0),
        );
        let body = search_body(&state, &rect, 0);
        let right = count(state.rows.selected(), state.rows.len());
        assert_eq!(
            picture(&rect, TITLE, &right, interior(&rect, &[], body)),
            vec![
                "╭─ luneta ───────────── 1/2 ─╮",
                "│                            │",
                "│                            │",
                "│                            │",
                "│ > luneta            2h ago │",
                "│   ─────── 🪦 Dead sessions │",
                "│   old               3h ago │",
                "╰────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn an_all_live_list_gets_no_headstone() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 6 };
        let state = matches(
            vec![session("luneta", Kind::Live, HOUR), session("dotfiles", Kind::Live, 2 * HOUR)],
            Some(0),
        );
        let body = search_body(&state, &rect, 0);
        assert_eq!(body.len(), 2);
        for line in &body {
            assert!(!line.content().contains('🪦'), "{}", line.content());
        }
    }

    #[test]
    fn an_all_dead_list_still_gets_one() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 6 };
        let state = matches(vec![session("old", Kind::Resurrectable, HOUR)], Some(0));
        let body = search_body(&state, &rect, 0);
        assert_eq!(body.len(), 2);
        assert!(body[0].content().trim_end().ends_with("🪦 Dead sessions"));
        assert!(body[1].content().starts_with(" > old"));
    }

    #[test]
    fn the_headstone_costs_a_row_of_list() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 6 };
        assert_eq!(rect.inner_height(), 4);
        let rows = || {
            vec![
                session("live-1", Kind::Live, HOUR),
                session("live-2", Kind::Live, HOUR),
                session("dead-1", Kind::Resurrectable, HOUR),
                session("dead-2", Kind::Resurrectable, HOUR),
            ]
        };
        let body = search_body(&matches(rows(), Some(0)), &rect, 0);
        assert_eq!(body.len(), 4);
        assert_eq!(body.iter().filter(|l| l.content().contains('🪦')).count(), 1);
    }

    #[test]
    fn the_selection_survives_the_boundary() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 6 };
        let names: Vec<String> = (0..10).map(|i| format!("s{i}")).collect();
        for selected in 0..10 {
            let rows: Vec<Row> = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let kind = if i < 5 { Kind::Live } else { Kind::Resurrectable };
                    session(name, kind, HOUR)
                })
                .collect();
            let body = search_body(&matches(rows, Some(selected)), &rect, 0);
            assert_eq!(body.len(), rect.inner_height());
            let shown: Vec<&str> = body.iter().map(super::super::paint::Painted::content).collect();
            let carets: Vec<&&str> = shown.iter().filter(|l| l.starts_with(" > ")).collect();
            assert_eq!(carets.len(), 1, "selected {selected} fell off: {shown:?}");
            assert!(
                carets[0].starts_with(&format!(" > {} ", names[selected])),
                "selected {selected}: {shown:?}"
            );
        }
    }

    #[test]
    fn notes_ride_on_top_of_the_banner_and_the_list() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 7 };
        let state = attached(vec![session("luneta", Kind::Live, 2 * HOUR)], Some(0), "notes");
        let notes = vec![Note::error("delete \"old\" failed")];
        let body = search_body(&state, &rect, notes.len());
        assert_eq!(
            picture(&rect, TITLE, "", interior(&rect, &notes, body)),
            vec![
                "╭─ luneta ───────────────────╮",
                "│                            │",
                "│                            │",
                "│ delete \"old\" failed        │",
                "│   notes ─────── 🏠 Current │",
                "│ > luneta            2h ago │",
                "╰────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn a_long_note_never_reaches_the_border() {
        let rect = Rect { x: 0, y: 0, width: 24, height: 5 };
        let note = Note::error("zoxide: no such file or directory (/usr/bin/zoxide)");
        let lines = picture(&rect, TITLE, "", interior(&rect, &[note], Vec::new()));
        for line in &lines {
            assert_eq!(line.width(), rect.width, "{line}");
        }
        assert_eq!(lines[3], "│ zoxide: no such fil… │");
    }

    #[test]
    fn every_line_is_exactly_the_width_of_its_box() {
        let rows = || {
            vec![
                session("luneta", Kind::Live, 2 * HOUR),
                session("a-very-long-session-name-indeed", Kind::Resurrectable, 400 * HOUR),
                session("日本語版", Kind::Live, 60),
            ]
        };
        let currents = [None, Some("notes"), Some("a-very-long-session-name-indeed")];
        for width in 10..60 {
            for height in 3..10 {
                for notes in 0..3 {
                    for current in currents {
                        let rect = Rect { x: 0, y: 0, width, height };
                        let mut state = matches(rows(), Some(1));
                        state.current_session = current.map(str::to_owned);
                        let notes: Vec<Note> =
                            (0..notes).map(|i| Note::dim(format!("note {i}"))).collect();
                        let body = search_body(&state, &rect, notes.len());
                        let interior = interior(&rect, &notes, body);
                        assert_eq!(interior.len(), rect.inner_height(), "{width}x{height}");
                        for line in picture(&rect, TITLE, "9/9", interior) {
                            assert_eq!(line.width(), width, "{width}x{height}: {line}");
                            assert!(line.chars().next().is_some_and(|c| "╭╰│".contains(c)));
                            assert!(line.chars().last().is_some_and(|c| "╮╯│".contains(c)));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_list_explains_itself_where_the_rows_would_be() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 5 };
        let mut state = matches(Vec::new(), None);
        state.search_term = "desp".to_owned();
        let body = search_body(&state, &rect, 0);
        assert_eq!(
            picture(&rect, TITLE, "", interior(&rect, &[], body)),
            vec![
                "╭─ luneta ───────────────────╮",
                "│                            │",
                "│                            │",
                "│ no match for \"desp\"        │",
                "╰────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn the_input_line_pushes_the_action_to_the_right() {
        let rect = Rect { x: 0, y: 0, width: 40, height: 3 };
        let line = input_line(&rect, ("desp".to_owned(), Some("Attach".to_owned()), false));
        assert_eq!(line.content(), " > desp_               <ENTER> Attach ");
        assert_eq!(line.content().width(), rect.width - 2);
    }

    #[test]
    fn a_narrow_box_drops_the_action_not_the_term() {
        let rect = Rect { x: 0, y: 0, width: 18, height: 3 };
        let line = input_line(&rect, ("despesas".to_owned(), Some("Attach".to_owned()), false));
        assert_eq!(line.content(), " > despesas_    ");
        assert_eq!(line.content().width(), rect.width - 2);
    }

    #[test]
    fn an_overlong_term_is_cut_from_the_left() {
        let rect = Rect { x: 0, y: 0, width: 16, height: 3 };
        let line = input_line(&rect, ("a-very-long-name".to_owned(), None, false));
        assert_eq!(line.content(), " > …ong-name_ ");
        assert_eq!(line.content().width(), rect.width - 2);
    }

    #[test]
    fn the_counter_reports_position_over_total() {
        assert_eq!(count(Some(2), 47), "3/47");
        assert_eq!(count(Some(0), 1), "1/1");
        assert_eq!(count(None, 47), "47");
        assert_eq!(count(None, 0), "");
        assert_eq!(count(Some(0), 0), "");
    }

    #[test]
    fn viewport_does_not_scroll_a_list_that_fits() {
        assert_eq!(viewport(0, 5, 10), (0, 5));
        assert_eq!(viewport(4, 5, 5), (0, 5));
        assert_eq!(viewport(0, 0, 10), (0, 0));
    }

    #[test]
    fn viewport_scrolls_only_to_keep_the_selection_on_screen() {
        assert_eq!(viewport(2, 20, 5), (0, 5));
        assert_eq!(viewport(4, 20, 5), (0, 5));
        assert_eq!(viewport(5, 20, 5), (1, 6));
        assert_eq!(viewport(19, 20, 5), (15, 20));
    }

    #[test]
    fn viewport_windows_stay_in_bounds() {
        for total in 0..12 {
            for visible in 1..8 {
                for selected in 0..total.max(1) {
                    let (start, end) = viewport(selected, total, visible);
                    assert!(end <= total, "{selected}/{total}/{visible}");
                    assert!(start <= end);
                    assert_eq!(end - start, visible.min(total));
                    if total > 0 {
                        assert!((start..end).contains(&selected), "{selected}/{total}/{visible}");
                    }
                }
            }
        }
    }

    #[test]
    fn keys_text_drops_descriptions_before_it_drops_keys() {
        let keys: &[Key] = &[("<↓↑>", "Navigate", "Nav"), ("<ENTER>", "Select", "Select")];
        assert_eq!(keys_text(60, keys).content(), "<↓↑> - Navigate, <ENTER> - Select");
        assert_eq!(keys_text(30, keys).content(), "<↓↑> Navigate  <ENTER> Select");
        assert_eq!(keys_text(24, keys).content(), "<↓↑> Nav  <ENTER> Select");
        assert_eq!(keys_text(23, keys).content(), "<↓↑> Nav <ENTER> Select");
        assert_eq!(keys_text(0, keys).content(), "<↓↑>/<ENTER>");
    }

    #[test]
    fn keys_text_respects_the_width_it_is_given() {
        let keys: &[Key] = &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Select", "Select"),
            ("<TAB>", "Agents", "Agents"),
            ("<ESC>", "Close", "Close"),
        ];
        for width in 12..80 {
            let line = keys_text(width, keys);
            if line.content().contains(' ') {
                assert!(line.content().width() <= width, "width {width}: {}", line.content());
            }
        }
    }

    #[test]
    fn a_session_preview_shows_the_pane_it_names() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut state = matches(vec![session("dotfiles", Kind::Live, HOUR)], Some(0));
        state.contents.insert("dotfiles".to_owned(), contents(3, "editor", "nvim"));
        let peeks = peeked("dotfiles", 7, "one\ntwo\n> cargo test\nok\n\n\n");
        let (title, right, lines) = session_preview(&state, &peeks, &rect);
        assert_eq!((title.as_str(), right.as_str()), ("dotfiles", "3 panes"));
        assert_eq!(
            picture(&rect, &title, &right, filled(&rect, lines)),
            vec![
                "╭─ dotfiles ─── 3 panes ─╮",
                "│ editor · nvim          │",
                "│                        │",
                "│ one                    │",
                "│ two                    │",
                "│ > cargo test           │",
                "│ ok                     │",
                "╰────────────────────────╯",
            ]
        );
    }

    #[test]
    fn a_pane_says_so_while_it_is_being_read() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut state = matches(vec![session("dotfiles", Kind::Live, HOUR)], Some(0));
        state.contents.insert("dotfiles".to_owned(), contents(1, "editor", "nvim"));

        let unasked = session_preview(&state, &Peeks::default(), &rect).2;
        assert!(unasked[2].content().contains("reading…"));
        let mut peeks = Peeks::default();
        assert!(peeks.claim("dotfiles", 7));
        let asked = session_preview(&state, &peeks, &rect).2;
        assert_eq!(asked[2].content(), unasked[2].content());

        let empty = peeked("dotfiles", 7, "\n\n");
        assert!(session_preview(&state, &empty, &rect).2[2].content().contains("nothing on"));
    }

    #[test]
    fn a_dead_session_has_nothing_to_look_inside() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let state = matches(vec![session("api-spike", Kind::Resurrectable, HOUR)], Some(0));
        let (title, right, lines) = session_preview(&state, &Peeks::default(), &rect);
        assert_eq!((title.as_str(), right.as_str()), ("api-spike", ""));
        assert_eq!(
            picture(&rect, &title, &right, filled(&rect, lines)),
            vec![
                "╭─ api-spike ────────────╮",
                "│ not running            │",
                "│                        │",
                "│ there is a saved       │",
                "│ layout to bring it     │",
                "│ back from, and nothing │",
                "│ running to look inside │",
                "╰────────────────────────╯",
            ]
        );
    }

    #[test]
    fn an_empty_list_previews_nothing() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 6 };
        let (title, _, lines) = session_preview(&matches(Vec::new(), None), &Peeks::default(), &rect);
        assert_eq!(title, "Preview");
        assert_eq!(lines.len(), 3);
        assert!(lines[0].content().contains("nothing highlighted"));
    }

    #[test]
    fn a_directory_preview_lists_what_eza_said() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"18 /home/you/misc/luneta\n", b"");
        dirs.ingest_worktrees(Some(0), b"");
        dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
        dirs.ingest_listing(
            "/home/you/misc/luneta".to_owned(),
            Some(0),
            b"src/\nCargo.toml\nREADME.md\n",
            b"",
        );
        let (title, right, lines) = dir_preview(&dirs, &rect);
        assert_eq!((title.as_str(), right.as_str()), ("luneta", "3 items"));
        assert_eq!(
            picture(&rect, &title, &right, filled(&rect, lines)),
            vec![
                "╭─ luneta ───── 3 items ─╮",
                "│ /home/you/misc/luneta  │",
                "│                        │",
                "│ src/                   │",
                "│ Cargo.toml             │",
                "│ README.md              │",
                "│                        │",
                "╰────────────────────────╯",
            ]
        );
    }

    #[test]
    fn a_coloured_listing_still_fills_the_box() {
        for width in 12..48 {
            let rect = Rect { x: 0, y: 0, width, height: 10 };
            let mut dirs = DirSet::default();
            dirs.ingest(Some(0), b"18 /home/you/misc/luneta\n", b"");
            dirs.ingest_worktrees(Some(0), b"");
            dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
            dirs.ingest_listing(
                "/home/you/misc/luneta".to_owned(),
                Some(0),
                EZA.as_bytes(),
                b"",
            );
            let (title, right, lines) = dir_preview(&dirs, &rect);
            for line in picture(&rect, &title, &right, filled(&rect, lines)) {
                assert_eq!(panes::columns(&line), width, "width {width}: {line:?}");
            }
        }
    }

    #[test]
    fn a_directory_says_so_while_it_is_being_read() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"18 /home/you/misc/luneta\n", b"");
        dirs.ingest_worktrees(Some(0), b"");
        dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);

        let unasked = dir_preview(&dirs, &rect).2;
        assert!(unasked[2].content().contains("reading…"));
        assert!(dirs.begin_listing("/home/you/misc/luneta"));
        let asked = dir_preview(&dirs, &rect).2;
        assert_eq!(asked[2].content(), unasked[2].content());
        assert_eq!(dir_preview(&dirs, &rect).1, "");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_cuts_only_overlong_words() {
        assert_eq!(wrap("not running yet", 20), vec!["not running yet"]);
        assert_eq!(wrap("there is a saved layout", 12), vec!["there is a", "saved layout"]);
        assert_eq!(wrap("/an/extremely/long/path", 10), vec!["/an/extre…"]);
        assert!(wrap("", 10).is_empty());
        for width in 4..40 {
            for line in wrap("there is a saved layout to bring it back from", width) {
                assert!(line.width() <= width, "width {width}: {line}");
            }
        }
    }

    #[test]
    fn an_agent_luneta_cannot_place_is_left_out_and_counted() {
        let json = r#"[{"status":"waiting","status_age":60,"cwd":"/home/lorenzo/Projects/misc",
            "zellij":{"session":"bipa.git","pane":"4"}},
            {"status":"idle","status_age":60,"cwd":"/home/lorenzo/Projects/misc/luneta",
             "zellij":{"session":"luneta","pane":"0"}}]"#;
        let places = Places::of(&[("luneta", &[(0, "/home/lorenzo/Projects/misc/luneta")])]);
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), json.as_bytes(), b"");
        let live = agents::Live::new(Some("luneta"), &[], &places);
        agents.rebuild("", &live, None, Age::ZERO, Selection::SnapToTop);

        assert_eq!(agents.rows.len(), 1);
        assert_eq!(agents.rows[0].display, "luneta");
        let notes = agent_note_texts(&agents, 80);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "1 agent sits in a pane luneta cannot find");
        assert!(!notes[0].error);
    }

    #[test]
    fn the_agent_you_are_in_is_pinned_above_the_list() {
        let json = r#"[{"status":"idle","status_age":60,"cwd":"/w/notes",
            "name":"handoff","name_source":"user","zellij":{"session":"notes","pane":"1"}},
            {"status":"busy","status_age":60,"cwd":"/w/luneta",
             "zellij":{"session":"luneta","pane":"4"}}]"#;
        let places = Places::of(&[("notes", &[(1, "/w/notes")]), ("luneta", &[(4, "/w/luneta")])]);
        let rect = Rect { x: 0, y: 0, width: 42, height: 5 };
        let body = |origin: Option<u32>, term: &str| -> Vec<String> {
            let mut agents = AgentSet::default();
            agents.ingest(Some(0), json.as_bytes(), b"");
            let live = agents::Live::new(Some("notes"), &[], &places);
            agents.rebuild(term, &live, origin, Age::ZERO, Selection::SnapToTop);
            agent_body(&agents, term, &rect, 0, 0).iter().map(|l| l.content().to_owned()).collect()
        };
        let banner = "   handoff ───────────────── 🏠 Current ";

        let pinned = body(Some(1), "");
        assert_eq!(pinned[0], banner);
        assert!(pinned[1].starts_with(" > luneta "), "{pinned:?}");
        assert_eq!(pinned.len(), 2);
        assert_eq!(body(Some(1), "lun")[0], banner);
        assert_eq!(body(Some(1), "zzz"), [banner, " no match for \"zzz\"                     "]);
        assert!(body(None, "")[0].starts_with(" > handoff "));
    }

    #[test]
    fn the_agent_you_are_in_alone_is_no_other_agent() {
        let json = r#"[{"status":"idle","status_age":60,"cwd":"/w/notes",
            "zellij":{"session":"notes","pane":"1"}}]"#;
        let places = Places::of(&[("notes", &[(1, "/w/notes")])]);
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), json.as_bytes(), b"");
        let live = agents::Live::new(Some("notes"), &[], &places);
        agents.rebuild("", &live, Some(1), Age::ZERO, Selection::SnapToTop);
        let rect = Rect { x: 0, y: 0, width: 30, height: 5 };
        let body: Vec<String> =
            agent_body(&agents, "", &rect, 0, 0).iter().map(|l| l.content().to_owned()).collect();
        assert_eq!(body, ["   notes ─────── 🏠 Current ", " no other agents            "]);
    }

    #[test]
    #[ignore = "prints the screens; run with --ignored --nocapture to look at them"]
    fn print_the_screens() {
        let (rows, cols) = (16, 84);
        let screen = Screen::new(rows, cols);
        let sessions = snapshot();
        let mut state = MatchSet::default();
        state.refresh(&sessions, Some("notes".to_owned()));
        state.rows.move_selection(1);
        state.contents.insert("dotfiles".to_owned(), contents(3, "editor", "nvim"));
        let peeks = peeked(
            "dotfiles",
            7,
            "  1 //! luneta: a personal zellij session picker.\n  2 \n  3 mod agents;\n\
             \n\"src/main.rs\" 1005L, 41k\n",
        );
        let notes: Vec<Note> = Vec::new();
        let rect = screen.results.as_ref().unwrap();
        let body = search_body(&state, rect, notes.len());
        let right = count(state.rows.selected(), state.rows.len());
        let list = picture(rect, TITLE, &right, interior(rect, &notes, body));
        let rect = screen.preview.as_ref().unwrap();
        let (title, right, lines) = session_preview(&state, &peeks, rect);
        print_pane(
            beside(list, picture(rect, &title, &right, filled(rect, lines))),
            &screen,
            "Sessions",
            prompt_text(&state),
            &search_help(help_width(cols)),
        );

        let mut agents = AgentSet::default();
        agents.ingest(Some(0), AGENTS.as_bytes(), b"");
        let places = Places::of(&[
            ("misc", &[(12, "/home/lorenzo/Projects/misc/luneta")]),
            ("bipa", &[(3, "/home/lorenzo/Projects/Work/bipa")]),
            ("notes", &[(7, "/home/lorenzo/Documents")]),
        ]);
        let live = agents::Live::new(Some("notes"), &sessions.live, &places);
        agents.rebuild("", &live, Some(7), Age::ZERO, Selection::SnapToTop);
        let rect = screen.results.as_ref().unwrap();
        let notes = agent_note_texts(&agents, help_width(cols));
        let body = agent_body(&agents, "", rect, notes.len(), 0);
        let right = count(agents.rows.selected(), agents.rows.len());
        let list = picture(rect, TITLE, &right, interior(rect, &notes, body));
        let rect = screen.preview.as_ref().unwrap();
        let peeks = peeked("misc", 12, "> read the docs?\n\n  1. yes\n  2. no\n\n> _\n");
        let (title, lines) = agent_preview(&agents, &peeks, rect);
        print_pane(
            beside(list, picture(rect, &title, "", filled(rect, lines))),
            &screen,
            "Agents",
            agent_prompt(&agents, ""),
            &agents_help(help_width(cols)),
        );

        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), ZOXIDE.as_bytes(), b"");
        dirs.ingest_worktrees(Some(0), b"");
        dirs.rebuild("", &sessions, Some("notes"), Selection::SnapToTop);
        dirs.ingest_listing(
            "/home/lorenzo/Projects/misc/luneta".to_owned(),
            Some(0),
            EZA.as_bytes(),
            b"",
        );
        let rect = screen.results.as_ref().unwrap();
        let notes = dir_note_texts(&dirs);
        let body = dir_body(&dirs, "", rect, notes.len());
        let right = count(dirs.rows.selected(), dirs.rows.len());
        let list = picture(rect, TITLE, &right, interior(rect, &notes, body));
        let rect = screen.preview.as_ref().unwrap();
        let (title, right, lines) = dir_preview(&dirs, rect);
        print_pane(
            beside(list, picture(rect, &title, &right, filled(rect, lines))),
            &screen,
            "Directories",
            dir_prompt(&dirs, ""),
            &dirs_help(help_width(cols)),
        );
    }

    fn print_pane(boxes: Vec<String>, screen: &Screen, title: &str, prompt: Prompt, help: &Painted) {
        for line in boxes {
            println!("{line}");
        }
        let input = &screen.input;
        println!("{}", input.top(title, "").line);
        println!("{}{}{}", VERTICAL, input_line(input, prompt).content(), VERTICAL);
        println!("{}", input.bottom());
        println!("  {}\n", help.content());
    }

    const AGENTS: &str = r#"[
        {"status": "waiting", "status_age": 1080, "cwd": "/home/lorenzo/Projects/misc/luneta",
         "name": "luneta", "name_source": "user", "zellij": {"session": "misc", "pane": "12"}},
        {"status": "busy", "status_age": 1860, "cwd": "/home/lorenzo/Projects/Work/bipa",
         "zellij": {"session": "bipa", "pane": "3"}},
        {"status": "idle", "status_age": 300, "cwd": "/home/lorenzo/Documents",
         "zellij": {"session": "notes", "pane": "7"}},
        {"status": "idle", "status_age": 60, "cwd": "/home/lorenzo"}
    ]"#;

    fn snapshot() -> Sessions {
        let named =
            |name: &str, age: u64| Session { name: name.to_owned(), age: Age::from_secs(age) };
        Sessions {
            live: vec![named("luneta", 2 * HOUR), named("dotfiles", 5 * HOUR)],
            dead: vec![named("despesas-old", 12 * DAY), named("api-spike", 40 * DAY)],
        }
    }

    const ZOXIDE: &str = "9268 /home/lorenzo/Projects/misc/luneta\n\
        4102 /home/lorenzo/Projects/misc/homelab\n\
        1877 /home/lorenzo/Projects/Work/bipa\n\
        18 /home/lorenzo/.local/bin\n";

    const EZA: &str = "\x1b[34m\u{f4d4} \x1b[1msrc\x1b[0m/\n\
        \x1b[34m\u{e5ff} \x1b[1mtarget\x1b[0m/\n\
        \x1b[33m\u{e6a8} \x1b[1;4mCargo.toml\x1b[0m\n\
        \x1b[33m\u{e673} \x1b[1;4mMakefile\x1b[0m\n\
        \x1b[33m\u{f00ba} \x1b[1;4mREADME.md\x1b[0m\n";

    fn session_node(name: &str, folded: bool, waiting: usize, loudest: Option<&str>) -> Node {
        Node::Session(SessionNode {
            name: name.to_owned(),
            current: false,
            folded,
            waiting,
            loudest: loudest.map(|word| Status::parse(Some(word))),
            hits: Vec::new(),
        })
    }

    fn pane_node(session: &str, id: u32, label: &str, agent: Option<(&str, u64)>) -> Node {
        Node::Pane(PaneNode {
            session: session.to_owned(),
            id,
            tab: None,
            label: label.to_owned(),
            agent: agent.map(|(word, age)| Mark {
                status: Status::parse(Some(word)),
                age: Held::from_secs(age),
            }),
            checkout: None,
            here: false,
            hits: Vec::new(),
        })
    }

    fn here(node: Node) -> Node {
        match node {
            Node::Pane(pane) => Node::Pane(PaneNode { here: true, ..pane }),
            session @ Node::Session(_) => session,
        }
    }

    /// `node`, in a work tree on `branch` that stands as `status` says.
    fn on(branch: &str, status: &str, node: Node) -> Node {
        let checkout = Checkout { branch: branch.to_owned(), status: status.to_owned() };
        match node {
            Node::Pane(pane) => Node::Pane(PaneNode { checkout: Some(checkout), ..pane }),
            session @ Node::Session(_) => session,
        }
    }

    fn sidebar_tree(nodes: Vec<Node>, selected: usize) -> Tree {
        Tree::of(nodes, selected)
    }

    /// `workday`, and what its header counts: the busy agent folded away in
    /// floresta as well as the ones on show.
    fn workday_tree(selected: usize) -> Tree {
        let mut tree = sidebar_tree(workday(), selected);
        tree.summary = Summary { sessions: 3, waiting: 1, idle: 1, busy: 3 };
        tree
    }

    fn sidebar_picture(
        tree: &Tree,
        notes: &[Note],
        focused: bool,
        rows: usize,
        cols: usize,
    ) -> Vec<String> {
        let menu = tree.menu.as_ref().map_or(0, |menu| sidebar_menu_height(menu, cols));
        let shelf = Shelf::new(rows, notes.len(), focused, tree.search().is_some(), menu);
        sidebar_lines(tree, notes, &shelf, focused, cols, 0)
            .iter()
            .map(|line| line.content().to_owned())
            .collect()
    }

    fn workday() -> Vec<Node> {
        vec![
            session_node("bipa", false, 1, Some("waiting")),
            pane_node("bipa", 31, "handoff-eng-11259-pr4b", Some(("idle", 13 * HOUR))),
            pane_node("bipa", 39, "dda-bank-slip-paid-feature", Some(("waiting", 180))),
            pane_node("bipa", 47, "Handoff pix outflow request readback", Some(("busy", 12))),
            session_node("floresta", true, 0, Some("busy")),
            session_node("unnamed", false, 0, Some("busy")),
            here(pane_node("unnamed", 0, "orca-copy", Some(("busy", 12)))),
            pane_node("unnamed", 1, "~/P/m/unnamed", None),
        ]
    }

    /// Every label is cut at one column, whatever its age, so the `…` line up.
    #[test]
    fn the_sidebar_draws_each_session_over_its_panes() {
        let tree = workday_tree(6);
        assert_eq!(
            sidebar_picture(&tree, &[], false, 11, 30),
            vec![
                "─ 3 sessions ────── ●1 ✓1 ⠋3 ─",
                "                              ",
                " ▾ bipa ────────────────── ●1 ",
                "   ✓ handoff-eng-11259-p… 13h ",
                "   ● dda-bank-slip-paid-…  3m ",
                "   ⠋ Handoff pix outflow… 12s ",
                " ▸ floresta ─────────────── ⠋ ",
                " ▾ unnamed ────────────────── ",
                "▌  ⠋ orca-copy            12s ",
                "     ~/P/m/unnamed            ",
                "                              ",
            ]
        );
    }

    /// Agents in worktrees, a shell in no repository, and a branch too long
    /// for the pane beside its work tree's status.
    fn worktrees() -> Vec<Node> {
        vec![
            session_node("luneta", false, 0, None),
            on("main", "", pane_node("luneta", 1, "fold", Some(("busy", 12)))),
            here(on(
                "branch-name",
                "!?⇡",
                pane_node("luneta", 2, "branch-name", Some(("idle", 60))),
            )),
            pane_node("luneta", 3, "~/notes", None),
            session_node("elo", false, 0, None),
            on("feature/very-long-branch-name", "+", pane_node("elo", 4, "elo/main", None)),
        ]
    }

    /// The branch goes on a line of its own under the label, in gray and cut
    /// to leave its work tree's status room under the age, and is lit with
    /// its row.
    #[test]
    fn a_pane_s_branch_is_gray_on_the_line_under_its_label() {
        let tree = sidebar_tree(worktrees(), 0);
        let shelf = Shelf::new(11, 0, false, false, 0);
        let lines = sidebar_lines(&tree, &[], &shelf, false, 30, 0);
        let drawn: Vec<&str> = lines.iter().map(Painted::content).collect();
        assert_eq!(
            drawn,
            [
                "──────────────────────────────",
                "                              ",
                " ▾ luneta ─────────────────── ",
                "   ⠋ fold                 12s ",
                "     main                     ",
                "▌  ✓ branch-name           1m ",
                "▌    branch-name          !?⇡ ",
                "     ~/notes                  ",
                " ▾ elo ────────────────────── ",
                "     elo/main                 ",
                "     feature/very-long-bra… + ",
            ]
        );
        let lit: Vec<usize> = (0..lines.len()).filter(|at| lines[*at].is_selected()).collect();
        assert_eq!(lit, [HEADER + 3, HEADER + 4]);
        assert!((5..16).all(|at| lines[HEADER + 4].is_gray_at(at)));
        assert!(!(26..29).any(|at| lines[HEADER + 4].is_gray_at(at)));
        assert!(!lines[HEADER + 3].is_gray_at(5));
    }

    /// A row of two lines is one row: a click on either line lands on it, the
    /// view keeps the whole of the lit row in sight and cuts the last row it
    /// has no room for, the wheel counts both of its lines, and stops once the
    /// whole of the last row is in sight.
    #[test]
    fn a_row_of_two_lines_is_one_row_to_the_view_and_the_mouse() {
        let agents = AgentSet::default();
        let view = |tree: &Tree, focused: bool, rows: usize| -> Vec<usize> {
            (0..rows)
                .filter_map(|line| sidebar_row_at(tree, &agents, focused, rows, 30, line))
                .collect()
        };
        let mut tree = sidebar_tree(worktrees(), 0);
        assert_eq!(view(&tree, false, 11), [0, 1, 1, 2, 2, 3, 4, 5, 5]);
        assert_eq!(view(&tree, false, 10), [0, 1, 1, 2, 2, 3, 4, 5]);
        // Your pane is the third row.
        assert_eq!(view(&tree, false, 6), [1, 1, 2, 2]);

        // Four lines of tree, under the header and over the keys. Three lines
        // of the wheel take the session's row and the two of the pane under it.
        assert!(scroll_sidebar(&mut tree, &agents, true, 7, 30, 3));
        assert_eq!(view(&tree, true, 7), [2, 2, 3, 4]);
        assert!(scroll_sidebar(&mut tree, &agents, true, 7, 30, -1));
        assert_eq!(view(&tree, true, 7), [1, 1, 2, 2]);
        assert!(scroll_sidebar(&mut tree, &agents, true, 7, 30, 99));
        assert_eq!(view(&tree, true, 7), [3, 4, 5, 5]);
        assert!(!scroll_sidebar(&mut tree, &agents, true, 7, 30, 1));
    }

    /// While the sidebar does not have the focus, the highlight shows where
    /// you are; once it has it, the highlight goes with the cursor, and the
    /// mark stays on the pane you are in.
    #[test]
    fn the_highlight_is_on_your_pane_until_the_sidebar_has_the_focus() {
        let tree = workday_tree(2);
        for (focused, row) in [(false, 6), (true, 2)] {
            let shelf = Shelf::new(11, 0, focused, false, 0);
            let lines = sidebar_lines(&tree, &[], &shelf, focused, 30, 0);
            let lit: Vec<usize> = (0..lines.len()).filter(|at| lines[*at].is_selected()).collect();
            assert_eq!(lit, [HEADER + row], "focused: {focused}");
            assert!(lines[HEADER + 6].content().starts_with(HERE_MARK), "focused: {focused}");
        }
    }

    #[test]
    fn the_header_leaves_out_what_there_is_none_of() {
        let header =
            |summary: Summary, cols: usize| header_line(&summary, cols, 0).content().to_owned();
        let summary = Summary { sessions: 4, waiting: 2, idle: 0, busy: 1 };
        assert_eq!(header(summary, 30), "─ 4 sessions ───────── ●2 ⠋1 ─");
        assert_eq!(header(summary, 16), "──────── ●2 ⠋1 ─");
        assert_eq!(header(Summary { sessions: 1, ..Summary::default() }, 16), "─ 1 session ────");
        assert_eq!(header(Summary::default(), 6), "──────");
    }

    #[test]
    fn the_keys_are_only_spelled_out_while_the_sidebar_has_the_focus() {
        let tree = sidebar_tree(workday(), 0);
        let notes = [Note::error("claude-ps: gone")];
        let unfocused = sidebar_picture(&tree, &notes, false, 6, 32);
        assert_eq!(unfocused.len(), 6);
        assert_eq!(unfocused[5], " claude-ps: gone                ");
        assert!(Shelf::new(6, 1, false, false, 0).help_y.is_none());

        let focused = sidebar_picture(&tree, &notes, true, 6, 32);
        assert_eq!(focused.len(), 5);
        assert_eq!(focused[4], " claude-ps: gone                ");
        assert_eq!(Shelf::new(6, 1, true, false, 0).help_y, Some(5));
        // `?` is in sight at the width of the README's layout, for the rest.
        assert_eq!(sidebar_help(30).content(), "<↓↑>/<ENTER>/<←→>/<ESC>/<?>");
        assert!(search_help_line(30).content().width() <= 30);
    }

    #[test]
    fn the_search_box_opens_under_the_tree_and_moves_no_row() {
        let mut tree = sidebar_tree(workday(), 0);
        let closed = sidebar_picture(&tree, &[], true, 9, 32);
        tree.search_for("handoff-eng-11259".to_owned());
        let open = sidebar_picture(&tree, &[], true, 9, 32);
        assert_eq!(open.len(), closed.len() - 1);
        assert_eq!(open[..], closed[..open.len()]);

        let shelf = Shelf::new(9, 0, true, true, 0);
        assert_eq!((shelf.search_y, shelf.help_y), (Some(7), Some(8)));
        assert_eq!(search_line("pix", 30).content(), " /pix_                          ");
        assert_eq!(search_line("handoff-eng-11259", 10).content(), " /…g-11259_ ");

        let agents = AgentSet::default();
        assert_eq!(sidebar_row_at(&tree, &agents, true, 9, 32, 6), Some(4));
        assert_eq!(sidebar_row_at(&tree, &agents, true, 9, 32, 7), None);
    }

    #[test]
    fn the_sidebar_scrolls_to_keep_the_cursor_in_sight() {
        let tree = sidebar_tree(workday(), 7);
        let lines = sidebar_picture(&tree, &[], true, 6, 32);
        assert_eq!(lines.len(), 5);
        assert!(lines[4].contains("~/P/m/unnamed"), "{lines:?}");
        let agents = AgentSet::default();
        assert_eq!(sidebar_row_at(&tree, &agents, true, 6, 32, 2), Some(5));
        assert_eq!(sidebar_row_at(&tree, &agents, true, 6, 32, 4), Some(7));
        assert_eq!(sidebar_row_at(&tree, &agents, true, 6, 32, 5), None);
        assert_eq!(sidebar_row_at(&tree, &agents, true, 5, 32, 4), None);
    }

    /// A sidebar too short for both the cursor and the pane you are in shows
    /// your pane, lit, while it does not have the focus, and a click on it
    /// lands on it.
    #[test]
    fn a_short_sidebar_keeps_your_pane_in_sight_until_it_has_the_focus() {
        let tree = workday_tree(0);
        let shelf = Shelf::new(5, 0, false, false, 0);
        let lines = sidebar_lines(&tree, &[], &shelf, false, 30, 0);
        let lit: Vec<usize> = (0..lines.len()).filter(|at| lines[*at].is_selected()).collect();
        assert_eq!(lit, [HEADER + 2]);
        assert!(lines[HEADER + 2].content().starts_with(HERE_MARK));
        let agents = AgentSet::default();
        assert_eq!(sidebar_row_at(&tree, &agents, false, 5, 30, HEADER + 2), Some(6));
    }

    #[test]
    fn the_letters_a_search_lands_on_stand_out_on_every_session_row() {
        for current in [false, true] {
            let node = SessionNode {
                name: "misc".to_owned(),
                current,
                folded: false,
                waiting: 0,
                loudest: None,
                hits: vec![0, 2],
            };
            let line = branch_line(&node, 20, 0);
            assert_eq!(line.content().chars().skip(3).take(4).collect::<String>(), "misc");
            let [m, i, s, c] = [3, 4, 5, 6].map(|at| line.colour_at(at));
            assert_eq!((m, i), (s, c), "current: {current}");
            assert_eq!(m, Some(ACCENT), "current: {current}");
            assert_eq!(i, current.then_some(NAME), "current: {current}");
        }
    }

    /// Whatever ink a label takes, the letters a search lands on in it take a
    /// colour of their own.
    #[test]
    fn the_letters_a_search_lands_on_stand_out_on_every_pane_row() {
        let columns = Columns { glyph: GLYPH, age: 2 };
        for (here, agent) in [
            (false, Some("waiting")),
            (false, Some("idle")),
            (false, None),
            (true, Some("waiting")),
        ] {
            let node = PaneNode {
                session: "a".to_owned(),
                id: 0,
                tab: None,
                label: "misc".to_owned(),
                agent: agent.map(|word| Mark {
                    status: Status::parse(Some(word)),
                    age: Held::from_secs(4),
                }),
                checkout: None,
                here,
                hits: vec![0, 2],
            };
            let line = leaf_line(&node, &columns, 20, 0);
            assert_eq!(line.content().chars().skip(5).take(4).collect::<String>(), "misc");
            let [m, i, s, c] = [5, 6, 7, 8].map(|at| line.colour_at(at));
            assert_eq!((m, i), (s, c), "{agent:?}, here: {here}");
            assert!(m.is_some(), "{agent:?}, here: {here}");
            assert_ne!(m, i, "{agent:?}, here: {here}");
        }
    }

    /// The wheel moves the view and leaves the cursor and the highlight where
    /// they are, out of sight or not, until a key comes or the cursor goes to
    /// another row.
    #[test]
    fn the_wheel_moves_the_view_until_the_cursor_moves() {
        let mut tree = workday_tree(1);
        let agents = AgentSet::default();
        let top =
            |tree: &Tree, focused: bool| sidebar_row_at(tree, &agents, focused, 6, 32, HEADER);
        // Four rows of eight, around your pane on the seventh.
        assert_eq!(top(&tree, false), Some(3));

        assert!(scroll_sidebar(&mut tree, &agents, false, 6, 32, -3));
        assert_eq!(top(&tree, false), Some(0));
        assert!(!scroll_sidebar(&mut tree, &agents, false, 6, 32, -3));
        assert!(scroll_sidebar(&mut tree, &agents, false, 6, 32, 99));
        assert_eq!(top(&tree, false), Some(4));
        assert_eq!(tree.rows.selected(), Some(1));
        assert!(!sidebar_picture(&tree, &[], false, 6, 32)[HEADER].contains("bipa"));

        // The focus lights the cursor instead, and the view stays.
        assert_eq!(top(&tree, true), Some(4));
        tree.unscroll();
        assert_eq!(top(&tree, true), Some(0));

        assert!(scroll_sidebar(&mut tree, &agents, true, 6, 32, 3));
        assert_eq!(top(&tree, true), Some(3));
        assert!(tree.rows.select(2));
        assert_eq!(top(&tree, true), Some(0));
    }

    /// A click on a row the wheel brought into view lights it where it is:
    /// the view stays, and the row stays under the pointer.
    #[test]
    fn a_click_leaves_the_view_where_the_wheel_left_it() {
        let mut tree = workday_tree(0);
        let agents = AgentSet::default();
        let row_at =
            |tree: &Tree, line: usize| sidebar_row_at(tree, &agents, true, 6, 32, HEADER + line);
        // Three rows of eight, under the header and over the keys.
        assert!(scroll_sidebar(&mut tree, &agents, true, 6, 32, 3));
        assert_eq!(row_at(&tree, 0), Some(3));
        let clicked = row_at(&tree, 1).expect("a row on the second line");
        select_sidebar_row(&mut tree, &agents, true, 6, 32, clicked);
        assert_eq!(tree.rows.selected(), Some(clicked));
        assert_eq!(row_at(&tree, 0), Some(3));
        assert_eq!(row_at(&tree, 1), Some(clicked));
    }

    /// So does the click that brings the focus in, with your own session
    /// folded: no row is lit before the focus comes, and the cursor is lit
    /// after, but it is on your session's row both times.
    #[test]
    fn the_click_that_brings_the_focus_leaves_the_view_where_the_wheel_left_it() {
        let mut nodes = workday();
        nodes.truncate(5);
        nodes.extend([
            session_node("zeta", false, 0, None),
            pane_node("zeta", 5, "one", None),
            pane_node("zeta", 6, "two", None),
            session_node("unnamed", true, 0, None),
        ]);
        let mut tree = sidebar_tree(nodes, 8);
        let agents = AgentSet::default();
        let row_at = |tree: &Tree, focused: bool| {
            sidebar_row_at(tree, &agents, focused, 6, 32, HEADER + 1)
        };
        assert!(scroll_sidebar(&mut tree, &agents, false, 6, 32, -3));
        let clicked = row_at(&tree, false).expect("a row on the second line");
        select_sidebar_row(&mut tree, &agents, true, 6, 32, clicked);
        assert_eq!(row_at(&tree, true), Some(clicked));
    }

    #[test]
    fn a_session_row_folds_on_its_fold_and_nowhere_else() {
        let session = session_node("bipa", false, 0, None);
        let Node::Session(node) = &session else { panic!("a session row") };
        let line = branch_line(node, 30, 0);
        let glyph = line.content().chars().position(|c| c.to_string() == UNFOLDED);
        assert!(glyph.is_some_and(|at| folds_at(&session, at)), "{:?}", line.content());
        let folds: Vec<bool> = (0..6).map(|column| folds_at(&session, column)).collect();
        assert_eq!(folds, [true, true, true, false, false, false]);
        assert!(!folds_at(&pane_node("bipa", 31, "handoff", None), 1));
    }

    #[test]
    fn a_click_on_the_header_or_below_the_last_row_lands_on_nothing() {
        let tree = sidebar_tree(workday(), 0);
        let agents = AgentSet::default();
        assert_eq!(sidebar_row_at(&tree, &agents, false, 20, 32, 0), None);
        assert_eq!(sidebar_row_at(&tree, &agents, false, 20, 32, 1), None);
        assert_eq!(sidebar_row_at(&tree, &agents, false, 20, 32, 2), Some(0));
        assert_eq!(sidebar_row_at(&tree, &agents, false, 20, 32, 9), Some(7));
        assert_eq!(sidebar_row_at(&tree, &agents, false, 20, 32, 10), None);
        assert_eq!(sidebar_row_at(&Tree::default(), &agents, false, 20, 32, 2), None);
    }

    #[test]
    fn a_sidebar_with_no_sessions_says_so() {
        let mut tree = Tree::default();
        assert_eq!(
            sidebar_picture(&tree, &[], false, 2, 20),
            vec![" no sessions        ", "                    "]
        );
        tree.search_for(String::new());
        assert_eq!(sidebar_picture(&tree, &[], true, 3, 20)[0], " no sessions        ");
        tree.search_for("zz".to_owned());
        assert_eq!(sidebar_picture(&tree, &[], true, 3, 20)[0], " no match           ");
    }

    #[test]
    fn every_sidebar_line_is_exactly_the_width_of_the_pane() {
        let notes = [Note::dim("2 agents sit in panes luneta cannot find")];
        for cols in 2..48 {
            let search = search_line("handoff pix outflow", cols - 2);
            assert_eq!(search.content().width(), cols, "{cols}: {}", search.content());
            for rows in 1..12 {
                for (focused, searching, menu) in [
                    (false, false, None),
                    (true, false, None),
                    (true, true, None),
                    (true, false, Some('c')),
                ] {
                    let trees = [0, 3, 7].into_iter().flat_map(|selected| {
                        [workday_tree(selected), sidebar_tree(worktrees(), selected)]
                    });
                    for mut tree in trees {
                        if searching {
                            tree.search_for("pix".to_owned());
                        }
                        tree.menu = menu.and_then(|key| sidebar_menu(MENU_KDL, Some(key)));
                        let lines = sidebar_picture(&tree, &notes, focused, rows, cols);
                        let agents = AgentSet::default();
                        let shelf = sidebar_shelf(&tree, &agents, focused, rows, cols);
                        let menu_lines = shelf.menu_y.map_or(0, |y| rows - y);
                        let below = [shelf.search_y, shelf.help_y];
                        assert_eq!(
                            lines.len() + below.iter().flatten().count() + menu_lines,
                            rows,
                            "{cols}x{rows}"
                        );
                        assert!(below.iter().flatten().all(|y| *y >= lines.len()), "{rows}");
                        assert!(shelf.menu_y.is_none_or(|y| y == lines.len()), "{rows}");
                        assert!(shelf.header == 0 || shelf.tree > 0, "{cols}x{rows}");
                        for line in lines {
                            assert_eq!(line.width(), cols, "{cols}x{rows}: {line:?}");
                        }
                        let Some(menu) = &tree.menu else {
                            continue;
                        };
                        let (drawn, help) = sidebar_menu_lines(menu, menu_lines, cols);
                        let shown = drawn.len() + usize::from(help.is_some());
                        assert_eq!(shown, menu_lines, "{cols}x{rows}");
                        for line in drawn.iter().map(Painted::content) {
                            assert_eq!(line.width(), cols, "{cols}x{rows}: {line:?}");
                        }
                    }
                }
            }
        }
    }

    const MENU_KDL: &str = r#"
        c "Claude" {
            t "/thermo-nuclear-review"
            m "/make-illegal-states-unrepresentable"
            s "/tigerstyle"
        }
    "#;

    /// The menu in the sidebar, as `key` opens it for a pane that works in
    /// `/w/misc/unnamed`.
    fn sidebar_menu(entries: &str, key: Option<char>) -> Option<SidebarMenu> {
        let cwd = Some("/w/misc/unnamed".to_owned());
        let row = crate::menu::Row { pane: Some(0), cwd, branch: None, there: None };
        SidebarMenu::open(Some(entries), row, key, &mut crate::menu::Numbers::default(), 0)
    }

    /// The menu's lines in `lines` lines of a sidebar `cols` wide, the keys
    /// under them last.
    fn sidebar_menu_picture(menu: &SidebarMenu, lines: usize, cols: usize) -> Vec<String> {
        let (drawn, help) = sidebar_menu_lines(menu, lines, cols);
        let drawn = drawn.iter().map(|line| line.content().to_owned());
        drawn.chain(help.map(|help| format!(" {}", help.content()))).collect()
    }

    /// The menu opens at the bottom of the sidebar, in the place of its keys:
    /// a rule that names the group open and where an agent starts, the keys
    /// of the group, and the keys that go back and close. The tree makes room
    /// for it and keeps the row it is for in sight.
    #[test]
    fn the_menu_opens_at_the_bottom_of_the_sidebar() {
        let mut tree = workday_tree(6);
        tree.menu = sidebar_menu(MENU_KDL, Some('c'));
        let agents = AgentSet::default();
        let shelf = sidebar_shelf(&tree, &agents, true, 13, 30);
        assert_eq!((shelf.tree, shelf.menu_y), (6, Some(8)));
        assert_eq!((shelf.help_y, shelf.search_y), (None, None));
        let menu = tree.menu.as_ref().expect("a menu");
        let mut picture = sidebar_picture(&tree, &[], true, 13, 30);
        picture.extend(sidebar_menu_picture(menu, 13 - 8, 30));
        assert_eq!(
            picture,
            [
                "─ 3 sessions ────── ●1 ✓1 ⠋3 ─",
                "                              ",
                "   ✓ handoff-eng-11259-p… 13h ",
                "   ● dda-bank-slip-paid-…  3m ",
                "   ⠋ Handoff pix outflow… 12s ",
                " ▸ floresta ─────────────── ⠋ ",
                " ▾ unnamed ────────────────── ",
                "▌  ⠋ orca-copy            12s ",
                "─ Claude ────── misc/unnamed ─",
                " t  /thermo-nuclear-review    ",
                " m  /make-illegal-states-unr… ",
                " s  /tigerstyle               ",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        assert_eq!(sidebar_row_at(&tree, &agents, true, 13, 30, HEADER + 5), Some(6));
        assert_eq!(sidebar_row_at(&tree, &agents, true, 13, 30, 8), None);

        let root = sidebar_menu(MENU_KDL, None).expect("a menu");
        assert_eq!(
            sidebar_menu_picture(&root, 3, 30),
            ["─ luneta ────── misc/unnamed ─", " c  Claude ▸                  ", " <ESC> - Close"]
        );

        let cwd = Some("/w/misc/unnamed".to_owned());
        let row = crate::menu::Row { pane: Some(0), cwd, branch: None, there: None };
        let help = SidebarMenu::help(Some(MENU_KDL), row, &mut crate::menu::Numbers::default());
        assert_eq!(
            sidebar_menu_picture(&help, 6, 30),
            [
                "─ luneta ────── misc/unnamed ─",
                " c  Claude ▸                  ",
                " d  Delete ▸                  ",
                " /  Search                    ",
                " n  Next agent                ",
                " <ESC> - Close",
            ]
        );
    }

    /// A sidebar too short for the whole menu leaves the tree a row, which a
    /// note leaves it too, and cuts the entries first, then the rule. A rule
    /// too narrow for where an agent starts leaves it out, and a menu that
    /// cannot be read says why.
    #[test]
    fn a_short_sidebar_cuts_the_menu_and_keeps_its_keys() {
        let menu = sidebar_menu(MENU_KDL, Some('c')).expect("a menu");
        assert_eq!(Shelf::new(4, 0, true, false, 5).menu_y, Some(1));
        for (rows, cut) in [(4, (1, 0, Some(1))), (7, (1, 1, Some(2)))] {
            let shelf = Shelf::new(rows, 1, true, false, 5);
            assert_eq!((shelf.tree, shelf.notes, shelf.menu_y), cut, "{rows}");
        }
        assert_eq!(
            sidebar_menu_picture(&menu, 3, 30),
            [
                "─ Claude ────── misc/unnamed ─",
                " t  /thermo-nuclear-review    ",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        assert_eq!(sidebar_menu_picture(&menu, 1, 30), [" <BS> - Back, <ESC> - Close"]);
        assert!(sidebar_menu_picture(&menu, 0, 30).is_empty());
        assert_eq!(menu_rule("Claude", "misc/unnamed", 20).content(), "─ Claude ───────────");
        assert_eq!(menu_rule("Claude", "", 8).content(), "─ Cla… ─");

        let wrong = sidebar_menu("t", Some('t')).expect("a menu");
        let picture = sidebar_menu_picture(&wrong, sidebar_menu_height(&wrong, 30), 30);
        assert!(picture[0].starts_with("─ luneta ──"), "{picture:?}");
        assert!(picture[1].starts_with(" menu: \"t\" takes one string"), "{picture:?}");
    }

    /// The floating menu is wide enough, while a delete key asks, to spell out
    /// the keys that answer it, however narrow the group's keys are.
    #[test]
    fn the_keys_that_answer_a_close_key_are_spelled_out() {
        let keys = Keys::new(None, crate::close::Scope::Pane).expect("a menu");
        let asking = Shown::Asking(&keys, "Close its tab? 2 agents, 1 busy");
        let help = menu_parts(asking, MIN_ASKING - PAD * 2).help;
        assert!(help.content().contains("<y> Yes"), "{}", help.content());
    }

    /// The menu is as wide as its longest entry and its rule need, so that
    /// no prompt in it is cut, in the group open or at the root.
    #[test]
    fn the_menu_is_wide_enough_for_every_entry_it_shows() {
        let configuration = std::collections::BTreeMap::from([(
            crate::menu::ENTRIES.to_owned(),
            r#"c "Claude" { m "/make-illegal-states-unrepresentable"; t "/tigerstyle"; }"#
                .to_owned(),
        )]);
        let mut menu = Menu::new(0, 1, &configuration);
        for opened in [false, true] {
            let (width, height) = menu_size(&menu).expect("a size");
            let Some(Shown::Keys(keys)) = menu.shown() else { panic!("no keys") };
            assert_eq!(height, keys.entries().len() + MENU_CHROME, "opened: {opened}");
            for entry in keys.entries() {
                let line = menu_line(entry, width - PAD * 2);
                assert!(!line.content().contains('…'), "opened: {opened}: {}", line.content());
            }
            menu.key(&zellij_tile::prelude::KeyWithModifier::new(
                zellij_tile::prelude::BareKey::Char('c'),
            ));
        }
    }
}
