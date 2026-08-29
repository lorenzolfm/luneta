//! Drawing the picker.
//!
//! Everything here goes through zellij's own UI components — `Text`, `Table`, and the
//! `print_*_with_coordinates` family — rather than hand-written SGR. That is not cosmetic
//! preference: those components are serialized to the host as a DCS payload and coloured
//! *there*, from the active theme's `StyleDeclaration`. So the picker picks up the user's
//! theme (and its selected/emphasis colours) for free, with no `Styling` palette to carry and
//! no `ModeUpdate` subscription to keep it current.
//!
//! Colour is expressed as *emphasis levels*, not colours:
//!
//! | level | used for                                        |
//! |-------|-------------------------------------------------|
//! | 0     | tags — the quietest thing on the row            |
//! | 1     | session names, chosen layout                    |
//! | 2     | labels (`Session:`) and the age column          |
//! | 3     | the typed term, key caps, fuzzy-match hits      |
//!
//! Absolute coordinates are safe here — and are what upstream uses — because the host deletes
//! the plugin pane's viewport before feeding it each render (`plugin_pane.rs:243`). That also
//! retires the old "build one frame, print it with no trailing newline" dance: there is no
//! cursor to scroll off the top any more.

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

use crate::dirs::{Action, DirRow, DirSet, Status};
use crate::sessions::{format_age, Kind, MatchSet, Row};
use crate::{Pending, Rename};

/// Emphasis levels, named. See the table above.
const TAG: usize = 0;
const NAME: usize = 1;
const LABEL: usize = 2;
const ACCENT: usize = 3;

/// The widest the content block is allowed to get, matching upstream's single screen. Past
/// this, a full-width pane stretches three short columns across the whole terminal and the
/// eye has to travel for nothing.
///
/// It is a *truncation* budget, not a position. Centring is done per element, on what that
/// element actually renders to — see [`print_centered`]. Centring this cap instead is what
/// upstream does, and it only works there because four columns of session detail fill it;
/// three narrow columns do not, so it slides short text into the middle of nowhere.
const MAX_WIDTH: usize = 90;

/// How much had to be given up to fit the pane's width.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    /// name + [ATTACH] + age
    Full,
    /// name + [A] + age
    AbbrTag,
    /// name + [A]
    NoAge,
}

pub fn render_search(state: &MatchSet, error: Option<&str>, rows: usize, cols: usize) {
    let width = budget(cols);
    let y = if rows > 4 { 1 } else { 0 };

    print_centered(prompt_text(state), cols, y);

    // The bottom rows are spoken for before the list gets to ask: the help line, a blank row
    // above it, and a note line each. Taking them off the top of the budget is what keeps the
    // list from growing into them.
    let notes = note_texts(state, error);
    // Directly under the prompt, with no blank row of its own: the table's title row is blank
    // and supplies the gap. This is the built-in session manager's spacing exactly — an extra
    // blank here pushes the list one row further from the thing that filters it.
    let table_y = y + 1;
    let list_rows = rows.saturating_sub(notes.len() + 2).saturating_sub(table_y);

    if list_rows == 0 {
        // Nothing left to draw the list into; the prompt alone still answers "what am I typing?"
    } else if state.rows.is_empty() {
        // No table means no title row, so the gap it would have supplied is spent explicitly —
        // otherwise "no sessions" lands flush against the prompt.
        print_centered(empty_text(state), cols, table_y + 1);
    } else {
        render_results(state, cols, table_y, width, list_rows);
    }

    let notes_y = rows.saturating_sub(1).saturating_sub(notes.len());
    for (i, note) in notes.into_iter().enumerate() {
        print_centered(note, cols, notes_y + i);
    }
    print_centered(search_help(width), cols, rows.saturating_sub(1));
}

/// The directory screen: the places you go, and what `Enter` would make of each.
///
/// Deliberately the same shape as [`render_search`] — same prompt row, same table, same note
/// and help rows in the same places — so that `Tab` swaps the *contents* of a screen rather
/// than the screen. The columns differ because the third one answers a different question:
/// sessions show an age because age is what they are sorted by, and directories show their
/// path because frecency is not a thing you can print usefully.
pub fn render_dirs(dirs: &DirSet, term: &str, rows: usize, cols: usize) {
    let width = budget(cols);
    let y = if rows > 4 { 1 } else { 0 };

    print_centered(dir_prompt(dirs, term), cols, y);

    let notes = dir_note_texts(dirs);
    let table_y = y + 1;
    let list_rows = rows.saturating_sub(notes.len() + 2).saturating_sub(table_y);

    if list_rows == 0 {
        // Nothing left to draw the list into; the prompt alone still answers "what am I typing?"
    } else if dirs.rows.is_empty() {
        print_centered(dir_empty_text(dirs, term), cols, table_y + 1);
    } else {
        render_dir_results(dirs, cols, table_y, width, list_rows);
    }

    let notes_y = rows.saturating_sub(1).saturating_sub(notes.len());
    for (i, note) in notes.into_iter().enumerate() {
        print_centered(note, cols, notes_y + i);
    }
    print_centered(dirs_help(width), cols, rows.saturating_sub(1));
}

/// Renaming the session you are in — the only session `rename_session` can address.
pub fn render_rename(rename: &Rename, current: Option<&str>, rows: usize, cols: usize) {
    let width = budget(cols);
    let y = if rows > 4 { 1 } else { 0 };

    print_centered(
        input_prompt("Rename to:", &rename.input, rename.error.as_deref(), "Rename"),
        cols,
        y,
    );
    if let Some(current) = current {
        let note = truncate(&format!("renaming \"{}\" — the session you are in", current), width);
        print_centered(Text::new(note).dim_all(), cols, y + 2);
    }
    print_centered(
        keys_text(width, &[("<ENTER>", "Rename", "Rename"), ("<ESC>", "Cancel", "Cancel")]),
        cols,
        rows.saturating_sub(1),
    );
}

/// The confirm step before a session is killed or deleted. Nothing has happened at this point —
/// this screen is what makes that true, and `Esc` backs out of it.
pub fn render_confirm(pending: &Pending, rows: usize, cols: usize) {
    let width = budget(cols);
    let y = if rows > 4 { 1 } else { 0 };

    let label = format!("{} session:", pending.verb());
    let heading = format!(
        "{} {}",
        label,
        truncate(&pending.name, width.saturating_sub(label.width() + 1))
    );
    print_centered(
        Text::new(&heading)
            .color_range(LABEL, ..label.chars().count())
            .color_range(ACCENT, label.chars().count() + 1..),
        cols,
        y,
    );
    // Spelling out what survives is the whole point of the screen: "kill" and "delete" look
    // alike on a keyboard and differ entirely in what you can get back.
    let consequence = Text::new(truncate(pending.consequence(), width));
    let consequence = match pending.kind {
        Kind::Live => consequence.dim_all(),
        Kind::Resurrectable => consequence.error_color_all(),
    };
    print_centered(consequence, cols, y + 2);

    print_centered(
        keys_text(
            width,
            &[("<ENTER>", pending.verb(), pending.verb()), ("<ESC>", "Cancel", "Cancel")],
        ),
        cols,
        rows.saturating_sub(1),
    );
}

/// The directory prompt names the session that would be made, not the directory that would be
/// entered. That is the thing you have to be able to argue with — the path is already on the
/// row, and the name is the part the plugin invented.
fn dir_prompt(dirs: &DirSet, term: &str) -> Text {
    let Some(row) = dirs.selected_row() else {
        return input_line("Directory:", term, None, false);
    };
    let refused = row.action == Action::Here;
    let action = if refused {
        // The one row `Enter` will not act on. It belongs in the prompt rather than on a note
        // line because it is a property of the highlight, and it has to move with it.
        "already in this session".to_string()
    } else {
        format!("{} \"{}\"", row.action.verb(), row.name)
    };
    input_line("Directory:", term, Some(&action), refused)
}

/// The directory screen has three ways to be empty and they are not interchangeable. Only the
/// failure gets a note line — the other two are self-explanatory in the list's own place.
fn dir_note_texts(dirs: &DirSet) -> Vec<Text> {
    match &dirs.status {
        Status::Failed(reason) => vec![Text::new(reason).error_color_all()],
        _ => Vec::new(),
    }
}

fn dir_empty_text(dirs: &DirSet, term: &str) -> Text {
    match &dirs.status {
        Status::Waiting => Text::new("asking zoxide…").dim_all(),
        // The reason is already on the note line directly below; on a pane this small, saying
        // it twice costs more than the second copy is worth.
        Status::Failed(_) => Text::new("no directories").dim_all(),
        Status::Ready if term.is_empty() => Text::new("zoxide knows nowhere yet").dim_all(),
        Status::Ready => Text::new(format!("no match for \"{}\"", term)).dim_all(),
    }
}

/// The narrowest a path column is worth having. Below this it says nothing a `…` would not, so
/// the column is dropped and the name and tag get the room.
const MIN_PATH: usize = 12;

/// `[RESURRECT]` collapsed to `[R]`, and the width that costs.
const ABBR_TAG: usize = 3;

fn render_dir_results(dirs: &DirSet, cols: usize, y: usize, width: usize, list_rows: usize) {
    let capacity = list_rows.saturating_sub(1);
    if capacity == 0 {
        return;
    }
    let overflows = dirs.rows.len() > capacity;
    let visible = if overflows { capacity.saturating_sub(1) } else { capacity };
    if visible == 0 {
        return;
    }
    let (start, end) = viewport(dirs.selected.unwrap_or(0), dirs.rows.len(), visible);
    let window = &dirs.rows[start..end];

    // Measured over the visible window only, for [`render_results`]'s reason: widths taken from
    // rows you cannot see make the columns jump as you scroll.
    let full_tag = window.iter().map(|r| r.action.full_tag().width()).max().unwrap_or(0);
    // The name is capped at a third of the width before anything else is decided. Nothing else
    // here has a natural size — a path will happily eat a whole row — so the cap is what keeps
    // three columns on screen instead of two and a half.
    let name_budget = window
        .iter()
        .map(|r| r.name.width())
        .max()
        .unwrap_or(0)
        .min(width / 3)
        .max(4);
    // The tag abbreviates before the path is dropped: you can read `[C]` as easily as
    // `[CREATE]` once you have seen one of each, and a path is not guessable that way.
    let (tag_width, abbr) = if name_budget + GAP + full_tag + GAP + MIN_PATH <= width {
        (full_tag, false)
    } else {
        (ABBR_TAG, true)
    };
    // ⚠️ Three, not two gaps. The host charges `max_column_width + 1` for *every* column, the
    // last one included, and silently drops any column that pushes the running total past the
    // width it was given — see [`print_table_centered`]. Budgeting the path to the two visible
    // gaps makes the table an exact fit, which costs it the path column entirely.
    let remaining = width.saturating_sub(name_budget + tag_width + 3);
    let path_budget = (remaining >= MIN_PATH).then_some(remaining);

    let mut table = header_row(Table::new(), if path_budget.is_some() { 3 } else { 2 });
    let mut name_column = 1; // the blank title cell is one column wide
    let mut path_column = 0;
    for (offset, row) in window.iter().enumerate() {
        let cells =
            dir_result_row(row, dirs.selected == Some(start + offset), abbr, name_budget, path_budget);
        // Measured after truncation, which is the only width the host will ever see.
        name_column = name_column.max(cells[0].content().width());
        if let Some(path) = cells.get(2) {
            path_column = path_column.max(path.content().width());
        }
        table = table.add_styled_row(cells);
    }

    let mut widths = vec![name_column, tag_width];
    if path_budget.is_some() {
        widths.push(path_column);
    }
    print_table_centered(table, &widths, cols, y, list_rows);

    if overflows {
        let hidden = dirs.rows.len() - window.len();
        print_centered(
            Text::new(format!("+{} more", hidden)).dim_all(),
            cols,
            y + 1 + window.len(),
        );
    }
}

fn dir_result_row(
    row: &DirRow,
    selected: bool,
    abbr: bool,
    name_budget: usize,
    path_budget: Option<usize>,
) -> Vec<Text> {
    let mut cells = vec![
        // Not highlighted, because the term was never matched against it — the match ran on the
        // path, and painting hits onto a string they were not found in would be a lie that
        // happens to line up sometimes.
        Text::new(truncate(&row.name, name_budget)).color_range(NAME, ..),
        Text::new(if abbr { row.action.abbr_tag() } else { row.action.full_tag() })
            .color_range(TAG, ..),
    ];
    if let Some(path_budget) = path_budget {
        let (path, dropped) = truncate_left(&row.path, path_budget);
        // The indices are into the *untruncated* path. Those that fell off the left are gone;
        // the rest shift down by what was dropped and back up by one for the `…` standing in
        // for it.
        let shift = if dropped > 0 { 1 } else { 0 };
        let indices: Vec<usize> = row
            .indices
            .iter()
            .filter(|i| **i >= dropped)
            .map(|i| i - dropped + shift)
            .collect();
        cells.push(Text::new(&path).color_range(LABEL, ..).color_indices(ACCENT, indices));
    }
    if selected {
        cells = cells.into_iter().map(Text::selected).collect();
    }
    cells
}

/// How wide any one element may render before it is truncated.
fn budget(cols: usize) -> usize {
    cols.min(MAX_WIDTH)
}

/// The x that puts something `content` columns wide on the pane's centre line.
fn centre(cols: usize, content: usize) -> usize {
    cols.saturating_sub(content) / 2
}

/// Print one line centred on its own rendered width.
///
/// Per element, not per screen: a single centred block would have to be as wide as its widest
/// member — the help line — leaving the prompt and the list parked at that block's left edge,
/// which is the offset look this replaced rather than a fix for it.
///
/// The cost is that a line re-centres when its own width changes, so the prompt drifts by half
/// a column per character as you type. That is what centred input does everywhere, and it is
/// the prompt itself moving rather than the whole screen shifting under it.
fn print_centered(text: Text, cols: usize, y: usize) {
    let content = text.content().width();
    print_text_with_coordinates(text, centre(cols, content), y, Some(content), None);
}

/// The search term, with the `Enter` outcome spelled out beside it.
///
/// Putting the outcome *in* the prompt rather than on a line of its own is what lets the
/// picker say what `Enter` does in both states — pointing at the highlighted row, or at the
/// literal text — without the two sentences ever contradicting each other.
///
/// Trailing `_` is a cursor stand-in: a plugin's real cursor is off by default and turning it
/// on would mean tracking its position through every re-render.
fn prompt_text(state: &MatchSet) -> Text {
    let (action, is_error) = enter_action(state);
    input_line("Session:", &state.search_term, action.as_deref(), is_error)
}

/// The same prompt, for a screen whose only job is to take a name: the refusal (when there is
/// one) replaces the action, so one sentence covers both states here too.
fn input_prompt(label: &str, input: &str, error: Option<&str>, action: &str) -> Text {
    match error {
        Some(error) => input_line(label, input, Some(error), true),
        None => input_line(label, input, Some(action), false),
    }
}

/// `Label: typed_ <ENTER> - Outcome`, with the outcome in the error colour when it is a refusal.
///
/// Trailing `_` is a cursor stand-in: a plugin's real cursor is off by default and turning it
/// on would mean tracking its position through every re-render.
fn input_line(label: &str, input: &str, action: Option<&str>, is_error: bool) -> Text {
    let mut line = format!("{} ", label);
    let term_start = line.chars().count();
    line.push_str(input);
    line.push('_');
    let term_end = line.chars().count();

    let mut key = None;
    let mut act = None;
    if let Some(action) = action {
        line.push(' ');
        let key_start = line.chars().count();
        line.push_str("<ENTER>");
        let key_end = line.chars().count();
        line.push_str(" - ");
        let act_start = line.chars().count();
        line.push_str(action);
        key = Some(key_start..key_end);
        act = Some(act_start..line.chars().count());
    }

    let mut text = Text::new(&line)
        .color_range(LABEL, ..label.chars().count())
        .color_range(ACCENT, term_start..term_end);
    if let Some(range) = key {
        text = text.color_range(ACCENT, range);
    }
    if let Some(range) = act {
        text = if is_error {
            text.error_color_range(range)
        } else {
            text.color_range(NAME, range)
        };
    }
    text
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
fn note_texts(state: &MatchSet, error: Option<&str>) -> Vec<Text> {
    let mut notes = Vec::new();
    if let Some(error) = error {
        notes.push(Text::new(error).error_color_all());
    }
    if state.current_matches {
        if let Some(current) = state.current_session.as_ref() {
            notes.push(Text::new(format!("you are in \"{}\" — not listed", current)).dim_all());
        }
    }
    notes
}

fn empty_text(state: &MatchSet) -> Text {
    if state.search_term.is_empty() {
        Text::new("no sessions").dim_all()
    } else {
        Text::new(format!("no match for \"{}\"", state.search_term)).dim_all()
    }
}

fn render_results(state: &MatchSet, cols: usize, y: usize, width: usize, list_rows: usize) {
    // The table spends one row on its own title row, which is left blank: the host styles row
    // zero as a header whether or not you wanted one.
    let capacity = list_rows.saturating_sub(1);
    if capacity == 0 {
        return;
    }
    // And one more is kept back for "+N more" whenever the list overflows.
    let overflows = state.rows.len() > capacity;
    let visible = if overflows { capacity.saturating_sub(1) } else { capacity };
    if visible == 0 {
        return;
    }
    let (start, end) = viewport(state.selected.unwrap_or(0), state.rows.len(), visible);
    let window = &state.rows[start..end];

    // Widths are measured over the *visible* window only. Measuring the whole list would make
    // the name column jump as you scroll, for the sake of names you cannot see. The host pads
    // each column to its widest cell, so this only has to decide what gets *cut*.
    let name_width = window.iter().map(|r| r.name.width()).max().unwrap_or(0);
    let tag_width = window.iter().map(|r| r.kind.full_tag().width()).max().unwrap_or(0);
    let age_width = window.iter().map(|r| format_age(r.age).width()).max().unwrap_or(0);
    let fit = choose_fit(name_width, tag_width, age_width, width);
    let name_budget = name_column_budget(&fit, tag_width, age_width, width);

    let columns = if matches!(fit, Fit::NoAge) { 2 } else { 3 };
    let mut table = header_row(Table::new(), columns);
    let mut name_column = 1; // the blank title cell is one column wide
    for (offset, row) in window.iter().enumerate() {
        let cells = result_row(row, state.selected == Some(start + offset), &fit, name_budget);
        // Measured after truncation, which is the only width the host will ever see.
        name_column = name_column.max(cells[0].content().width());
        table = table.add_styled_row(cells);
    }

    let tag_column = if matches!(fit, Fit::Full) { tag_width } else { 3 };
    let mut widths = vec![name_column, tag_column];
    if !matches!(fit, Fit::NoAge) {
        widths.push(age_width);
    }
    print_table_centered(table, &widths, cols, y, list_rows);

    if overflows {
        let hidden = state.rows.len() - window.len();
        print_centered(
            Text::new(format!("+{} more", hidden)).dim_all(),
            cols,
            y + 1 + window.len(),
        );
    }
}

fn result_row(row: &Row, selected: bool, fit: &Fit, name_budget: usize) -> Vec<Text> {
    let name = truncate(&row.name, name_budget);
    // A truncated name drops the indices that fell off the end — colouring a position that no
    // longer exists would paint the wrong character.
    let visible_chars = name.chars().count();
    let indices: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible_chars).collect();

    let mut cells = vec![
        Text::new(&name).color_range(NAME, ..).color_indices(ACCENT, indices),
        Text::new(match fit {
            Fit::Full => row.kind.full_tag(),
            Fit::AbbrTag | Fit::NoAge => row.kind.abbr_tag(),
        })
        .color_range(TAG, ..),
    ];
    if !matches!(fit, Fit::NoAge) {
        cells.push(Text::new(format_age(row.age)).color_range(LABEL, ..));
    }
    if selected {
        cells = cells.into_iter().map(Text::selected).collect();
    }
    cells
}

/// A blank first row. `Table` styles row zero as a title row unconditionally, so a table that
/// does not want a header has to spend a row on an empty one — upstream does the same.
fn header_row(table: Table, columns: usize) -> Table {
    table.add_styled_row(vec![Text::new(" "); columns])
}

/// Print a table centred on the width it will actually occupy.
///
/// `columns` are the padded widths the host will settle on — it pads every cell in a column to
/// the widest one and puts a single space between columns, so the rendered width is their sum
/// plus one per gap.
///
/// The coordinate width passed on is the whole distance from `x` to the pane's right edge, not
/// the table's own width: the host charges `max_column_width + 1` for *every* column including
/// the last, and drops any column that does not fit that running total. Handing it an exact fit
/// would cost the table its last column.
fn print_table_centered(table: Table, columns: &[usize], cols: usize, y: usize, rows: usize) {
    let content = columns.iter().sum::<usize>() + columns.len().saturating_sub(1);
    let x = centre(cols, content);
    print_table_with_coordinates(table, x, y, Some(cols.saturating_sub(x)), Some(rows));
}

/// Keep the selection on screen, scrolling only when it would otherwise fall off. A centred
/// viewport would shift every row on every keystroke; row-index stability is a non-goal, but
/// gratuitous motion is still noise.
fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = if selected < visible {
        0
    } else {
        (selected + 1).saturating_sub(visible)
    };
    (start, (start + visible).min(total))
}

/// `Table` puts a single space between columns, so the gaps are one column each.
const GAP: usize = 1;

fn choose_fit(name_width: usize, tag_width: usize, age_width: usize, cols: usize) -> Fit {
    if name_width + GAP + tag_width + GAP + age_width <= cols {
        return Fit::Full;
    }
    if name_width + GAP + 3 + GAP + age_width <= cols {
        return Fit::AbbrTag;
    }
    Fit::NoAge
}

/// How many columns the name may use once the fixed columns have taken theirs. A name is
/// truncated rather than wrapped: a wrapped name would break the one-row-per-session invariant
/// the selection index depends on.
fn name_column_budget(fit: &Fit, tag_width: usize, age_width: usize, cols: usize) -> usize {
    let fixed = match fit {
        Fit::Full => GAP + tag_width + GAP + age_width,
        Fit::AbbrTag => GAP + 3 + GAP + age_width,
        Fit::NoAge => GAP + 3,
    };
    cols.saturating_sub(fixed).max(4)
}

/// The help line, in as much detail as the width allows. Below the compact form it is dropped
/// entirely — a truncated key list is worse than none.
fn search_help(width: usize) -> Text {
    keys_text(
        width,
        &[
            ("<↓↑>", "Navigate", "Nav"),
            ("<ENTER>", "Select", "Select"),
            ("<TAB>", "Directories", "Dirs"),
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

/// A key and the two lengths of its description, longest first.
type Key<'a> = (&'a str, &'a str, &'a str);

/// The help line in the most detail that fits, over four spellings: `<KEY> - Description, …`,
/// the same without the dashes, the same with short descriptions, and finally keys alone.
///
/// The default floating pane is 60% of the terminal, which puts the search screen's five keys
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

/// Truncate from the **left**, keeping the tail, and report how many characters went.
///
/// A path is identified by its last components; its first are `/home/you/` on every row of the
/// list. The count comes back because the caller is holding match positions into the original
/// string and has to shift the ones that survived.
fn truncate_left(text: &str, max: usize) -> (String, usize) {
    if text.width() <= max {
        return (text.to_string(), 0);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut width = 0;
    let mut kept = 0;
    for ch in chars.iter().rev() {
        let w = ch.to_string().width();
        // One column is held back for the `…` that replaces everything dropped.
        if width + w > max.saturating_sub(1) {
            break;
        }
        width += w;
        kept += 1;
    }
    let dropped = chars.len() - kept;
    let mut out = String::from("…");
    out.extend(chars[dropped..].iter());
    (out, dropped)
}

fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = ch.to_string().width();
        if width + w > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}
