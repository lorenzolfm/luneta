//! Drawing the picker.
//!
//! Everything here goes through zellij's own `Text` component and the
//! `print_text_with_coordinates` family rather than hand-written SGR. That is not cosmetic
//! preference: a `Text` is serialized to the host as a DCS payload and coloured *there*, from
//! the active theme's `StyleDeclaration`. So the picker picks up the user's theme (and its
//! selected/emphasis colours) for free, with no `Styling` palette to carry and no `ModeUpdate`
//! subscription to keep it current.
//!
//! Colour is expressed as *emphasis levels*, not colours:
//!
//! | level | used for                                        |
//! |-------|-------------------------------------------------|
//! | 0     | tags — the quietest thing on the row            |
//! | 1     | session names, chosen layout                    |
//! | 2     | labels, box titles, the age column              |
//! | 3     | the typed term, key caps, fuzzy-match hits      |
//!
//! Absolute coordinates are safe here — and are what upstream uses — because the host deletes
//! the plugin pane's viewport before feeding it each render (`plugin_pane.rs:243`). That also
//! retires the old "build one frame, print it with no trailing newline" dance: there is no
//! cursor to scroll off the top any more.
//!
//! ## One `Text` per row
//!
//! Rows used to be `Table` cells, and the host measured, padded and joined them. They are now
//! whole lines, borders included, measured here — because a bordered row is not a thing a
//! `Table` can express, and because the padding the host applied was invisible from this side.
//! Three things follow from the change, and they are why it was worth making:
//!
//! - The selected row is one `Text::selected()` spanning the full width, so the highlight is a
//!   continuous band rather than cells with gaps between them.
//! - A trailing column can be pushed flush against the right border, which `Table` has no way
//!   to ask for.
//! - The empty-cell trap is gone. A `Table` cell's text crossed the wire as a comma-separated
//!   list of its bytes, so `""` arrived as a list with no bytes rather than as text of length
//!   zero: the cell was dropped, and since the wire format is one flat run of cells cut into
//!   rows by a column count, *every* cell after it slid one place left. A row that dropped a
//!   cell ate the first cell of the row below. There are no cells now.
//!
//! The cost is arithmetic that used to be the host's, and roughly one `print_*` call per
//! visible row instead of one per table. Renders are throttled to ~1/s (`main.rs`'s
//! `polled || self.spinning()`), so that is ~30 calls a second in the ordinary case.

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

use crate::agents::{self, AgentRow, AgentSet, Status as AgentStatus};
use crate::dirs::{Action, DirRow, DirSet, Status};
use crate::layout::{anchor, truncate, truncate_left, Border, Line, Rect, Screen, PAD};
use crate::sessions::{format_age, Kind, MatchSet, Row};
use crate::{Pending, Rename};

/// Emphasis levels, named. See the table above.
const TAG: usize = 0;
const NAME: usize = 1;
const LABEL: usize = 2;
const ACCENT: usize = 3;

/// The blank columns between two columns of a row. Two, not the one the host used to insert,
/// because a full-width box has room for it and three columns jammed together read as one.
const GAP: usize = 2;

/// A note line: what it says, and whether saying it is bad news.
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

// ---------------------------------------------------------------------------------------------
// The screens
// ---------------------------------------------------------------------------------------------

pub fn render_search(state: &MatchSet, error: Option<&str>, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);
    let notes = note_texts(state, error);

    if let Some(rect) = &screen.results {
        let body = search_body(state, rect, notes.len());
        draw(rect, "Results", &count(state.selected, state.rows.len()), interior(rect, &notes, body));
    }
    draw_input(&screen, "Sessions", prompt_text(state));
    draw_help(&screen, search_help(help_width(cols)));
}

/// The directory screen: the places you go, and what `Enter` would make of each.
///
/// Deliberately the same shape as [`render_search`] — same two boxes, same help row, same
/// bottom-anchored list — so that `Tab` swaps the *contents* of a screen rather than the
/// screen. The columns differ because the second one answers a different question: sessions
/// show an age because age is what they are sorted by, and directories show their path because
/// frecency is not a thing you can print usefully.
pub fn render_dirs(dirs: &DirSet, term: &str, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);
    let notes = dir_note_texts(dirs);

    if let Some(rect) = &screen.results {
        let body = dir_body(dirs, term, rect, notes.len());
        draw(rect, "Results", &count(dirs.selected, dirs.rows.len()), interior(rect, &notes, body));
    }
    draw_input(&screen, "Directories", dir_prompt(dirs, term));
    draw_help(&screen, dirs_help(help_width(cols)));
}

/// The agent screen: who is running, what they are doing, and how long they have been doing it.
///
/// `frame` is the animation tick, and reaches exactly one thing: the busy spinner's glyph. It
/// is threaded down rather than read from `agents`, because it is a fact about *when we are
/// drawing*, not about the agents — the snapshot they came from is still frozen.
pub fn render_agents(agents: &AgentSet, term: &str, rows: usize, cols: usize, frame: u64) {
    let screen = Screen::new(rows, cols);
    let notes = agent_note_texts(agents, help_width(cols));

    if let Some(rect) = &screen.results {
        let body = agent_body(agents, term, rect, notes.len(), frame);
        draw(
            rect,
            "Results",
            &count(agents.selected, agents.rows.len()),
            interior(rect, &notes, body),
        );
    }
    draw_input(&screen, "Agents", agent_prompt(agents, term));
    draw_help(&screen, agents_help(help_width(cols)));
}

/// Renaming the session you are in — the only session `rename_session` can address.
pub fn render_rename(rename: &Rename, current: Option<&str>, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);

    if let Some(rect) = &screen.results {
        let notes = current
            .map(|current| {
                Note::dim(format!("renaming \"{}\" — the session you are in", current))
            })
            .into_iter()
            .collect::<Vec<_>>();
        draw(rect, "Rename", "", interior(rect, &notes, Vec::new()));
    }
    let (action, is_error) = match rename.error.as_deref() {
        Some(error) => (error, true),
        None => ("Rename", false),
    };
    draw_input(&screen, "Rename", (rename.input.clone(), Some(action.to_string()), is_error));
    draw_help(
        &screen,
        keys_text(
            help_width(cols),
            &[("<ENTER>", "Rename", "Rename"), ("<ESC>", "Cancel", "Cancel")],
        ),
    );
}

/// The confirm step before a session is killed or deleted. Nothing has happened at this point —
/// this screen is what makes that true, and `Esc` backs out of it.
///
/// The keys live on the help row rather than in the input box, unlike every other screen: the
/// input box here is not taking input, and printing `<ENTER> Kill` in both places would say the
/// same thing twice on the one screen where the reader is being asked to stop and read.
pub fn render_confirm(pending: &Pending, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);

    if let Some(rect) = &screen.results {
        // Spelling out what survives is the whole point of the screen: "kill" and "delete" look
        // alike on a keyboard and differ entirely in what you can get back.
        let note = match pending.kind {
            Kind::Live => Note::dim(pending.consequence()),
            Kind::Resurrectable => Note::error(pending.consequence()),
        };
        draw(rect, pending.verb(), "", interior(rect, &[note], Vec::new()));
    }
    let question = format!("{} \"{}\"?", pending.verb(), pending.name);
    draw_input(&screen, pending.verb(), (question, None, false));
    draw_help(
        &screen,
        keys_text(
            help_width(cols),
            &[("<ENTER>", pending.verb(), pending.verb()), ("<ESC>", "Cancel", "Cancel")],
        ),
    );
}

// ---------------------------------------------------------------------------------------------
// The chrome
// ---------------------------------------------------------------------------------------------

/// What the input box holds: the text being typed, what `Enter` would do with it, and whether
/// saying so is bad news.
type Prompt = (String, Option<String>, bool);

/// The help row is indented to line up with the content inside the boxes above it.
fn help_width(cols: usize) -> usize {
    cols.saturating_sub(PAD * 2)
}

fn print_at(text: Text, x: usize, y: usize, width: usize) {
    print_text_with_coordinates(text, x, y, Some(width), None);
}

/// Draw a box: top border, interior, bottom border.
fn draw(rect: &Rect, title: &str, right: &str, interior: Vec<Text>) {
    print_at(border_text(rect.top(title, right)), rect.x, rect.y, rect.width);
    for (i, line) in interior.into_iter().enumerate() {
        print_at(line, rect.x, rect.inner_y() + i, rect.width);
    }
    print_at(Text::new(rect.bottom()).dim_all(), rect.x, rect.bottom_y(), rect.width);
}

fn border_text(border: Border) -> Text {
    let rule = border.rule_indices();
    let Border { line, title, right } = border;
    let mut text = Text::new(line).dim_indices(rule);
    if let Some(range) = title {
        text = text.color_range(LABEL, range);
    }
    if let Some(range) = right {
        text = text.color_range(TAG, range);
    }
    text
}

/// A box's interior: blank rows, then the notes, then the body — bottom-anchored as one block,
/// so the list hugs the prompt and the notes ride on top of the list.
fn interior(rect: &Rect, notes: &[Note], body: Vec<Text>) -> Vec<Text> {
    let block = anchor(rect, notes.len(), body.len());
    let blank = rect.blank();
    let mut lines: Vec<Text> =
        (rect.inner_y()..block.y).map(|_| Text::new(&blank).dim_all()).collect();
    lines.extend(notes.iter().take(block.notes).map(|note| note_line(rect, note)));
    lines.extend(body.into_iter().take(block.rows));
    lines
}

/// ⚠️ Notes are hard-truncated to the box. A note wider than its box would paint over the right
/// border — the same defect that once let a long `RunCommandResult` failure reason, carrying an
/// absolute path, run past the pane and be overwritten mid-word by the help line.
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

/// `> typed_` on the left, what `Enter` would do on the right.
///
/// The two are pushed apart rather than run together because they answer different questions and
/// change at different times: the left half moves under your fingers, the right half changes
/// when the highlight does.
///
/// When they will not both fit, the action is cut first and dropped below [`MIN_ACTION`]. The
/// term you are typing is never the thing that gets cut — and when the term alone overruns the
/// box it is truncated from the *left*, because what you just typed is at its end.
fn input_line(rect: &Rect, prompt: Prompt) -> Text {
    let (input, action, is_error) = prompt;
    let inner = rect.inner_width();
    let mut line = Line::new();
    line.push("> ", TAG);

    // Trailing `_` is a cursor stand-in: a plugin's real cursor is off by default and turning it
    // on would mean tracking its position through every re-render.
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

/// The narrowest an action clause is worth printing: `<ENTER> ` and something after it.
const MIN_ACTION: usize = 12;

fn draw_input(screen: &Screen, title: &str, prompt: Prompt) {
    let rect = &screen.input;
    if !screen.bordered {
        // No room for a border. The one row left says what you are typing, which is the only
        // thing on this screen that cannot be inferred from anything else.
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

/// `3/47` — where the cursor is, over how many rows matched.
///
/// One token answering both "how many did my search leave?" and "how far down am I?", which is
/// what retired the `+N more` line: `1/47` says there are forty-six below without spending a
/// row to say it. With nothing selected there is no position to report, so only the count is.
fn count(selected: Option<usize>, total: usize) -> String {
    match selected {
        Some(index) if total > 0 => format!("{}/{}", index + 1, total),
        _ if total > 0 => total.to_string(),
        _ => String::new(),
    }
}

/// Keep the selection on screen, scrolling only when it would otherwise fall off. A centred
/// viewport would shift every row on every keystroke; row-index stability is a non-goal, but
/// gratuitous motion is still noise.
fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = if selected < visible { 0 } else { (selected + 1).saturating_sub(visible) };
    (start, (start + visible).min(total))
}

// ---------------------------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------------------------

/// The narrowest a path column is worth having. Below this it says nothing a `…` would not, so
/// the column is dropped and the name and tag get the room.
const MIN_PATH: usize = 12;

fn search_body(state: &MatchSet, rect: &Rect, notes: usize) -> Vec<Text> {
    if state.rows.is_empty() {
        return vec![note_line(rect, &Note::dim(empty_text(state)))];
    }
    let inner = rect.inner_width();
    let capacity = rect.inner_height().saturating_sub(notes);
    if capacity == 0 {
        return Vec::new();
    }

    // The separator takes a row of its own, so the window is over *display lines* rather than
    // over rows. Every index below is one or the other and never both: `row` indexes
    // `state.rows` and is what the selection means, `line` indexes what is drawn.
    let dead_at = dead_from(&state.rows);
    let line_of = |row: usize| row + usize::from(dead_at.is_some_and(|at| row >= at));
    let lines = state.rows.len() + usize::from(dead_at.is_some());
    let selected_line = state.selected.map(line_of).unwrap_or(0);
    let (start, end) = viewport(selected_line, lines, capacity);

    // Widths are measured over the *visible* window only. Measuring the whole list would make
    // the name column jump as you scroll, for the sake of names you cannot see.
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
        .map(|i| format_age(state.rows[*i].age).width())
        .max()
        .unwrap_or(0);
    // Two columns now that the tag is gone, so the name gets everything the age does not need
    // and there is nothing left to degrade — the `Full`/`AbbrTag`/`NoAge` ladder went with it.
    let name_budget = inner.saturating_sub(GAP + age_width).max(4);

    (start..end)
        .map(|line| match visible(line) {
            None => separator(inner, "🪦 Dead sessions"),
            Some(i) => {
                let selected = state.selected == Some(i);
                result_line(&state.rows[i], selected, name_budget, age_width, inner)
            },
        })
        .collect()
}

/// Where the dead sessions start, when any of them do.
///
/// A single position, not a search per row: `MatchSet` sorts live before resurrectable at every
/// stage (see its module doc, rule 2), so the list is always two groups and never an
/// interleaving. If that ever stops being true this returns a boundary that files live rows
/// under a headstone, which is why the sort keeps `kind_rank` above everything else.
fn dead_from(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|row| row.kind == Kind::Resurrectable)
}

/// `🪦 Dead sessions ──────────────────`
///
/// Chrome, not a `Row`. It never enters `MatchSet.rows`, so it cannot be selected, `↑`/`↓` step
/// over it without knowing it is there, and `selected_name()` cannot return it. It scrolls with
/// the list like any other line: pinning it would mean `viewport` reserving a row conditionally
/// on where the selection sits, and the input line already says `Resurrect` for the highlighted
/// row, so scrolling it away costs the grouping, not the meaning.
///
/// The rule runs to the right edge rather than being centred, which also makes it self-
/// correcting: `🪦` is East-Asian-Wide and a fair number of terminal fonts draw it in one cell
/// anyway, and a rule that ends at the border absorbs the disagreement.
fn separator(inner: usize, label: &str) -> Text {
    let mut line = Line::new();
    line.push(&truncate(label, inner), LABEL);
    if line.columns() < inner {
        line.gap(1);
        line.push(&"─".repeat(inner - line.columns()), TAG);
    }
    line.finish(inner)
}

fn result_line(
    row: &Row,
    selected: bool,
    name_budget: usize,
    age_width: usize,
    inner: usize,
) -> Text {
    let name = truncate(&row.name, name_budget);
    // A truncated name drops the indices that fell off the end — colouring a position that no
    // longer exists would paint the wrong character.
    let visible = name.chars().count();
    let hits: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible).collect();

    let mut line = Line::new();
    line.push_hits(&name, NAME, ACCENT, &hits);
    // The age is pushed flush against the right border rather than left-packed behind the
    // longest name. Two columns in a box as wide as the pane would otherwise huddle at the left
    // with half the box empty beside them, and the age column would shift every time the
    // longest visible name changed.
    let age = format_age(row.age);
    line.pad_to(inner.saturating_sub(age.width().max(age_width)));
    line.push(&age, LABEL);

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

/// The search term, with the `Enter` outcome spelled out beside it.
///
/// Putting the outcome in the input box rather than on a line of its own is what lets the
/// picker say what `Enter` does in both states — pointing at the highlighted row, or at the
/// literal text — without the two sentences ever contradicting each other.
fn prompt_text(state: &MatchSet) -> Prompt {
    let (action, is_error) = enter_action(state);
    (state.search_term.clone(), action, is_error)
}

/// What `Enter` will do, and whether saying so is bad news.
///
/// The refusals are the two states `confirm_search` turns into a no-op. Surfacing them here,
/// live and in the error colour, is what makes that no-op legible without an error overlay —
/// upstream's `show_error()` would swallow the next keystroke, and this is a state you wander
/// into by typing.
fn enter_action(state: &MatchSet) -> (Option<String>, bool) {
    if let Some(index) = state.selected {
        return match state.rows.get(index).map(|r| r.kind) {
            Some(Kind::Live) => (Some("Attach".to_string()), false),
            Some(Kind::Resurrectable) => (Some("Resurrect".to_string()), false),
            None => (None, false),
        };
    }
    if state.is_own_name() {
        // A no-op, not an error and not an offer to create a name that is already taken by the
        // session you are sitting in.
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

/// *"Where did my own session go?"* — from inside `despesas`, typing `desp` gives a blank list
/// and no explanation. Shown only when the term actually reaches for it; with an empty term you
/// can see the list and you know where you are.
///
/// It lives outside the result list on purpose: never a row, never selectable, never indexed —
/// which is what lets it talk about the current session without putting the current session
/// back into the match set it was deliberately taken out of.
fn note_texts(state: &MatchSet, error: Option<&str>) -> Vec<Note> {
    let mut notes = Vec::new();
    if let Some(error) = error {
        notes.push(Note::error(error));
    }
    if state.current_matches {
        if let Some(current) = state.current_session.as_ref() {
            notes.push(Note::dim(format!("you are in \"{}\" — not listed", current)));
        }
    }
    notes
}

fn empty_text(state: &MatchSet) -> String {
    if state.search_term.is_empty() {
        "no sessions".to_string()
    } else {
        format!("no match for \"{}\"", state.search_term)
    }
}

// ---------------------------------------------------------------------------------------------
// Directories
// ---------------------------------------------------------------------------------------------

fn dir_body(dirs: &DirSet, term: &str, rect: &Rect, notes: usize) -> Vec<Text> {
    if dirs.rows.is_empty() {
        return vec![note_line(rect, &Note::dim(dir_empty_text(dirs, term)))];
    }
    let inner = rect.inner_width();
    let capacity = rect.inner_height().saturating_sub(notes);
    if capacity == 0 {
        return Vec::new();
    }
    let (start, end) = viewport(dirs.selected.unwrap_or(0), dirs.rows.len(), capacity);
    let window = &dirs.rows[start..end];

    // The name is capped at a third of the width before anything else is decided. Nothing else
    // here has a natural size — a path will happily eat a whole row — so the cap is what keeps
    // two columns on screen instead of one and a half.
    let name_column =
        window.iter().map(|r| r.name.width()).max().unwrap_or(0).min(inner / 3).max(4);
    let path_budget = inner.saturating_sub(name_column + GAP);

    window
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected = dirs.selected == Some(start + offset);
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
    // The one row `Enter` will not act on, and with the `[HERE]` tag gone this is the only
    // thing on the row that says so. Greyed out is the universal spelling of "not actionable",
    // it costs no columns, and this is the only row on any screen where `Enter` does nothing —
    // so there is nothing else it could be confused with. The input line still spells out
    // `already in this session` when the highlight lands here.
    let refused = row.action == Action::Here;
    let level = if refused { TAG } else { NAME };

    let mut line = Line::new();
    // Not highlighted, because the term was never matched against it — the match ran on the
    // path, and painting hits onto a string they were not found in would be a lie that happens
    // to line up sometimes.
    line.push(&truncate(&row.name, name_column), level);
    line.pad_to(name_column);
    line.gap(GAP);

    let (path, dropped) = truncate_left(&row.path, path_budget);
    if refused {
        line.push(&path, TAG);
    } else {
        // The indices are into the *untruncated* path. Those that fell off the left are gone;
        // the rest shift down by what was dropped and back up by one for the `…` standing in
        // for it.
        let shift = usize::from(dropped > 0);
        let hits: Vec<usize> =
            row.indices.iter().filter(|i| **i >= dropped).map(|i| i - dropped + shift).collect();
        line.push_hits(&path, LABEL, ACCENT, &hits);
    }

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

/// The directory prompt names the session that would be made, not the directory that would be
/// entered. That is the thing you have to be able to argue with — the path is already on the
/// row, and the name is the part the plugin invented.
fn dir_prompt(dirs: &DirSet, term: &str) -> Prompt {
    let Some(row) = dirs.selected_row() else {
        return (term.to_string(), None, false);
    };
    let refused = row.action == Action::Here;
    let action = if refused {
        // The one row `Enter` will not act on. It belongs in the prompt rather than on a note
        // line because it is a property of the highlight, and it has to move with it.
        "already in this session".to_string()
    } else {
        format!("{} \"{}\"", row.action.verb(), row.name)
    };
    (term.to_string(), Some(action), refused)
}

/// The directory screen has three ways to be empty and they are not interchangeable. Only the
/// failure gets a note line — the other two are self-explanatory in the list's own place.
fn dir_note_texts(dirs: &DirSet) -> Vec<Note> {
    match &dirs.status {
        Status::Failed(reason) => vec![Note::error(reason)],
        _ => Vec::new(),
    }
}

fn dir_empty_text(dirs: &DirSet, term: &str) -> String {
    match &dirs.status {
        Status::Waiting => "asking zoxide…".to_string(),
        // The reason is already on the note line directly above; on a pane this small, saying
        // it twice costs more than the second copy is worth.
        Status::Failed(_) => "no directories".to_string(),
        Status::Ready if term.is_empty() => "zoxide knows nowhere yet".to_string(),
        Status::Ready => format!("no match for \"{}\"", term),
    }
}

// ---------------------------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------------------------

/// How much the agent row had to give up to fit the box's width.
///
/// The ladder is **abbreviate the tag → drop cwd → drop age**, and that order is a judgement
/// about what each column is for. Age outranks cwd because in the common case the label already
/// names the project the cwd would repeat, while nothing else on the row says how long an agent
/// has been stuck — which is the whole routing decision.
///
/// There was a rung above `Full` carrying a token count, dropped when `claude-ps` stopped
/// emitting `context`: its join was a guess off a lossy cwd slug and it was that tool's only
/// unbounded read. A rung no producer can reach is not a fallback, it is a branch that cannot
/// be exercised.
#[derive(PartialEq, Eq)]
enum AgentFit {
    /// name + [WAITING] + age + cwd
    Full,
    /// name + [W] + age + cwd
    AbbrTag,
    /// name + [W] + age
    NoCwd,
    /// name + [W]
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
    let (start, end) = viewport(agents.selected.unwrap_or(0), agents.rows.len(), capacity);
    let window = &agents.rows[start..end];

    // Measured at the same `frame` the cells below are built at, so a width and the glyph that
    // has to fit in it can never come from two different turns of the spinner — every spinner
    // frame is one column wide anyway (see `agents::SPINNER`), and passing the frame is what
    // keeps that a property of the spinner rather than an assumption made here.
    let full_tag =
        window.iter().map(|r| agents::full_tag(&r.status, frame).width()).max().unwrap_or(0);
    // Measured rather than assumed, unlike the directory screen's fixed `ABBR_TAG`: a glyph is
    // two columns wide and an unknown status's `[S]` fallback is three, so the narrow tag column
    // is not one width any more.
    let abbr_width =
        window.iter().map(|r| agents::abbr_tag(&r.status, frame).width()).max().unwrap_or(0);
    let age_width =
        window.iter().map(|r| agents::format_duration(r.age).width()).max().unwrap_or(0);
    // Capped at a third of the width before anything else is decided — the name is the one
    // column with a natural size, and letting it take what it likes is what turns four columns
    // into two and a half.
    let name_column =
        window.iter().map(|r| r.label().width()).max().unwrap_or(0).min(inner / 3).max(4);

    let fit = if name_column + GAP + full_tag + GAP + age_width + GAP + MIN_PATH <= inner {
        AgentFit::Full
    } else if name_column + GAP + abbr_width + GAP + age_width + GAP + MIN_PATH <= inner {
        AgentFit::AbbrTag
    } else if name_column + GAP + abbr_width + GAP + age_width <= inner {
        AgentFit::NoCwd
    } else {
        AgentFit::NoAge
    };

    let abbr = !matches!(fit, AgentFit::Full);
    let tag_column = if abbr { abbr_width } else { full_tag };
    let cwd_budget = match fit {
        AgentFit::Full | AgentFit::AbbrTag => Some(
            inner.saturating_sub(name_column + GAP + tag_column + GAP + age_width + GAP),
        ),
        _ => None,
    };

    window
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected = agents.selected == Some(start + offset);
            agent_line(
                row,
                selected,
                &fit,
                name_column,
                tag_column,
                age_width,
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
    age_width: usize,
    cwd_budget: Option<usize>,
    inner: usize,
    frame: u64,
) -> Text {
    let label = truncate(&row.label(), name_column);
    // The term was matched against the **bare** label, so a `:pane` suffix cannot carry a hit —
    // and a truncated label drops the indices that fell off the end, because colouring a
    // position that no longer exists paints the wrong character.
    let visible = label.chars().count();
    let hits: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible).collect();

    let mut line = Line::new();
    line.push_hits(&label, NAME, ACCENT, &hits);
    line.pad_to(name_column);
    line.gap(GAP);
    // The one status that is spelled in the accent colour. Every other status — including ones
    // released after this was written — renders as itself, quietly.
    let tag_level = if agents::is_waiting(&row.status) { ACCENT } else { TAG };
    let tag = if matches!(fit, AgentFit::Full) {
        agents::full_tag(&row.status, frame)
    } else {
        agents::abbr_tag(&row.status, frame)
    };
    line.push(&tag, tag_level);

    if !matches!(fit, AgentFit::NoAge) {
        line.pad_to(name_column + GAP + tag_column);
        line.gap(GAP);
        line.push(&agents::format_duration(row.age), LABEL);
    }
    if let Some(cwd_budget) = cwd_budget {
        line.pad_to(name_column + GAP + tag_column + GAP + age_width);
        line.gap(GAP);
        // Still `truncate_left` underneath: two components are short, but not bounded — a
        // single directory may be named anything at all.
        let (cwd, _) = truncate_left(&short_cwd(&row.cwd), cwd_budget);
        // Not highlighted: the match ran on the row's label, and painting hits onto a string
        // they were not found in would be a lie that happens to line up sometimes.
        line.push(&cwd, LABEL);
    }

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

/// The agent prompt names where `Enter` would put you — the session, and the pane when the
/// session alone does not say which.
fn agent_prompt(agents: &AgentSet, term: &str) -> Prompt {
    match agents.selected_row() {
        Some(row) => (term.to_string(), Some(format!("Go to \"{}\"", row.label())), false),
        None => (term.to_string(), None, false),
    }
}

/// Agents outside zellij are not rows — `Enter` could do nothing for them. Counting them here
/// is what keeps them from being *silently* absent: without this, an agent running in a plain
/// terminal is a name you can type that gives a blank list and no reason.
fn agent_note_texts(agents: &AgentSet, width: usize) -> Vec<Note> {
    let mut notes = Vec::new();
    if let AgentStatus::Failed(reason) = &agents.status {
        notes.push(Note::error(truncate(reason, width)));
    }
    let outside = match agents.outside {
        0 => return notes,
        1 => "1 agent not in zellij — not listed".to_string(),
        n => format!("{} agents not in zellij — not listed", n),
    };
    notes.push(Note::dim(outside));
    notes
}

fn agent_empty_text(agents: &AgentSet, term: &str) -> String {
    match &agents.status {
        AgentStatus::Waiting => "looking for agents…".to_string(),
        // The reason is already on the note line directly above; on a pane this small, saying
        // it twice costs more than the second copy is worth.
        AgentStatus::Failed(_) => "no agents".to_string(),
        AgentStatus::Ready if term.is_empty() => "no agents running".to_string(),
        AgentStatus::Ready => format!("no match for \"{}\"", term),
    }
}

/// The last two components of a path: `misc/luneta` for `/home/you/Projects/misc/luneta`.
///
/// Down a column of agents the leading components are the same `/home/you/…` on every row, so
/// they cost width without separating anything. What tells two agents apart is at the end.
///
/// Two components rather than one, for the reason [`crate::dirs`] derives session names from
/// two: measured across a real 136-path zoxide database, the last-two form collided **zero**
/// times where the bare basename collided nine ways (`master`, `backend`, `frontend`, `bin`,
/// …). One component would be shorter and would routinely name two different projects the same
/// thing, on the one screen whose job is telling agents apart.
///
/// ⚠️ No `…` marks the elision, unlike `truncate_left`. This is an abbreviation applied to
/// every row by the same rule, not a row running out of room — a marker on all of them would
/// be noise carrying no per-row information.
fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        // `/` itself, or something that is not a path at all.
        [] => path.to_string(),
        [only] => (*only).to_string(),
        [.., parent, base] => format!("{}/{}", parent, base),
    }
}

// ---------------------------------------------------------------------------------------------
// The help row
// ---------------------------------------------------------------------------------------------

fn search_help(width: usize) -> Text {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Select", "Select"),
            ("<TAB>", "Agents", "Agents"),
            ("<Ctrl r>", "Rename", "Rename"),
            ("<Del>", "Delete", "Delete"),
            ("<ESC>", "Deselect/Close", "Close"),
        ],
    )
}

/// The directory screen's keys. Shorter than the session screen's because two of that screen's
/// keys have nothing to act on here: there is no rename that means anything to a directory, and
/// deleting one would be a different verb against a different database.
fn dirs_help(width: usize) -> Text {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Go there", "Go"),
            ("<TAB>", "Sessions", "Sessions"),
            ("<ESC>", "Back", "Back"),
        ],
    )
}

/// The agent screen's keys. `Enter` does the same thing on every row, so there is only one to
/// name — and the third screen has to fit the same 60%-of-terminal pane as the other two.
fn agents_help(width: usize) -> Text {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Go to agent", "Go"),
            ("<TAB>", "Directories", "Dirs"),
            ("<ESC>", "Back", "Back"),
        ],
    )
}

/// A key and the two lengths of its description, longest first.
type Key<'a> = (&'a str, &'a str, &'a str);

/// The help line in the most detail that fits, over four spellings: `<KEY> - Description, …`,
/// the same without the dashes, the same with short descriptions, and finally keys alone.
///
/// The default floating pane is 60% of the terminal, which puts the search screen's six keys
/// past the first two spellings on any ordinary terminal — the short tier is what the user
/// actually sees, not a rarely-hit fallback. Dropping descriptions before dropping a key is
/// deliberate: a key you cannot see is a feature you will never find.
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
    use std::time::Duration;

    use super::*;

    /// A pane, rendered to the lines it would print. The picture, in other words — which is the
    /// thing that used to be unknowable from inside this crate, because the host assembled it.
    fn picture(rect: &Rect, title: &str, right: &str, interior: Vec<Text>) -> Vec<String> {
        let mut lines = vec![rect.top(title, right).line];
        lines.extend(interior.into_iter().map(|line| line.content().to_string()));
        lines.push(rect.bottom());
        lines
    }

    fn session(name: &str, kind: Kind, age: u64) -> Row {
        Row::new(name.to_string(), kind, Duration::from_secs(age), 0, vec![], false)
    }

    fn matches(rows: Vec<Row>, selected: Option<usize>) -> MatchSet {
        let mut state = MatchSet::default();
        state.rows = rows;
        state.selected = selected;
        state
    }

    const HOUR: u64 = 3600;

    /// The whole frame, exactly: the list pinned to the bottom of its box rather than the top,
    /// the age flush against the right border, and the headstone between the two groups doing
    /// the job the `[ATTACH]`/`[RESURRECT]` tags used to do per row.
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
        let right = count(state.selected, state.rows.len());
        assert_eq!(
            picture(&rect, "Results", &right, interior(&rect, &[], body)),
            vec![
                "╭─ Results ──────────── 1/2 ─╮",
                "│                            │",
                "│                            │",
                "│                            │",
                "│ luneta              2h ago │",
                "│ 🪦 Dead sessions ───────── │",
                "│ old                 3h ago │",
                "╰────────────────────────────╯",
            ]
        );
    }

    /// No dead sessions, no headstone. The separator is drawn exactly when there is a group
    /// for it to label, so the common case — nothing dead — spends no row on saying so.
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

    /// A list that is only dead sessions still gets its headstone. Without it — and with the
    /// tags gone — nothing on screen would say that `Enter` resurrects rather than attaches.
    #[test]
    fn an_all_dead_list_still_gets_one() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 6 };
        let state = matches(vec![session("old", Kind::Resurrectable, HOUR)], Some(0));
        let body = search_body(&state, &rect, 0);
        assert_eq!(body.len(), 2);
        assert!(body[0].content().starts_with("│ 🪦 Dead sessions"));
        assert!(body[1].content().starts_with("│ old"));
    }

    /// The separator is a display line, so it takes a row from the window like anything else.
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
        // Four rows and a separator make five display lines for four rows of box, so one row
        // scrolls off — not all four rows plus a separator crammed into four.
        let body = search_body(&matches(rows(), Some(0)), &rect, 0);
        assert_eq!(body.len(), 4);
        assert_eq!(body.iter().filter(|l| l.content().contains('🪦')).count(), 1);
    }

    /// Scrolling into the dead group keeps the selection on screen, which is the thing the
    /// row-index/display-line split exists to get right: the two disagree by one from the
    /// boundary onwards, and windowing on the wrong one loses the cursor.
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
            assert!(
                shown.iter().any(|l| l.starts_with(&format!("│ {} ", names[selected]))),
                "selected {selected} fell off: {shown:?}"
            );
        }
    }

    /// Notes ride on top of the list and the pair anchors as one block, so a note never lands
    /// between the highlighted row and the caret below it.
    #[test]
    fn notes_ride_on_top_of_the_list() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 7 };
        let state = matches(vec![session("luneta", Kind::Live, 2 * HOUR)], Some(0));
        let notes = vec![Note::dim("you are in \"desp\" — not listed")];
        let body = search_body(&state, &rect, notes.len());
        assert_eq!(
            picture(&rect, "Results", "", interior(&rect, &notes, body)),
            vec![
                "╭─ Results ──────────────────╮",
                "│                            │",
                "│                            │",
                "│                            │",
                "│ you are in \"desp\" — not l… │",
                "│ luneta              2h ago │",
                "╰────────────────────────────╯",
            ]
        );
    }

    /// A note wider than the box is cut to fit. Letting one run on would paint over the right
    /// border — the bordered version of the defect that used to let it be overwritten mid-word.
    #[test]
    fn a_long_note_never_reaches_the_border() {
        let rect = Rect { x: 0, y: 0, width: 24, height: 5 };
        let note = Note::error("zoxide: no such file or directory (/usr/bin/zoxide)");
        let lines = picture(&rect, "Results", "", interior(&rect, &[note], Vec::new()));
        for line in &lines {
            assert_eq!(line.width(), rect.width, "{line}");
        }
        assert_eq!(lines[3], "│ zoxide: no such fil… │");
    }

    /// Whatever the pane's size and whatever is in it, every line of a box is exactly as wide
    /// as the box and closed at both ends. This is the invariant a bordered layout lives or
    /// dies by: one short line and the right-hand border develops a hole.
    #[test]
    fn every_line_is_exactly_the_width_of_its_box() {
        // Rebuilt per case rather than cloned: `Row` is not `Clone`, and making it so to
        // please a test would be the test reaching into the production type.
        let rows = || {
            vec![
                session("luneta", Kind::Live, 2 * HOUR),
                session("a-very-long-session-name-indeed", Kind::Resurrectable, 400 * HOUR),
                // Four characters, eight columns — the case where counting characters instead
                // of columns puts the right border in the wrong place.
                session("日本語版", Kind::Live, 60),
            ]
        };
        for width in 10..60 {
            for height in 3..10 {
                for notes in 0..3 {
                    let rect = Rect { x: 0, y: 0, width, height };
                    let state = matches(rows(), Some(1));
                    let notes: Vec<Note> =
                        (0..notes).map(|i| Note::dim(format!("note {i}"))).collect();
                    let body = search_body(&state, &rect, notes.len());
                    let interior = interior(&rect, &notes, body);
                    assert_eq!(interior.len(), rect.inner_height(), "{width}x{height}");
                    for line in picture(&rect, "Results", "9/9", interior) {
                        assert_eq!(line.width(), width, "{width}x{height}: {line}");
                        assert!(line.chars().next().is_some_and(|c| "╭╰│".contains(c)));
                        assert!(line.chars().last().is_some_and(|c| "╮╯│".contains(c)));
                    }
                }
            }
        }
    }

    /// An empty list still says why it is empty, in the place a row would have been.
    #[test]
    fn an_empty_list_explains_itself_where_the_rows_would_be() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 5 };
        let mut state = matches(Vec::new(), None);
        state.search_term = "desp".to_string();
        let body = search_body(&state, &rect, 0);
        assert_eq!(
            picture(&rect, "Results", "", interior(&rect, &[], body)),
            vec![
                "╭─ Results ──────────────────╮",
                "│                            │",
                "│                            │",
                "│ no match for \"desp\"        │",
                "╰────────────────────────────╯",
            ]
        );
    }

    /// The caret and the term on the left, what `Enter` would do flush against the right.
    #[test]
    fn the_input_line_pushes_the_action_to_the_right() {
        let rect = Rect { x: 0, y: 0, width: 40, height: 3 };
        let line = input_line(&rect, ("desp".to_string(), Some("Attach".to_string()), false));
        assert_eq!(line.content(), "│ > desp_               <ENTER> Attach │");
        assert_eq!(line.content().width(), rect.width);
    }

    /// The term is never the thing that gets cut. Below the width an action clause needs, the
    /// action goes and the term keeps the box to itself.
    #[test]
    fn a_narrow_box_drops_the_action_not_the_term() {
        let rect = Rect { x: 0, y: 0, width: 18, height: 3 };
        let line = input_line(&rect, ("despesas".to_string(), Some("Attach".to_string()), false));
        assert_eq!(line.content(), "│ > despesas_    │");
        assert_eq!(line.content().width(), rect.width);
    }

    /// A term longer than the box keeps its tail: what you just typed is at the end, and the
    /// cursor stand-in has to stay next to it.
    #[test]
    fn an_overlong_term_is_cut_from_the_left() {
        let rect = Rect { x: 0, y: 0, width: 16, height: 3 };
        let line = input_line(&rect, ("a-very-long-name".to_string(), None, false));
        assert_eq!(line.content(), "│ > …ong-name_ │");
        assert_eq!(line.content().width(), rect.width);
    }

    /// `3/47` while something is selected, the bare count when nothing is, nothing at all when
    /// there is nothing to count.
    #[test]
    fn the_counter_reports_position_over_total() {
        assert_eq!(count(Some(2), 47), "3/47");
        assert_eq!(count(Some(0), 1), "1/1");
        assert_eq!(count(None, 47), "47");
        assert_eq!(count(None, 0), "");
        assert_eq!(count(Some(0), 0), "");
    }

    /// A list that fits shows all of it, from the top, whatever is selected.
    #[test]
    fn viewport_does_not_scroll_a_list_that_fits() {
        assert_eq!(viewport(0, 5, 10), (0, 5));
        assert_eq!(viewport(4, 5, 5), (0, 5));
        assert_eq!(viewport(0, 0, 10), (0, 0));
    }

    /// Scrolling starts only once the selection would fall off the bottom, and then moves by
    /// exactly as much as it has to. Gratuitous motion is the thing being avoided.
    #[test]
    fn viewport_scrolls_only_to_keep_the_selection_on_screen() {
        assert_eq!(viewport(2, 20, 5), (0, 5));
        assert_eq!(viewport(4, 20, 5), (0, 5));
        assert_eq!(viewport(5, 20, 5), (1, 6));
        assert_eq!(viewport(19, 20, 5), (15, 20));
    }

    /// The window is always exactly `visible` rows while there are rows to fill it, never
    /// reaches past the end of the list, and always contains the selection.
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

    /// Two components, because one collides across projects. See the doc comment.
    #[test]
    fn short_cwd_keeps_the_last_two_components() {
        assert_eq!(short_cwd("/home/you/Projects/misc/luneta"), "misc/luneta");
        assert_eq!(short_cwd("/home/you"), "home/you");
        assert_eq!(short_cwd("/home"), "home");
        assert_eq!(short_cwd("/"), "/");
        assert_eq!(short_cwd(""), "");
        // A trailing slash is not a component.
        assert_eq!(short_cwd("/misc/luneta/"), "misc/luneta");
    }

    /// The ladder degrades by dropping detail, never by dropping a key: "a key you cannot see
    /// is a feature you will never find."
    #[test]
    fn keys_text_drops_descriptions_before_it_drops_keys() {
        let keys: &[Key] = &[("<↓↑>", "Navigate", "Nav"), ("<ENTER>", "Select", "Select")];
        assert_eq!(keys_text(60, keys).content(), "<↓↑> - Navigate, <ENTER> - Select");
        assert_eq!(keys_text(30, keys).content(), "<↓↑> Navigate  <ENTER> Select");
        assert_eq!(keys_text(24, keys).content(), "<↓↑> Nav  <ENTER> Select");
        assert_eq!(keys_text(23, keys).content(), "<↓↑> Nav <ENTER> Select");
        // Past every tier, the keys alone survive.
        assert_eq!(keys_text(0, keys).content(), "<↓↑>/<ENTER>");
    }

    /// Every tier that claims to fit must actually fit.
    #[test]
    fn keys_text_respects_the_width_it_is_given() {
        let keys: &[Key] = &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Select", "Select"),
            ("<TAB>", "Agents", "Agents"),
            ("<ESC>", "Deselect/Close", "Close"),
        ];
        for width in 12..80 {
            let line = keys_text(width, keys);
            // The last-resort spelling is allowed to overflow — there is nothing shorter that
            // still names every key.
            if line.content().contains(' ') {
                assert!(line.content().width() <= width, "width {width}: {}", line.content());
            }
        }
    }

    /// Not an assertion — a way to look at a whole pane from `cargo test -- --nocapture`.
    #[test]
    #[ignore = "prints a pane; run with --ignored --nocapture to look at one"]
    fn print_a_pane() {
        let (rows, cols) = (16, 54);
        let screen = Screen::new(rows, cols);
        let state = matches(
            vec![
                session("luneta", Kind::Live, 2 * 3600),
                session("dotfiles", Kind::Live, 5 * 3600),
                session("notes", Kind::Live, 3 * 86400),
                session("despesas-old", Kind::Resurrectable, 12 * 86400),
                session("api-spike", Kind::Resurrectable, 40 * 86400),
            ],
            Some(1),
        );
        let notes = vec![Note::dim("you are in \"notes\" — not listed")];
        let rect = screen.results.unwrap();
        let body = search_body(&state, &rect, notes.len());
        let right = count(state.selected, state.rows.len());
        for line in picture(&rect, "Results", &right, interior(&rect, &notes, body)) {
            println!("{line}");
        }
        let input = &screen.input;
        println!("{}", input.top("Sessions", "").line);
        println!("{}", input_line(input, prompt_text(&state)).content());
        println!("{}", input.bottom());
        println!("  {}", search_help(help_width(cols)).content());
    }
}
