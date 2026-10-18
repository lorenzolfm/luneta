use unicode_width::UnicodeWidthStr;

use crate::agents::{self, AgentRow, AgentSet, Status};
use crate::dirs::{short_cwd, DirRow, DirSet, Listing};
use crate::fetch::Fetch;
use crate::layout::{
    anchor, truncate, truncate_left, Border, Ink, Line, Rect, Screen, PAD, VERTICAL,
};
use crate::paint::{self, Painted, SUCCESS};
use crate::panes::{self, Peek, Peeks};
use crate::sessions::{Contents, Kind, MatchSet, Row};
use crate::tree::{Node, PaneNode, SessionNode, Summary, Tree};

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

/// How a sidebar `rows` tall is cut up: the header and a blank line under it
/// at the top, the tree under them, the notes under it, the search box while
/// it is open, and a line of keys at the bottom while the sidebar has the
/// focus. The box opens at the bottom so that no row moves. The header goes
/// before it would leave the tree no row.
struct Shelf {
    header: usize,
    tree: usize,
    notes: usize,
    search_y: Option<usize>,
    help_y: Option<usize>,
}

impl Shelf {
    fn new(rows: usize, notes: usize, focused: bool, searching: bool) -> Self {
        let search = usize::from(searching && rows > 0);
        let help = usize::from(focused && rows > search + 1);
        let header = if rows > search + help + HEADER { HEADER } else { 0 };
        let notes = notes.min(rows - search - help - header);
        Shelf {
            header,
            tree: rows - search - help - header - notes,
            notes,
            search_y: (search == 1).then(|| rows - help - 1),
            help_y: (help == 1).then(|| rows - 1),
        }
    }
}

/// The header's rule, and the blank line under it.
const HEADER: usize = 2;

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
    let shelf = Shelf::new(rows, notes.len(), focused, term.is_some());
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
    let notes = agent_note_texts(agents, cols.saturating_sub(2)).len();
    let shelf = Shelf::new(rows, notes, focused, tree.search().is_some());
    let line = line.checked_sub(shelf.header).filter(|line| *line < shelf.tree)?;
    let (start, end) = viewport(tree.rows.selected().unwrap_or(0), tree.rows.len(), shelf.tree);
    let index = start + line;
    (index < end).then_some(index)
}

const UNFOLDED: &str = "▾";

const FOLDED: &str = "▸";

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
        lines.push(header_line(&tree.summary, cols, frame));
        lines.push(Painted::new(" ".repeat(cols)));
    }
    if tree.rows.is_empty() {
        if shelf.tree > 0 {
            let empty = if tree.narrowed() { "no match" } else { "no sessions" };
            lines.push(note_within(inner, &Note::dim(empty)));
        }
    } else {
        let (start, end) = viewport(tree.rows.selected().unwrap_or(0), tree.rows.len(), shelf.tree);
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
        // The highlight is on the cursor while the sidebar has the focus, and
        // on the pane you are in while it does not.
        let lit = if focused {
            tree.rows.selected()
        } else {
            tree.rows.iter().position(|node| matches!(node, Node::Pane(pane) if pane.here))
        };
        for (offset, node) in window.iter().enumerate() {
            let line = match node {
                Node::Session(node) => branch_line(node, cols, frame),
                Node::Pane(node) => leaf_line(node, &columns, cols, frame),
            };
            lines.push(if lit == Some(start + offset) { line.selected() } else { line });
        }
    }
    lines.resize_with(shelf.header + shelf.tree, || Painted::new(" ".repeat(cols)));
    lines.extend(notes.iter().take(shelf.notes).map(|note| note_within(inner, note)));
    lines
}

/// The rule along the top of the sidebar: how many sessions are live, and how
/// many agents across them wait for you, are done, and are at work. A count
/// of none is left out, and so is the word for the sessions where the counts
/// need its room.
fn header_line(summary: &Summary, cols: usize, frame: u64) -> Painted {
    let counts: Vec<(Status, usize)> = [
        (Status::Waiting, summary.waiting),
        (Status::Idle, summary.idle),
        (Status::Busy, summary.busy),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .collect();
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
        Status::Idle => Ink::Bold(SUCCESS),
        Status::Busy | Status::Unknown(_) => Ink::Level(TAG),
        Status::Shell | Status::Unreported => Ink::Dim,
    }
}

/// A pane's row: its dot, its label, and how long its agent has been in its
/// state. The label is plain, in the accent while its agent waits for you,
/// and dimmed with no agent. The pane you are in has the mark and a bold one.
fn leaf_line(node: &PaneNode, columns: &Columns, cols: usize, frame: u64) -> Painted {
    let mut line = Line::new();
    if node.here {
        line.push(HERE_MARK, Ink::Bold(ACCENT));
    } else {
        line.gap(1);
    }
    line.gap(2);
    let start = line.columns();
    if let Some(mark) = &node.agent {
        line.push(&agents::dot(&mark.status, frame), dot_ink(&mark.status));
    }
    line.pad_to(start + columns.glyph);
    line.gap(1);
    let label = match &node.agent {
        _ if node.here => Ink::Strong,
        Some(mark) if mark.status.is_waiting() => Ink::Bold(ACCENT),
        Some(_) => Ink::Plain,
        None => Ink::Dim,
    };
    // A label in the accent shows the letters a search lands on in a colour.
    let hit = if label == Ink::Bold(ACCENT) { Ink::Bold(NAME) } else { Ink::Bold(ACCENT) };
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

fn sidebar_help(width: usize) -> Painted {
    keys_text(
        width,
        &[
            ("<↓↑>", "Move", "Move"),
            ("<ENTER>", "Go", "Go"),
            ("<←→>", "Fold", "Fold"),
            ("<n>", "Next", "Next"),
            ("<ESC>", "Back", "Back"),
            ("</>", "Search", "Search"),
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
            label: label.to_owned(),
            agent: agent.map(|(word, age)| Mark {
                status: Status::parse(Some(word)),
                age: Held::from_secs(age),
            }),
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

    fn sidebar_tree(nodes: Vec<Node>, selected: usize) -> Tree {
        let mut tree = Tree::default();
        tree.rows = Cursor::seeded(nodes, selected);
        tree
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
        let shelf = Shelf::new(rows, notes.len(), focused, tree.search().is_some());
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

    /// While the sidebar does not have the focus, the highlight shows where
    /// you are; once it has it, the highlight goes with the cursor, and the
    /// mark stays on the pane you are in.
    #[test]
    fn the_highlight_is_on_your_pane_until_the_sidebar_has_the_focus() {
        let tree = workday_tree(2);
        for (focused, row) in [(false, 6), (true, 2)] {
            let shelf = Shelf::new(11, 0, focused, false);
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
        assert!(Shelf::new(6, 1, false, false).help_y.is_none());

        let focused = sidebar_picture(&tree, &notes, true, 6, 32);
        assert_eq!(focused.len(), 5);
        assert_eq!(focused[4], " claude-ps: gone                ");
        assert_eq!(Shelf::new(6, 1, true, false).help_y, Some(5));
        assert!(sidebar_help(30).content().width() <= 30);
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

        let shelf = Shelf::new(9, 0, true, true);
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
        let lines = sidebar_picture(&tree, &[], false, 5, 32);
        assert_eq!(lines.len(), 5);
        assert!(lines[4].contains("~/P/m/unnamed"), "{lines:?}");
        let agents = AgentSet::default();
        assert_eq!(sidebar_row_at(&tree, &agents, false, 5, 32, 2), Some(5));
        assert_eq!(sidebar_row_at(&tree, &agents, false, 5, 32, 4), Some(7));
        assert_eq!(sidebar_row_at(&tree, &agents, false, 5, 32, 5), None);
        assert_eq!(sidebar_row_at(&tree, &agents, true, 5, 32, 4), None);
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
            let [m, i, s, c] = [3, 4, 5, 6].map(|at| line.level_at(at));
            assert_eq!((m, i), (s, c), "current: {current}");
            assert_eq!(m, Some(ACCENT), "current: {current}");
            assert_eq!(i, current.then_some(NAME), "current: {current}");
        }
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
                for (focused, searching) in [(false, false), (true, false), (true, true)] {
                    for selected in [0, 3, 7] {
                        let mut tree = workday_tree(selected);
                        if searching {
                            tree.search_for("pix".to_owned());
                        }
                        let lines = sidebar_picture(&tree, &notes, focused, rows, cols);
                        let shelf = Shelf::new(rows, notes.len(), focused, searching);
                        let below = [shelf.search_y, shelf.help_y];
                        assert_eq!(
                            lines.len() + below.iter().flatten().count(),
                            rows,
                            "{cols}x{rows}"
                        );
                        assert!(below.iter().flatten().all(|y| *y >= lines.len()), "{rows}");
                        for line in lines {
                            assert_eq!(line.width(), cols, "{cols}x{rows}: {line:?}");
                        }
                    }
                }
            }
        }
    }
}
