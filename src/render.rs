use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

use crate::agents::{self, AgentRow, AgentSet};
use crate::dirs::{DirRow, DirSet, Listing};
use crate::fetch::Fetch;
use crate::layout::{anchor, truncate, truncate_left, Border, Line, Rect, Screen, PAD, VERTICAL};
use crate::panes::{self, Peek, Peeks};
use crate::sessions::{Contents, Kind, MatchSet, Row};

const TAG: usize = 0;
const NAME: usize = 1;
const LABEL: usize = 2;
const ACCENT: usize = 3;

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
    draw_help(&screen, search_help(help_width(cols)));
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
    draw_help(&screen, dirs_help(help_width(cols)));
}

pub fn render_agents(
    agents: &AgentSet,
    peeks: &Peeks,
    term: &str,
    rows: usize,
    cols: usize,
    frame: u64,
) {
    let screen = Screen::new(rows, cols);
    let notes = agent_note_texts(agents, help_width(cols));

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
    draw_help(&screen, agents_help(help_width(cols)));
}

pub fn render_rename(current: &str, input: &str, error: Option<&str>, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);

    if let Some(rect) = &screen.full {
        let notes = vec![Note::dim(format!("renaming \"{}\" — the session you are in", current))];
        draw(rect, "Rename", "", interior(rect, &notes, Vec::new()));
    }
    let (action, is_error) = match error {
        Some(error) => (error, true),
        None => ("Rename", false),
    };
    draw_input(&screen, "Rename", (input.to_string(), Some(action.to_string()), is_error));
    draw_help(
        &screen,
        keys_text(
            help_width(cols),
            &[("<ENTER>", "Rename", "Rename"), ("<ESC>", "Cancel", "Cancel")],
        ),
    );
}

type Prompt = (String, Option<String>, bool);

fn help_width(cols: usize) -> usize {
    cols.saturating_sub(PAD * 2)
}

fn print_at(text: Text, x: usize, y: usize, width: usize) {
    print_text_with_coordinates(text, x, y, Some(width), None);
}

fn draw(rect: &Rect, title: &str, right: &str, interior: Vec<Text>) {
    print_at(border_text(rect.top(title, right)), rect.x, rect.y, rect.width);
    for (i, line) in interior.into_iter().enumerate() {
        draw_row(rect, rect.inner_y() + i, line);
    }
    print_at(Text::new(rect.bottom()).dim_all(), rect.x, rect.bottom_y(), rect.width);
}

fn draw_row(rect: &Rect, y: usize, row: Text) {
    let Some(inner) = rect.width.checked_sub(2) else {
        return;
    };
    let edge = || Text::new(VERTICAL.to_string()).dim_all();
    print_at(edge(), rect.x, y, 1);
    print_at(row, rect.x + 1, y, inner);
    print_at(edge(), rect.x + rect.width - 1, y, 1);
}

fn border_text(border: Border) -> Text {
    let rule = border.rule_indices();
    let Border { line, title, right } = border;
    let mut text = Text::new(line).dim_indices(rule);
    if let Some(range) = title {
        text = text.color_range(LABEL, range);
    }
    if let Some(range) = right {
        text = text.unbold_range(range.clone()).color_range(TAG, range);
    }
    text
}

fn interior(rect: &Rect, notes: &[Note], body: Vec<Text>) -> Vec<Text> {
    let block = anchor(rect, notes.len(), body.len());
    let mut lines: Vec<Text> = (rect.inner_y()..block.y).map(|_| blank_line(rect)).collect();
    lines.extend(notes.iter().take(block.notes).map(|note| note_line(rect, note)));
    lines.extend(body.into_iter().take(block.rows));
    lines
}

fn note_line(rect: &Rect, note: &Note) -> Text {
    let inner = rect.inner_width();
    let mut line = Line::new();
    let text = truncate(&note.text, inner);
    if note.error {
        line.push_error(&text);
    } else {
        line.push(&text, TAG);
    }
    line.finish(inner)
}

fn input_line(rect: &Rect, prompt: Prompt) -> Text {
    let (input, action, is_error) = prompt;
    let inner = rect.inner_width();
    let mut line = Line::new();
    line.push("> ", TAG);

    let (typed, _) = truncate_left(&format!("{}_", input), inner.saturating_sub(2));
    line.push(&typed, ACCENT);

    let room = inner.saturating_sub(line.columns() + GAP);
    if let Some(action) = action.filter(|_| room >= MIN_ACTION) {
        let action = truncate(&format!("<ENTER> {}", action), room);
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
        print_at(input_line(rect, prompt), rect.x, rect.y, rect.width);
        return;
    }
    draw(rect, title, "", vec![input_line(rect, prompt)]);
}

fn draw_help(screen: &Screen, help: Text) {
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

fn blank_line(rect: &Rect) -> Text {
    Text::new(rect.blank()).dim_all()
}

enum PreviewRow {
    Own(Text),
    Pane(String),
}

impl From<Text> for PreviewRow {
    fn from(text: Text) -> Self {
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

fn pane_row(inner: usize, line: &str) -> String {
    let line = panes::fit(line, inner);
    let pad = inner.saturating_sub(panes::columns(&line));
    format!(" {}{} ", line, " ".repeat(pad))
}

fn draw_preview(rect: &Rect, title: &str, right: &str, lines: Vec<PreviewRow>) {
    print_at(border_text(rect.top(title, right)), rect.x, rect.y, rect.width);
    for (i, row) in filled(rect, lines).into_iter().enumerate() {
        let y = rect.inner_y() + i;
        match row {
            PreviewRow::Own(text) => draw_row(rect, y, text),
            PreviewRow::Pane(line) => draw_pane_row(rect, y, &line),
        }
    }
    print_at(Text::new(rect.bottom()).dim_all(), rect.x, rect.bottom_y(), rect.width);
}

fn draw_pane_row(rect: &Rect, y: usize, line: &str) {
    let Some(inner) = rect.width.checked_sub(2) else {
        return;
    };
    let edge = || Text::new(VERTICAL.to_string()).dim_all();
    print_at(edge(), rect.x, y, 1);
    print!("\u{1b}[{};{}H\u{1b}[m{}\u{1b}[m", y + 1, rect.x + 2, panes::fit(line, inner));
    print_at(edge(), rect.x + rect.width - 1, y, 1);
}

fn filled(rect: &Rect, mut lines: Vec<PreviewRow>) -> Vec<PreviewRow> {
    let height = rect.inner_height();
    if lines.len() > height {
        let hidden = lines.len() - height + 1;
        lines.truncate(height.saturating_sub(1));
        lines.push(note_line(rect, &Note::dim(format!("… {} more", hidden))).into());
    }
    lines.resize_with(height, || blank_line(rect).into());
    lines
}

fn preview_line(inner: usize, text: &str, level: usize) -> PreviewRow {
    let mut line = Line::new();
    line.push(&truncate(text, inner), level);
    line.finish(inner).into()
}

fn wrapped_lines(inner: usize, text: &str, level: usize) -> Vec<PreviewRow> {
    wrap(text, inner).iter().map(|line| preview_line(inner, line, level)).collect()
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
        "Preview".to_string(),
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

fn caption(inner: usize, tab: &str, title: &str) -> Text {
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
                    .map(|line| PreviewRow::Pane(pane_row(inner, line))),
            );
            lines
        },
    }
}

fn plural(n: usize, thing: &str) -> String {
    match n {
        1 => format!("1 {}", thing),
        n => format!("{} {}s", n, thing),
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
            lines.extend(entries.iter().map(|entry| entry_line(inner, entry)));
        },
    }
    (row.name.clone(), right, lines)
}

fn entry_line(inner: usize, entry: &str) -> PreviewRow {
    PreviewRow::Pane(pane_row(inner, entry))
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
    lines.extend(screen_lines(rect, peeks, &row.session, row.pane, lines.len()));
    (row.label(), lines)
}

const MIN_PATH: usize = 12;

fn search_body(state: &MatchSet, rect: &Rect, notes: usize) -> Vec<Text> {
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
    let selected_line = state.rows.selected().map(line_of).unwrap_or(0);
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

const HERE: &str = "current";

fn here_line(current: &str, inner: usize) -> Text {
    let mut line = Line::new();
    line.gap(CARET);
    line.push(&truncate(&format!("🏠 {}", current), inner.saturating_sub(CARET)), NAME);
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

fn separator(inner: usize, label: &str) -> Text {
    let mut line = Line::new();
    line.gap(CARET);
    line.push(&truncate(label, inner.saturating_sub(CARET)), LABEL);
    if line.columns() < inner {
        line.gap(1);
        line.push(&"─".repeat(inner - line.columns()), TAG);
    }
    line.finish(inner)
}

fn result_line(row: &Row, selected: bool, name_budget: usize, inner: usize) -> Text {
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
            Kind::Live => (Some("Attach".to_string()), false),
            Kind::Resurrectable => (Some("Resurrect".to_string()), false),
        };
    }
    if state.is_own_name() {
        return (Some("already attached".to_string()), false);
    }
    if let Some(reason) = state.name_error() {
        return (Some(reason.to_string()), true);
    }
    if state.search_term.is_empty() {
        (Some("New session".to_string()), false)
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
        "no other sessions".to_string()
    } else {
        "no sessions".to_string()
    }
}

fn dir_body(dirs: &DirSet, term: &str, rect: &Rect, notes: usize) -> Vec<Text> {
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
) -> Text {
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
        return (term.to_string(), None, false);
    };
    (term.to_string(), Some(format!("Create \"{}\"", row.name)), false)
}

fn dir_note_texts(dirs: &DirSet) -> Vec<Note> {
    match &dirs.status {
        Fetch::Failed(reason) => vec![Note::error(reason)],
        _ => Vec::new(),
    }
}

fn dir_empty_text(dirs: &DirSet, term: &str) -> String {
    match &dirs.status {
        Fetch::Waiting => "asking zoxide…".to_string(),
        Fetch::Failed(_) => "no directories".to_string(),
        Fetch::Ready(_) if term.is_empty() => "zoxide knows nowhere yet".to_string(),
        Fetch::Ready(_) => format!("no match for \"{}\"", term),
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
) -> Vec<Text> {
    if agents.rows.is_empty() {
        return vec![note_line(rect, &Note::dim(agent_empty_text(agents, term)))];
    }
    let inner = rect.inner_width();
    let capacity = rect.inner_height().saturating_sub(notes);
    if capacity == 0 {
        return Vec::new();
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

    window
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected = agents.rows.selected() == Some(start + offset);
            agent_line(
                row,
                selected,
                &fit,
                name_column,
                tag_column,
                cwd_budget,
                inner,
                frame,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn agent_line(
    row: &AgentRow,
    selected: bool,
    fit: &AgentFit,
    name_column: usize,
    tag_column: usize,
    cwd_budget: Option<usize>,
    inner: usize,
    frame: u64,
) -> Text {
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
            let (cwd, _) = truncate_left(&short_cwd(&row.cwd), cwd_budget);
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
        Some(row) => (term.to_string(), Some(format!("Go to \"{}\"", row.label())), false),
        None => (term.to_string(), None, false),
    }
}

fn agent_note_texts(agents: &AgentSet, width: usize) -> Vec<Note> {
    match &agents.status {
        Fetch::Failed(reason) => vec![Note::error(truncate(reason, width))],
        _ => Vec::new(),
    }
}

fn agent_empty_text(agents: &AgentSet, term: &str) -> String {
    match &agents.status {
        Fetch::Waiting => "looking for agents…".to_string(),
        Fetch::Failed(_) => "no agents".to_string(),
        Fetch::Ready(_) if term.is_empty() => "no agents running".to_string(),
        Fetch::Ready(_) => format!("no match for \"{}\"", term),
    }
}

fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_string(),
        [only] => (*only).to_string(),
        [.., parent, base] => format!("{}/{}", parent, base),
    }
}

fn search_help(width: usize) -> Text {
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

fn dirs_help(width: usize) -> Text {
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

fn agents_help(width: usize) -> Text {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Go to agent", "Go"),
            ("<TAB>", "Directories", "Dirs"),
            ("<ESC>", "Close", "Close"),
        ],
    )
}

type Key<'a> = (&'a str, &'a str, &'a str);

fn keys_text(width: usize, keys: &[Key]) -> Text {
    let spelled = |sep: &str, joiner: &str, short: bool| {
        keys.iter()
            .map(|(k, long, brief)| format!("{}{}{}", k, sep, if short { brief } else { long }))
            .collect::<Vec<_>>()
            .join(joiner)
    };
    let line = [
        spelled(" - ", ", ", false),
        spelled(" ", "  ", false),
        spelled(" ", "  ", true),
        spelled(" ", " ", true),
    ]
    .into_iter()
    .find(|candidate| candidate.width() <= width)
    .unwrap_or_else(|| keys.iter().map(|(k, _, _)| *k).collect::<Vec<_>>().join("/"));

    let mut text = Text::new(&line).dim_all();
    for (key, _, _) in keys {
        text = text.color_substring(ACCENT, key);
    }
    text
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::cursor::Cursor;
    use crate::elapsed::Age;
    use crate::sessions::{Focus, Selection, Session, Sessions};

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
        Row::new(name.to_string(), kind, Age::from_secs(age), 0, vec![], false)
    }

    fn matches(rows: Vec<Row>, selected: Option<usize>) -> MatchSet {
        let mut state = MatchSet::default();
        state.rows = Cursor::seeded(rows, selected.unwrap_or(0));
        state
    }

    fn attached(rows: Vec<Row>, selected: Option<usize>, current: &str) -> MatchSet {
        let mut state = matches(rows, selected);
        state.current_session = Some(current.to_string());
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
                "│   🏠 notes ─────────────────── current │",
                "│   luneta                        2h ago │",
                "│ > dotfiles                      5h ago │",
                "│   🪦 Dead sessions ─────────────────── │",
                "│   api-spike                     5w ago │",
                "╰────────────────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn the_banner_gives_up_its_label_before_it_gives_up_the_name() {
        let banner = |width: usize, name: &str| {
            let rect = Rect { x: 0, y: 0, width, height: 5 };
            search_body(&attached(Vec::new(), None, name), &rect, 0)[0].content().to_string()
        };
        assert_eq!(banner(24, "notes"), "   🏠 notes ─ current ");
        assert_eq!(banner(22, "notes"), "   🏠 notes ─────── ");
        assert_eq!(
            banner(42, "a-very-long-session-name-indeed"),
            "   🏠 a-very-long-session-name-indeed ─ "
        );
        assert_eq!(
            banner(42, "a-very-long-session-name-indeed-and-then-some"),
            "   🏠 a-very-long-session-name-indeed-… "
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
                "│   🏠 notes ─────── current │",
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
            let shown: Vec<&str> = body.iter().map(|l| l.content()).collect();
            assert!(shown[0].starts_with("   🏠 notes "), "selected {selected}: {shown:?}");
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
        assert!(body[0].content().starts_with("   🏠 notes"));

        assert!(search_body(&state, &rect, 1).is_empty());
    }

    fn contents(panes: usize, tab: &str, title: &str) -> Contents {
        Contents {
            panes,
            focus: Some(Focus { pane: 7, tab: tab.to_string(), title: title.to_string() }),
        }
    }

    fn peeked(session: &str, pane: u32, screen: &str) -> Peeks {
        let mut peeks = Peeks::default();
        peeks.ingest((session.to_string(), pane), Some(0), screen.as_bytes(), b"");
        peeks
    }

    fn beside(left: Vec<String>, right: Vec<String>) -> Vec<String> {
        left.into_iter().zip(right).map(|(left, right)| format!("{}{}", left, right)).collect()
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
                "│   🪦 Dead sessions ─────── │",
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
        assert!(body[0].content().starts_with("   🪦 Dead sessions"));
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
            let shown: Vec<&str> = body.iter().map(|l| l.content()).collect();
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
                "│   🏠 notes ─────── current │",
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
                        state.current_session = current.map(str::to_string);
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
        state.search_term = "desp".to_string();
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
        let line = input_line(&rect, ("desp".to_string(), Some("Attach".to_string()), false));
        assert_eq!(line.content(), " > desp_               <ENTER> Attach ");
        assert_eq!(line.content().width(), rect.width - 2);
    }

    #[test]
    fn a_narrow_box_drops_the_action_not_the_term() {
        let rect = Rect { x: 0, y: 0, width: 18, height: 3 };
        let line = input_line(&rect, ("despesas".to_string(), Some("Attach".to_string()), false));
        assert_eq!(line.content(), " > despesas_    ");
        assert_eq!(line.content().width(), rect.width - 2);
    }

    #[test]
    fn an_overlong_term_is_cut_from_the_left() {
        let rect = Rect { x: 0, y: 0, width: 16, height: 3 };
        let line = input_line(&rect, ("a-very-long-name".to_string(), None, false));
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
    fn short_cwd_keeps_the_last_two_components() {
        assert_eq!(short_cwd("/home/you/Projects/misc/luneta"), "misc/luneta");
        assert_eq!(short_cwd("/home/you"), "home/you");
        assert_eq!(short_cwd("/home"), "home");
        assert_eq!(short_cwd("/"), "/");
        assert_eq!(short_cwd(""), "");
        assert_eq!(short_cwd("/misc/luneta/"), "misc/luneta");
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
        state.contents.insert("dotfiles".to_string(), contents(3, "editor", "nvim"));
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
        state.contents.insert("dotfiles".to_string(), contents(1, "editor", "nvim"));

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
        dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
        dirs.ingest_listing(
            "/home/you/misc/luneta".to_string(),
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
            dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
            dirs.ingest_listing(
                "/home/you/misc/luneta".to_string(),
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
    #[ignore = "prints the screens; run with --ignored --nocapture to look at them"]
    fn print_the_screens() {
        let (rows, cols) = (16, 84);
        let screen = Screen::new(rows, cols);
        let sessions = snapshot();
        let mut state = MatchSet::default();
        state.refresh(&sessions, Some("notes".to_string()));
        state.rows.move_selection(1);
        state.contents.insert("dotfiles".to_string(), contents(3, "editor", "nvim"));
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
            search_help(help_width(cols)),
        );

        let mut agents = AgentSet::default();
        agents.ingest(Some(0), AGENTS.as_bytes(), b"");
        agents.rebuild("", Some("notes"), None, Age::ZERO, Selection::SnapToTop);
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
            agents_help(help_width(cols)),
        );

        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), ZOXIDE.as_bytes(), b"");
        dirs.rebuild("", &sessions, Some("notes"), Selection::SnapToTop);
        dirs.ingest_listing(
            "/home/lorenzo/Projects/misc/luneta".to_string(),
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
            dirs_help(help_width(cols)),
        );
    }

    fn print_pane(boxes: Vec<String>, screen: &Screen, title: &str, prompt: Prompt, help: Text) {
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
            |name: &str, age: u64| Session { name: name.to_string(), age: Age::from_secs(age) };
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
}
