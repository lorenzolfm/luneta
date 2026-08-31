//! Drawing the picker.
//!
//! This module draws through the `Text` component of zellij and the
//! `print_text_with_coordinates` family, and not through its own SGR sequences. A `Text` goes to
//! the host as a DCS payload, and the host colours it from the `StyleDeclaration` of the active
//! theme. The picker thus follows the theme of the user, with no palette to carry and no
//! `ModeUpdate` subscription to keep current.
//!
//! There is one exception: the screen rows of the preview box. Those bytes belong to another
//! pane, and their colours are that pane's colours. A `Text` cannot carry them, because it has
//! four emphasis levels and the host decides how each one looks. Those rows are printed as the
//! escape sequences they arrived as. See [`PreviewRow`].
//!
//! Colour is expressed as emphasis levels, not as colours:
//!
//! | level | used for                                   |
//! |-------|--------------------------------------------|
//! | 0     | tags, which are the quietest part of a row  |
//! | 1     | session names                              |
//! | 2     | labels, box titles, the age column         |
//! | 3     | the typed term, key names, match hits      |
//!
//! Weight has one user. The host draws every character of a `Text` bold unless index level 5
//! says otherwise, so bold was the default and said nothing. [`Line::finish`] removes it from
//! the content of every row, which leaves the box titles as the only bold text. See
//! [`border_text`].
//!
//! Absolute coordinates are safe, because the host clears the viewport of the plugin pane
//! before each render (`plugin_pane.rs:243`). No cursor can scroll a line off the top.
//!
//! ## One `Text` per row
//!
//! Rows were once `Table` cells, which the host measured, padded and joined. They are now whole
//! lines, measured here, because a `Table` cannot express a bordered row. Three results follow:
//!
//! - The selected row is one `Text::selected()` across the full width, so the highlight is one
//!   band and not a set of cells with gaps between them.
//! - A last column can sit against the right border, which a `Table` cannot do.
//! - The empty cell problem is gone. The text of a `Table` cell crossed the wire as a
//!   comma-separated list of its bytes, so `""` arrived as a list of no bytes and not as text
//!   of length zero. The host dropped the cell, and because the wire format is one run of cells
//!   divided into rows by a column count, every later cell moved one place left. A row that
//!   dropped a cell took the first cell of the row below it.
//!
//! The cost is the arithmetic the host used to do, and three `print_*` calls for each visible
//! row instead of one for each table: the interior of a row, with a border printed on each side,
//! so that the highlight of a selected row cannot cover them (see [`draw_row`]). A render runs
//! about once a second (`polled || self.spinning()` in `main.rs`), which is about 90 calls a
//! second on a full pane.

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

use crate::agents::{self, AgentRow, AgentSet};
use crate::dirs::{Action, DirRow, DirSet, Listing};
use crate::fetch::Fetch;
use crate::layout::{anchor, truncate, truncate_left, Border, Line, Rect, Screen, PAD, VERTICAL};
use crate::panes::{self, Peek, Peeks};
use crate::sessions::{format_age, Contents, Kind, MatchSet, Row};
use crate::Rename;

/// Emphasis levels, named. See the table above.
const TAG: usize = 0;
const NAME: usize = 1;
const LABEL: usize = 2;
const ACCENT: usize = 3;

/// The name of the results box, on every screen that has one.
///
/// This is the name of the picker, not the name of the screen. The title of the input box below
/// it already changes with `Tab`, and the top left corner of a floating pane says what the
/// window is. It is also the pane name (`rename_plugin_pane` in `main.rs`).
const TITLE: &str = "luneta";

/// The selection gutter: one column for the caret, and one for the space after it.
///
/// Every row has one, selected or not. A gutter that appeared only under the highlight would
/// move the whole list two columns sideways as you move down it.
const CARET: usize = 2;

/// Open a row with its selection gutter.
///
/// The row has both the caret and `Text::selected`. The band alone is hard to see on a theme
/// whose selected background is close to its normal one, and the caret alone is one character
/// to find on a wide row.
fn gutter(line: &mut Line, selected: bool) {
    line.push(if selected { ">" } else { " " }, ACCENT);
    line.gap(1);
}

/// The blank columns between two columns of a row. Two, not the one the host used, because a
/// full-width box has the space and three columns that touch read as one.
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

/// The session screen.
///
/// The results box is titled [`TITLE`] and not "Results". That corner names the window, and the
/// input box below already names the list.
pub fn render_search(
    state: &MatchSet,
    peeks: &Peeks,
    error: Option<&str>,
    rows: usize,
    cols: usize,
) {
    let screen = Screen::new(rows, cols);
    let notes = note_texts(state, error);

    if let Some(rect) = &screen.results {
        let body = search_body(state, rect, notes.len());
        draw(rect, TITLE, &count(state.selected, state.rows.len()), interior(rect, &notes, body));
    }
    if let Some(rect) = &screen.preview {
        let (title, right, lines) = session_preview(state, peeks, rect);
        draw_preview(rect, &title, &right, lines);
    }
    draw_input(&screen, "Sessions", prompt_text(state));
    draw_help(&screen, search_help(help_width(cols)));
}

/// The directory screen: the places you go, and what `Enter` does with each one.
///
/// This has the shape of [`render_search`]: the same two boxes, the same help row, and the same
/// list at the bottom. `Tab` thus changes the contents of a screen and not the screen. The
/// columns differ, because sessions sort by age and directories sort by frecency, which is not
/// a useful thing to print.
pub fn render_dirs(dirs: &DirSet, term: &str, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);
    let notes = dir_note_texts(dirs);

    if let Some(rect) = &screen.results {
        let body = dir_body(dirs, term, rect, notes.len());
        draw(rect, TITLE, &count(dirs.selected, dirs.rows.len()), interior(rect, &notes, body));
    }
    if let Some(rect) = &screen.preview {
        let (title, right, lines) = dir_preview(dirs, rect);
        draw_preview(rect, &title, &right, lines);
    }
    draw_input(&screen, "Directories", dir_prompt(dirs, term));
    draw_help(&screen, dirs_help(help_width(cols)));
}

/// The agent screen: which agents run, what they do, and for how long.
///
/// `frame` is the animation tick, and it reaches one thing: the glyph of the busy spinner. It
/// is passed down and not read from `agents`, because it describes the moment of the draw and
/// not the agents. Their snapshot does not change.
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
        draw(rect, TITLE, &count(agents.selected, agents.rows.len()), interior(rect, &notes, body));
    }
    if let Some(rect) = &screen.preview {
        let (title, lines) = agent_preview(agents, peeks, rect);
        draw_preview(rect, &title, "", lines);
    }
    draw_input(&screen, "Agents", agent_prompt(agents, term));
    draw_help(&screen, agents_help(help_width(cols)));
}

/// Rename the current session, which is the only session `rename_session` can address.
pub fn render_rename(rename: &Rename, current: Option<&str>, rows: usize, cols: usize) {
    let screen = Screen::new(rows, cols);

    // The full width, not the half the list would take. This screen asks one question and has
    // nothing to preview.
    if let Some(rect) = &screen.full {
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

// ---------------------------------------------------------------------------------------------
// The chrome
// ---------------------------------------------------------------------------------------------

/// What the input box holds: the text you type, what `Enter` does with it, and whether that is
/// a refusal.
type Prompt = (String, Option<String>, bool);

/// The help row is indented to align with the content inside the boxes above it.
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
        draw_row(rect, rect.inner_y() + i, line);
    }
    print_at(Text::new(rect.bottom()).dim_all(), rect.x, rect.bottom_y(), rect.width);
}

/// One interior row: the left border, the row, and the right border. These are three `Text`
/// values, not one.
///
/// The division keeps the selection band inside the box. `Text::selected()` applies to a whole
/// `Text`, and the host paints the selected background over all of it. A row that held its own
/// `│` characters thus lost both sides of the box under the highlight. The borders are now
/// their own `Text` values, and neither is ever the selected one.
///
/// The cost is three `print_*` calls for each row instead of one, which is about 90 calls a
/// second on a full pane.
fn draw_row(rect: &Rect, y: usize, row: Text) {
    let Some(inner) = rect.width.checked_sub(2) else {
        // A box of two columns is two borders with nothing between them.
        return;
    };
    let edge = || Text::new(VERTICAL.to_string()).dim_all();
    print_at(edge(), rect.x, y, 1);
    print_at(row, rect.x + 1, y, inner);
    print_at(edge(), rect.x + rect.width - 1, y, 1);
}

/// The only text on the screen that stays bold.
///
/// [`Line::finish`] removes bold from every character of every row, so a box title is bold
/// because nothing removed it. [`TITLE`] and the screen name thus read as headings. The count
/// on the right is not bold: it is a readout, not a heading.
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

/// The interior of a box: blank rows, then the notes, then the body. The block sits at the
/// bottom, so that the list is next to the prompt and the notes are above the list.
fn interior(rect: &Rect, notes: &[Note], body: Vec<Text>) -> Vec<Text> {
    let block = anchor(rect, notes.len(), body.len());
    let mut lines: Vec<Text> = (rect.inner_y()..block.y).map(|_| blank_line(rect)).collect();
    lines.extend(notes.iter().take(block.notes).map(|note| note_line(rect, note)));
    lines.extend(body.into_iter().take(block.rows));
    lines
}

/// A note is truncated to the box. A note that is wider would write over the right border. A
/// long failure reason from `RunCommandResult`, which carries an absolute path, once passed the
/// edge of the pane and the help line then wrote over it.
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

/// `> typed_` on the left, and what `Enter` does on the right.
///
/// The two are separated, because they answer different questions and change at different
/// times. The left half changes as you type, and the right half changes with the highlight.
///
/// If both do not fit, the action is cut first, and it goes below [`MIN_ACTION`]. The term is
/// never cut. If the term alone is too wide, it is truncated from the left, because the
/// characters you typed last are at its end.
fn input_line(rect: &Rect, prompt: Prompt) -> Text {
    let (input, action, is_error) = prompt;
    let inner = rect.inner_width();
    let mut line = Line::new();
    line.push("> ", TAG);

    // The final `_` stands for the cursor. The real cursor of a plugin is off by default, and
    // to turn it on would mean to track its position through every render.
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

/// The narrowest action worth printing: `<ENTER> ` and one word after it.
const MIN_ACTION: usize = 12;

fn draw_input(screen: &Screen, title: &str, prompt: Prompt) {
    let rect = &screen.input;
    if !screen.bordered {
        // No room for a border. The one row that is left shows what you type, which nothing
        // else on this screen can show.
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

/// `3/47`: the position of the cursor, over the number of rows that matched.
///
/// This answers two questions in one place: how many rows the search left, and how far down the
/// list you are. `1/47` says that 46 rows are below, and costs no row of its own. With no
/// selection there is no position, so only the count shows.
fn count(selected: Option<usize>, total: usize) -> String {
    match selected {
        Some(index) if total > 0 => format!("{}/{}", index + 1, total),
        _ if total > 0 => total.to_string(),
        _ => String::new(),
    }
}

/// Keep the selection on the screen, and scroll only when it would leave. A centred viewport
/// would move every row on every keystroke.
fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = if selected < visible { 0 } else { (selected + 1).saturating_sub(visible) };
    (start, (start + visible).min(total))
}

// ---------------------------------------------------------------------------------------------
// The preview box
// ---------------------------------------------------------------------------------------------

/// An empty interior row.
fn blank_line(rect: &Rect) -> Text {
    Text::new(rect.blank()).dim_all()
}

/// One row of a preview box: a row the picker wrote, or a row a pane wrote.
///
/// The two cannot be one type, because of what a `Text` is. A `Text` is a string and a set of
/// emphasis levels, and the host turns the levels into colours from the theme. That is correct
/// for what the picker says about a pane, and useless for what is on one. A pane line carries
/// its own colours as `SGR`, in truecolour: `nvim` syntax, the red and green of a diff, the
/// branch in a prompt. No level can hold those.
///
/// A pane row is therefore printed as the bytes it arrived as. That is safe for two reasons.
/// [`crate::panes::sgr_only`] has removed every escape that is not a colour, so the bytes cannot
/// move the cursor or clear the screen. The host also places its own `Text` components with the
/// same `ESC [ y ; x H` this code uses (`ui/components/component_coordinates.rs:19`).
enum PreviewRow {
    /// Ours: a styled row, coloured by the theme.
    Own(Text),
    /// Theirs: one interior width of a pane line, with its colours. [`pane_row`] has already
    /// cut and padded it to the box.
    Pane(String),
}

impl From<Text> for PreviewRow {
    fn from(text: Text) -> Self {
        PreviewRow::Own(text)
    }
}

impl PreviewRow {
    /// The row as characters, which is how the box looks. A pane row keeps its escapes, which
    /// are part of the row and take no columns. Only the tests read a row back.
    #[cfg(test)]
    fn content(&self) -> &str {
        match self {
            PreviewRow::Own(text) => text.content(),
            PreviewRow::Pane(line) => line,
        }
    }
}

/// A pane line, laid out as [`Line::finish`] lays out our own rows: one column of padding on
/// each side, cut to the interior and padded back to it. A pane row and a picker row are thus
/// the same width, and the right border is in the same place on both.
fn pane_row(inner: usize, line: &str) -> String {
    let line = panes::fit(line, inner);
    let pad = inner.saturating_sub(panes::columns(&line));
    format!(" {}{} ", line, " ".repeat(pad))
}

/// Draw the preview box: the borders of any other box, with the content at the top.
///
/// The box answers a question that no row has the width to answer. The name of a session does
/// not say what runs in it, the name of a directory does not say what is in it, and the label of
/// an agent does not say what it waits for. Each screen answers from a different source: the
/// session preview from the snapshot that gives the ages, the directory preview from eza,
/// and the agent preview from the row. The three functions below share these two helpers only.
///
/// This is [`draw`] with one more case in the middle: a pane row does not go through
/// `print_at`.
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

/// One row of a pane screen: the borders of the box on each side, and the bytes of the pane
/// between them.
///
/// Both resets are necessary. The first one clears the colour that the last `Text` left, because
/// the first characters of the pane can set no colour of their own. The second one clears a
/// colour that the pane line did not close, which would otherwise reach the right border.
fn draw_pane_row(rect: &Rect, y: usize, line: &str) {
    let Some(inner) = rect.width.checked_sub(2) else {
        return;
    };
    let edge = || Text::new(VERTICAL.to_string()).dim_all();
    print_at(edge(), rect.x, y, 1);
    print!("\u{1b}[{};{}H\u{1b}[m{}\u{1b}[m", y + 1, rect.x + 2, panes::fit(line, inner));
    print_at(edge(), rect.x + rect.width - 1, y, 1);
}

/// The interior of a preview box: content at the top, and blank rows below it.
///
/// This is anchored to the top, where [`interior`] is anchored to the bottom, because the two
/// are read from opposite ends. The list grows up from the prompt you type into. A preview is
/// read from its first line down, and a short one at the bottom would be far from the title that
/// names it.
///
/// Content that is too tall loses its end, and the last row says how much. The preview cannot
/// scroll, because the cursor is in the list beside it and every key that could move it has a
/// meaning there.
fn filled(rect: &Rect, mut lines: Vec<PreviewRow>) -> Vec<PreviewRow> {
    let height = rect.inner_height();
    if lines.len() > height {
        // One more than the overflow, because the marker takes the row of the last line that
        // would have fitted.
        let hidden = lines.len() - height + 1;
        lines.truncate(height.saturating_sub(1));
        lines.push(note_line(rect, &Note::dim(format!("… {} more", hidden))).into());
    }
    lines.resize_with(height, || blank_line(rect).into());
    lines
}

/// One line of a preview box: text at `level`, cut to the box. These are the words of the
/// picker, so the result is a [`PreviewRow::Own`].
fn preview_line(inner: usize, text: &str, level: usize) -> PreviewRow {
    let mut line = Line::new();
    line.push(&truncate(text, inner), level);
    line.finish(inner).into()
}

/// The same, wrapped over as many lines as it takes.
fn wrapped_lines(inner: usize, text: &str, level: usize) -> Vec<PreviewRow> {
    wrap(text, inner).iter().map(|line| preview_line(inner, line, level)).collect()
}

/// The same again, in the error colour.
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

/// Divide `text` into lines of `width` columns or fewer, at spaces.
///
/// This is the only place in the picker that wraps text. Everywhere else a string is one column
/// of a row, so a cut end costs a name and the row still reads. Here the box has the room for a
/// paragraph, and a sentence that explains why a session shows nothing has nowhere else to go.
///
/// [`truncate`] still cuts a word that is wider than the box. A break inside a word reads as two
/// words, which is worse than a mark that says the word was cut.
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

/// What a preview box says when the list beside it has no highlight.
fn nothing_highlighted(rect: &Rect) -> (String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    (
        // Not [`TITLE`], because the box beside this one has that name. A preview is named
        // after what it shows, and it shows nothing.
        "Preview".to_string(),
        vec![
            preview_line(inner, "nothing highlighted", TAG),
            blank_line(rect).into(),
            preview_line(inner, "Enter takes what you type", TAG),
        ],
    )
}

// ---------------------------------------------------------------------------------------------
// Preview: sessions
// ---------------------------------------------------------------------------------------------

/// What is on the screen of the highlighted session, or why a dead session has no screen.
///
/// The title is the session. The right label of the border is the pane count. The body is one
/// of its panes. [`crate::sessions::Focus`] says which pane and why.
fn session_preview(
    state: &MatchSet,
    peeks: &Peeks,
    rect: &Rect,
) -> (String, String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    let Some(row) = state.selected.and_then(|i| state.rows.get(i)) else {
        let (title, lines) = nothing_highlighted(rect);
        return (title, String::new(), lines);
    };
    let contents = state.contents.get(&row.name);
    // Only for a live session, and only after it reports its panes. A count in the border of
    // a box that explains that there is nothing to count says nothing.
    let right = match (row.kind, contents) {
        (Kind::Live, Some(contents)) => plural(contents.panes, "pane"),
        _ => String::new(),
    };
    let lines = match (row.kind, contents) {
        (Kind::Live, Some(contents)) => live_preview(rect, peeks, &row.name, contents),
        // A live session whose server has not yet written its metadata. The next poll finds
        // it, so the box says so and does not show a blank.
        (Kind::Live, None) => {
            wrapped_lines(inner, "no detail yet — the session has not reported what is in it", TAG)
        },
        (Kind::Resurrectable, _) => dead_preview(rect),
    };
    (row.name.clone(), right, lines)
}

/// A live session: which pane you are looking at, then what is on it.
fn live_preview(rect: &Rect, peeks: &Peeks, name: &str, contents: &Contents) -> Vec<PreviewRow> {
    let inner = rect.inner_width();
    let Some(focus) = &contents.focus else {
        // Every pane is a plugin pane, and a plugin pane dumps an empty screen. The box gives
        // the reason and is not blank.
        return wrapped_lines(inner, "nothing but plugin panes — no screen to show", TAG);
    };
    let mut lines = vec![caption(inner, &focus.tab, &focus.title).into(), blank_line(rect).into()];
    lines.extend(screen_lines(rect, peeks, &panes::key(name, focus.pane), lines.len()));
    lines
}

/// `editor · nvim`: the tab, then the title of the pane.
///
/// The title of the box is the session, so this line completes the address and names the pane.
/// Without it, a session of seven panes shows one of them and does not say which.
fn caption(inner: usize, tab: &str, title: &str) -> Text {
    let mut line = Line::new();
    // The tab name is cut before the pane title. Two panes of one tab differ in their titles,
    // and you look at a pane.
    let room = inner.saturating_sub(title.width() + 3).max(MIN_TAB);
    line.push(&truncate(tab, room), LABEL);
    line.push(" · ", TAG);
    line.push(&truncate(title, inner.saturating_sub(line.columns())), TAG);
    line.finish(inner)
}

/// The narrowest tab name worth printing. The pane title takes the rest.
const MIN_TAB: usize = 6;

/// The screen of the pane, or why there is none yet.
///
/// The box shows the end of the screen, cut to the space that is left, at the bottom of the
/// box. You read a terminal from the bottom, so a preview of the top of a pane would show where
/// the session was and not where it is. A short screen in the middle of the box would also put
/// the prompt in a different place for each row. `used` is the space the lines above have taken.
///
/// The three messages that replace a screen are at the top, because the box says them about
/// itself and not about a terminal.
fn screen_lines(rect: &Rect, peeks: &Peeks, key: &str, used: usize) -> Vec<PreviewRow> {
    let inner = rect.inner_width();
    let rows = rect.inner_height().saturating_sub(used);
    match peeks.get(key) {
        // To the reader, "not asked yet" and "asked but not answered" are the same: the
        // answer is on its way. [`crate::PREVIEW_DELAY`] is the time between them.
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

/// A dead session has no process, so it has no screen and no panes to count. `0 panes` would
/// say that it has no panes, and not that nothing runs to hold any.
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

// ---------------------------------------------------------------------------------------------
// Preview: directories
// ---------------------------------------------------------------------------------------------

/// What is in the highlighted directory, as eza draws it. The title of the box is the session
/// name the row would create, and the count in the border is the number of entries.
///
/// The listing is found by path, never by row index. The cursor moves faster than eza answers,
/// and a reply filed under the wrong directory would show the contents of another place. See
/// [`crate::dirs::PATH_KEY`].
fn dir_preview(dirs: &DirSet, rect: &Rect) -> (String, String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    let Some(row) = dirs.selected_row() else {
        let (title, lines) = nothing_highlighted(rect);
        return (title, String::new(), lines);
    };
    // Cut from the left, as on the row. The end of a path identifies a directory.
    let (path, _) = truncate_left(&row.path, inner);
    let mut lines = vec![preview_line(inner, &path, LABEL), blank_line(rect).into()];
    let mut right = String::new();
    match dirs.listing(&row.path) {
        // To the reader, "not asked yet" and "asked but not answered" are the same: the
        // answer is on its way. [`crate::PREVIEW_DELAY`] is the time between them.
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

/// One entry of a listing, in the colours and the icon eza gave it.
///
/// This is a [`PreviewRow::Pane`] and not a `Text`, for the reason a pane row is: the colours
/// belong to the program that wrote them, and a `Text` has only the emphasis levels of the
/// theme. eza already separates a directory from a file by colour, by icon and by the `/` that
/// `--classify` adds, so nothing here has to decide that again.
fn entry_line(inner: usize, entry: &str) -> PreviewRow {
    PreviewRow::Pane(pane_row(inner, entry))
}

// ---------------------------------------------------------------------------------------------
// Preview: agents
// ---------------------------------------------------------------------------------------------

/// What the highlighted agent does, and what is on its screen.
///
/// The status and its age are on the first line, because they decide where you go: an agent
/// that has waited eleven minutes is the row to choose. Below them is the pane of the agent,
/// which says what it waits for.
fn agent_preview(agents: &AgentSet, peeks: &Peeks, rect: &Rect) -> (String, Vec<PreviewRow>) {
    let inner = rect.inner_width();
    let Some(row) = agents.selected_row() else {
        return nothing_highlighted(rect);
    };
    // The status that takes the accent colour in the list takes it here too.
    let level = if agents::is_waiting(row.status.as_ref()) { ACCENT } else { LABEL };
    let mut line = Line::new();
    line.push(&truncate(agents::word(row.status.as_ref()), inner), level);
    line.push(" · ", TAG);
    line.push(&agents::format_duration(row.age), TAG);
    let mut lines = vec![line.finish(inner).into(), blank_line(rect).into()];
    lines.extend(screen_lines(rect, peeks, &panes::key(&row.session, row.pane), lines.len()));
    (row.label(), lines)
}

// ---------------------------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------------------------

/// The narrowest path column worth having. Below this it says no more than a `…`, so the
/// column goes and the name and the tag take the space.
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

    // The separator takes a row, so the window counts display lines and not rows. Each index
    // below is one or the other: `row` indexes `state.rows` and holds the selection, and `line`
    // indexes what is drawn.
    let dead_at = dead_from(&state.rows);
    let line_of = |row: usize| row + usize::from(dead_at.is_some_and(|at| row >= at));
    let lines = state.rows.len() + usize::from(dead_at.is_some());
    let selected_line = state.selected.map(line_of).unwrap_or(0);
    let (start, end) = viewport(selected_line, lines, capacity);

    // Widths are measured over the visible window. The whole list would move the name column
    // as you scroll, for names you cannot see.
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
    // Two columns, so the name takes what the gutter and the age do not need. There is nothing
    // left to remove on a narrow pane.
    let name_budget = inner.saturating_sub(CARET + GAP + age_width).max(4);

    (start..end)
        .map(|line| match visible(line) {
            None => separator(inner, "🪦 Dead sessions"),
            Some(i) => {
                let selected = state.selected == Some(i);
                result_line(&state.rows[i], selected, name_budget, inner)
            },
        })
        .collect()
}

/// Where the dead sessions start, if there are any.
///
/// This is one position, not a test for each row. `MatchSet` sorts live sessions before
/// resurrectable ones at every stage (rule 2 of its module doc), so the list is always two
/// groups. If that changes, this returns a boundary that puts live rows under the separator,
/// which is why the sort keeps `kind_rank` above all else.
fn dead_from(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|row| row.kind == Kind::Resurrectable)
}

/// `🪦 Dead sessions ──────────────────`
///
/// This is a border, not a `Row`. It never enters `MatchSet.rows`, so it cannot be selected,
/// `↑` and `↓` pass over it, and `selected_name()` cannot return it. It scrolls with the list. A
/// fixed position would make `viewport` reserve a row that depends on the selection, and the
/// input line already says `Resurrect` for the highlighted row.
///
/// The rule runs to the right edge and is not centred, which also corrects an error of width:
/// `🪦` is an East Asian Wide character, and many terminal fonts draw it in one cell.
fn separator(inner: usize, label: &str) -> Text {
    let mut line = Line::new();
    // Indented past the selection gutter, so that it starts where the names start.
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
    // A truncated name drops the indexes past its end. A colour on a position that is gone
    // would paint another character.
    let visible = name.chars().count();
    let hits: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible).collect();

    let mut line = Line::new();
    gutter(&mut line, selected);
    line.push_hits(&name, NAME, ACCENT, &hits);
    // The age sits against the right border, and not after the longest name. Two columns in a
    // box as wide as the pane would otherwise stay on the left with half the box empty, and the
    // age column would move whenever the longest visible name changed. The text is aligned
    // right, so that `ago` is in the same column on every row.
    let age = format_age(row.age);
    line.pad_to(inner.saturating_sub(age.width()));
    line.push(&age, LABEL);

    let text = line.finish(inner);
    if selected {
        text.selected()
    } else {
        text
    }
}

/// The search term, with the result of `Enter` beside it.
///
/// The result is in the input box and not on its own line, so that one sentence says what
/// `Enter` does in both states: on the highlighted row, or on the text you typed.
fn prompt_text(state: &MatchSet) -> Prompt {
    let (action, is_error) = enter_action(state);
    (state.search_term.clone(), action, is_error)
}

/// What `Enter` does, and whether that is a refusal.
///
/// The two refusals are the states in which `confirm_search` does nothing. They show here, in
/// the error colour, as you type. An error overlay would take the next keystroke, and you reach
/// this state by typing.
fn enter_action(state: &MatchSet) -> (Option<String>, bool) {
    if let Some(index) = state.selected {
        return match state.rows.get(index).map(|r| r.kind) {
            Some(Kind::Live) => (Some("Attach".to_string()), false),
            Some(Kind::Resurrectable) => (Some("Resurrect".to_string()), false),
            None => (None, false),
        };
    }
    if state.is_own_name() {
        // This does nothing. It is not an error, and not an offer to create a name that the
        // current session already has.
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

/// The note that says where the current session went. From inside `despesas`, the term `desp`
/// gives an empty list and no explanation. The note shows only when the term matches that
/// session. With an empty term you can see the list and you know where you are.
///
/// The note is not a row. It cannot be selected and has no index, so it can name the current
/// session without a return of that session to the match set.
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

    // The name has a limit of one third of the width, set before all else. A path has no
    // natural size and would take a whole row, so the limit keeps two columns on the screen.
    let name_column =
        window.iter().map(|r| r.name.width()).max().unwrap_or(0).min(inner / 3).max(4);
    let path_budget = inner.saturating_sub(CARET + name_column + GAP);

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
    // The only row `Enter` does not act on. A dim row is the usual form for "no action", it
    // costs no columns, and no other row on any screen refuses `Enter`. The input line also
    // says `already in this session` when the highlight is here.
    let refused = row.action == Action::Here;
    let level = if refused { TAG } else { NAME };

    let mut line = Line::new();
    gutter(&mut line, selected);
    // Not highlighted, because the term never matched this string. The match ran on the path,
    // and hits painted on another string would be wrong.
    line.push(&truncate(&row.name, name_column), level);
    line.pad_to(CARET + name_column);
    line.gap(GAP);

    let (path, dropped) = truncate_left(&row.path, path_budget);
    // Aligned right. A path is already cut from the left, so an end at the border puts the `…`
    // in an uneven column and the part that identifies the directory in a straight one.
    line.pad_to(inner.saturating_sub(path.width()));
    if refused {
        line.push(&path, TAG);
    } else {
        // The indexes point into the full path. Those before the cut are gone, and the rest
        // move down by the number removed and up by one for the `…`.
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

/// The directory prompt names the session that would be created, not the directory. The path is
/// already on the row, and the name is the part the plugin derived.
fn dir_prompt(dirs: &DirSet, term: &str) -> Prompt {
    let Some(row) = dirs.selected_row() else {
        return (term.to_string(), None, false);
    };
    let refused = row.action == Action::Here;
    let action = if refused {
        // The only row `Enter` does not act on. This is in the prompt and not on a note line,
        // because it belongs to the highlight and must move with it.
        "already in this session".to_string()
    } else {
        format!("{} \"{}\"", row.action.verb(), row.name)
    };
    (term.to_string(), Some(action), refused)
}

/// The directory screen has three ways to be empty. Only the failure needs a note line, because
/// the other two explain themselves in the place of the list.
fn dir_note_texts(dirs: &DirSet) -> Vec<Note> {
    match &dirs.status {
        Fetch::Failed(reason) => vec![Note::error(reason)],
        _ => Vec::new(),
    }
}

fn dir_empty_text(dirs: &DirSet, term: &str) -> String {
    match &dirs.status {
        Fetch::Waiting => "asking zoxide…".to_string(),
        // The reason is on the note line above. A pane this small has no space to say it
        // twice.
        Fetch::Failed(_) => "no directories".to_string(),
        Fetch::Ready if term.is_empty() => "zoxide knows nowhere yet".to_string(),
        Fetch::Ready => format!("no match for \"{}\"", term),
    }
}

// ---------------------------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------------------------

/// How much an agent row must give up to fit the width of the box.
///
/// The order of removal is: shorten the tag, remove the cwd, remove the age. The age stays
/// longer than the cwd, because the label usually names the project that the cwd would repeat,
/// and nothing else on the row says how long an agent has waited.
///
/// A level above `Full` once carried a token count. It went when `claude-ps` stopped sending
/// `context`, which it derived from a cwd that could not always be reversed and which was its
/// only read of unbounded size.
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

    // Measured at the `frame` that builds the cells below, so that a width and the glyph it
    // must hold come from one turn of the spinner. Every spinner frame is one column wide (see
    // `agents::SPINNER`), and the frame is passed so that this code does not assume that.
    let full_tag = window
        .iter()
        .map(|r| agents::full_tag(r.status.as_ref(), frame).width())
        .max()
        .unwrap_or(0);
    // Measured, not assumed. A glyph takes two columns and the `[S]` form of an unknown status
    // takes three, so the narrow tag column has no fixed width.
    let abbr_width = window
        .iter()
        .map(|r| agents::abbr_tag(r.status.as_ref(), frame).width())
        .max()
        .unwrap_or(0);
    let age_width =
        window.iter().map(|r| agents::format_duration(r.age).width()).max().unwrap_or(0);
    // A limit of one third of the width, set before all else. Without it the name takes the
    // space of the other columns.
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
            let selected = agents.selected == Some(start + offset);
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
    // The term matched the bare label, so a `:pane` suffix holds no hit. A truncated label
    // also drops the indexes past its end, because a colour on a position that is gone would
    // paint another character.
    let visible = label.chars().count();
    let hits: Vec<usize> = row.indices.iter().copied().filter(|i| *i < visible).collect();

    let mut line = Line::new();
    gutter(&mut line, selected);
    line.push_hits(&label, NAME, ACCENT, &hits);
    line.pad_to(CARET + name_column);
    line.gap(GAP);
    // The only status in the accent colour. Every other status, including one released after
    // this code, shows as itself.
    let tag_level = if agents::is_waiting(row.status.as_ref()) { ACCENT } else { TAG };
    let tag = if matches!(fit, AgentFit::Full) {
        agents::full_tag(row.status.as_ref(), frame)
    } else {
        agents::abbr_tag(row.status.as_ref(), frame)
    };
    line.push(&tag, tag_level);

    // The last column that remains is aligned right, as on the other two screens. The columns
    // before it keep the widths they were measured to.
    let age = agents::format_duration(row.age);
    match cwd_budget {
        Some(cwd_budget) => {
            line.pad_to(CARET + name_column + GAP + tag_column);
            line.gap(GAP);
            line.push(&age, LABEL);
            // `truncate_left` is still necessary: two components are short but have no
            // limit, because a directory can have any name.
            let (cwd, _) = truncate_left(&short_cwd(&row.cwd), cwd_budget);
            line.pad_to(inner.saturating_sub(cwd.width()));
            // Not highlighted: the match ran on the label of the row, and hits painted on
            // another string would be wrong.
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

/// The agent prompt names where `Enter` puts you: the session, and the pane when the session
/// alone is not enough.
fn agent_prompt(agents: &AgentSet, term: &str) -> Prompt {
    match agents.selected_row() {
        Some(row) => (term.to_string(), Some(format!("Go to \"{}\"", row.label())), false),
        None => (term.to_string(), None, false),
    }
}

/// An agent outside zellij is not a row, because `Enter` can do nothing for it. The count here
/// stops such an agent from being absent without an explanation: its name would otherwise give
/// an empty list and no reason.
fn agent_note_texts(agents: &AgentSet, width: usize) -> Vec<Note> {
    let mut notes = Vec::new();
    if let Fetch::Failed(reason) = &agents.status {
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
        Fetch::Waiting => "looking for agents…".to_string(),
        // The reason is on the note line above. A pane this small has no space to say it
        // twice.
        Fetch::Failed(_) => "no agents".to_string(),
        Fetch::Ready if term.is_empty() => "no agents running".to_string(),
        Fetch::Ready => format!("no match for \"{}\"", term),
    }
}

/// The last two components of a path: `misc/luneta` for `/home/you/Projects/misc/luneta`.
///
/// In a column of agents the first components are the same `/home/you/…` on every row. They take
/// width and separate nothing. The end of the path is what differs.
///
/// Two components, not one, because this column must tell two agents apart. In a 136-path
/// zoxide database, the last two components collided zero times and the last component alone
/// collided nine times (`master`, `backend`, `frontend`, `bin`). One component would give two
/// different projects the same text.
///
/// There is no `…` here, unlike in `truncate_left`. The same rule applies to every row, so a
/// marker on all of them would give no information.
fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        // The root directory, or a string that is not a path.
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
            ("<Del>", "Kill or delete", "Delete"),
            ("<ESC>", "Close", "Close"),
        ],
    )
}

/// The keys of the directory screen. Two keys of the session screen have nothing to act on
/// here: a directory has no name to change, and to remove one is a different action on a
/// different database.
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

/// The keys of the agent screen. `Enter` does one thing on every row, and this screen must fit
/// the same pane as the other two.
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

/// A key and the two lengths of its description, longest first.
type Key<'a> = (&'a str, &'a str, &'a str);

/// The help line in the most detail that fits. There are four forms: `<KEY> - Description, …`,
/// the same without the dashes, the same with short descriptions, and the keys alone.
///
/// The floating pane is 60% of the terminal, which is too narrow for the first two forms with
/// the six keys of the search screen. The third form is thus the usual one. A description goes
/// before a key does, because you cannot find a key you cannot see.
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
    // Only the tests build these. The renderer receives them.
    use crate::sessions::{Focus, Selection};

    /// A pane, rendered to the lines it prints. This picture was not available inside the
    /// crate while the host assembled it.
    fn picture<R: Into<PreviewRow>>(
        rect: &Rect,
        title: &str,
        right: &str,
        interior: Vec<R>,
    ) -> Vec<String> {
        let mut lines = vec![rect.top(title, right).line];
        // The borders are added here because `draw_row` adds them there. The `Text` of a row
        // is the interior only, so that a selected row cannot highlight them.
        lines.extend(interior.into_iter().map(|line| {
            format!("{}{}{}", VERTICAL, line.into().content(), VERTICAL)
        }));
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

    fn contents(panes: usize, tab: &str, title: &str) -> Contents {
        Contents {
            panes,
            focus: Some(Focus { pane: 7, tab: tab.to_string(), title: title.to_string() }),
        }
    }

    /// A cache that holds one pane screen, as `dump-screen` leaves it.
    fn peeked(session: &str, pane: u32, screen: &str) -> Peeks {
        let mut peeks = Peeks::default();
        peeks.ingest(panes::key(session, pane), Some(0), screen.as_bytes(), b"");
        peeks
    }

    /// Two boxes side by side, as the pane draws them.
    fn beside(left: Vec<String>, right: Vec<String>) -> Vec<String> {
        left.into_iter().zip(right).map(|(left, right)| format!("{}{}", left, right)).collect()
    }

    const HOUR: u64 = 3600;

    /// The whole frame: the list at the bottom of its box, the age against the right border,
    /// and the separator between the two groups.
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

    /// With no dead sessions there is no separator. It is drawn only when it has a group to
    /// name, so the usual case costs no row.
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

    /// A list of dead sessions alone still gets the separator. Without it, nothing on the
    /// screen would say that `Enter` resurrects and does not attach.
    #[test]
    fn an_all_dead_list_still_gets_one() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 6 };
        let state = matches(vec![session("old", Kind::Resurrectable, HOUR)], Some(0));
        let body = search_body(&state, &rect, 0);
        assert_eq!(body.len(), 2);
        assert!(body[0].content().starts_with("   🪦 Dead sessions"));
        assert!(body[1].content().starts_with(" > old"));
    }

    /// The separator is a display line, so it takes a row from the window.
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
        // Four rows and a separator make five display lines for a box of four rows, so one
        // row scrolls out of view.
        let body = search_body(&matches(rows(), Some(0)), &rect, 0);
        assert_eq!(body.len(), 4);
        assert_eq!(body.iter().filter(|l| l.content().contains('🪦')).count(), 1);
    }

    /// A scroll into the dead group keeps the selection on the screen. The row index and the
    /// display line differ by one after the boundary, and a window over the wrong one loses the
    /// cursor.
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
            // The caret is on one line, and that line is the selected session.
            let carets: Vec<&&str> = shown.iter().filter(|l| l.starts_with(" > ")).collect();
            assert_eq!(carets.len(), 1, "selected {selected} fell off: {shown:?}");
            assert!(
                carets[0].starts_with(&format!(" > {} ", names[selected])),
                "selected {selected}: {shown:?}"
            );
        }
    }

    /// The notes are above the list, and the two are anchored as one block, so that a note is
    /// never between the highlighted row and the prompt.
    #[test]
    fn notes_ride_on_top_of_the_list() {
        let rect = Rect { x: 0, y: 0, width: 30, height: 7 };
        let state = matches(vec![session("luneta", Kind::Live, 2 * HOUR)], Some(0));
        let notes = vec![Note::dim("you are in \"desp\" — not listed")];
        let body = search_body(&state, &rect, notes.len());
        assert_eq!(
            picture(&rect, TITLE, "", interior(&rect, &notes, body)),
            vec![
                "╭─ luneta ───────────────────╮",
                "│                            │",
                "│                            │",
                "│                            │",
                "│ you are in \"desp\" — not l… │",
                "│ > luneta            2h ago │",
                "╰────────────────────────────╯",
            ]
        );
    }

    /// A note that is wider than the box is cut. A longer note would write over the right
    /// border.
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

    /// At every size and with every content, each line of a box is as wide as the box and
    /// closed at both ends. One short line would make a hole in the right border.
    #[test]
    fn every_line_is_exactly_the_width_of_its_box() {
        // Rebuilt for each case, not cloned. `Row` is not `Clone`, and a test must not change
        // a production type.
        let rows = || {
            vec![
                session("luneta", Kind::Live, 2 * HOUR),
                session("a-very-long-session-name-indeed", Kind::Resurrectable, 400 * HOUR),
                // Four characters and eight columns. A count of characters instead of columns
                // puts the right border in the wrong place.
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
                    for line in picture(&rect, TITLE, "9/9", interior) {
                        assert_eq!(line.width(), width, "{width}x{height}: {line}");
                        assert!(line.chars().next().is_some_and(|c| "╭╰│".contains(c)));
                        assert!(line.chars().last().is_some_and(|c| "╮╯│".contains(c)));
                    }
                }
            }
        }
    }

    /// An empty list says why it is empty, in the place of a row.
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

    /// The caret and the term on the left, and the action of `Enter` against the right.
    #[test]
    fn the_input_line_pushes_the_action_to_the_right() {
        let rect = Rect { x: 0, y: 0, width: 40, height: 3 };
        let line = input_line(&rect, ("desp".to_string(), Some("Attach".to_string()), false));
        assert_eq!(line.content(), " > desp_               <ENTER> Attach ");
        // The interior of the box, without the borders. See `Line::finish`.
        assert_eq!(line.content().width(), rect.width - 2);
    }

    /// The term is never cut. Below the width an action needs, the action goes and the term
    /// keeps the box.
    #[test]
    fn a_narrow_box_drops_the_action_not_the_term() {
        let rect = Rect { x: 0, y: 0, width: 18, height: 3 };
        let line = input_line(&rect, ("despesas".to_string(), Some("Attach".to_string()), false));
        assert_eq!(line.content(), " > despesas_    ");
        // The interior of the box, without the borders. See `Line::finish`.
        assert_eq!(line.content().width(), rect.width - 2);
    }

    /// A term that is longer than the box keeps its end, where the characters you typed last
    /// are, together with the cursor.
    #[test]
    fn an_overlong_term_is_cut_from_the_left() {
        let rect = Rect { x: 0, y: 0, width: 16, height: 3 };
        let line = input_line(&rect, ("a-very-long-name".to_string(), None, false));
        assert_eq!(line.content(), " > …ong-name_ ");
        // The interior of the box, without the borders. See `Line::finish`.
        assert_eq!(line.content().width(), rect.width - 2);
    }

    /// `3/47` with a selection, the count alone without one, and nothing when there is nothing
    /// to count.
    #[test]
    fn the_counter_reports_position_over_total() {
        assert_eq!(count(Some(2), 47), "3/47");
        assert_eq!(count(Some(0), 1), "1/1");
        assert_eq!(count(None, 47), "47");
        assert_eq!(count(None, 0), "");
        assert_eq!(count(Some(0), 0), "");
    }

    /// A list that fits shows all of its rows, from the top, at every selection.
    #[test]
    fn viewport_does_not_scroll_a_list_that_fits() {
        assert_eq!(viewport(0, 5, 10), (0, 5));
        assert_eq!(viewport(4, 5, 5), (0, 5));
        assert_eq!(viewport(0, 0, 10), (0, 0));
    }

    /// The scroll starts only when the selection would leave the bottom, and then moves as
    /// little as it can.
    #[test]
    fn viewport_scrolls_only_to_keep_the_selection_on_screen() {
        assert_eq!(viewport(2, 20, 5), (0, 5));
        assert_eq!(viewport(4, 20, 5), (0, 5));
        assert_eq!(viewport(5, 20, 5), (1, 6));
        assert_eq!(viewport(19, 20, 5), (15, 20));
    }

    /// The window is `visible` rows while the list can fill it, never passes the end of the
    /// list, and always holds the selection.
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

    /// The help line drops detail and never drops a key."
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

    /// Every form that reports that it fits must fit.
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
            // The last form can pass the width, because no shorter form names every key.
            if line.content().contains(' ') {
                assert!(line.content().width() <= width, "width {width}: {}", line.content());
            }
        }
    }

    /// The whole preview box: which pane you look at, and the end of what is on it.
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

    /// To the reader, "not asked yet" and "asked but not answered" are the same.
    #[test]
    fn a_pane_says_so_while_it_is_being_read() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut state = matches(vec![session("dotfiles", Kind::Live, HOUR)], Some(0));
        state.contents.insert("dotfiles".to_string(), contents(1, "editor", "nvim"));

        let unasked = session_preview(&state, &Peeks::default(), &rect).2;
        assert!(unasked[2].content().contains("reading…"));
        let mut peeks = Peeks::default();
        assert!(peeks.claim(&panes::key("dotfiles", 7)));
        let asked = session_preview(&state, &peeks, &rect).2;
        assert_eq!(asked[2].content(), unasked[2].content());

        // A pane with nothing on it is an answer, not a wait.
        let empty = peeked("dotfiles", 7, "\n\n");
        assert!(session_preview(&state, &empty, &rect).2[2].content().contains("nothing on"));
    }

    /// A dead session has no process, so it has no screen and nothing to count.
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

    /// With no highlight there is nothing to preview, and the box says so.
    #[test]
    fn an_empty_list_previews_nothing() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 6 };
        let (title, _, lines) = session_preview(&matches(Vec::new(), None), &Peeks::default(), &rect);
        assert_eq!(title, "Preview");
        assert_eq!(lines.len(), 3);
        assert!(lines[0].content().contains("nothing highlighted"));
    }

    /// The directory preview: the path, then the reply from eza, in the order eza gave it,
    /// with the count in the border.
    #[test]
    fn a_directory_preview_lists_what_eza_said() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"18 /home/you/misc/luneta\n", b"");
        dirs.rebuild("", &[], &[], None, Selection::SnapToTop);
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

    /// A coloured entry is padded by what it shows and not by what it holds. An escape takes
    /// no columns, so a line measured by its bytes would leave the right border short.
    #[test]
    fn a_coloured_listing_still_fills_the_box() {
        for width in 12..48 {
            let rect = Rect { x: 0, y: 0, width, height: 10 };
            let mut dirs = DirSet::default();
            dirs.ingest(Some(0), b"18 /home/you/misc/luneta\n", b"");
            dirs.rebuild("", &[], &[], None, Selection::SnapToTop);
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

    /// To the reader, "not asked yet" and "asked but not answered" are the same.
    #[test]
    fn a_directory_says_so_while_it_is_being_read() {
        let rect = Rect { x: 0, y: 0, width: 26, height: 8 };
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"18 /home/you/misc/luneta\n", b"");
        dirs.rebuild("", &[], &[], None, Selection::SnapToTop);

        let unasked = dir_preview(&dirs, &rect).2;
        assert!(unasked[2].content().contains("reading…"));
        assert!(dirs.begin_listing("/home/you/misc/luneta"));
        let asked = dir_preview(&dirs, &rect).2;
        assert_eq!(asked[2].content(), unasked[2].content());
        // No count in the border until there is something to count.
        assert_eq!(dir_preview(&dirs, &rect).1, "");
    }

    /// Only the preview box wraps text, and it breaks at spaces. A word that is wider than the
    /// box is cut, because a break inside a word reads as two words.
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

    /// Not a test. This prints whole panes with `cargo test -- --ignored --nocapture`, drawn
    /// by the renderer that draws them for real. The pictures in the README come from here.
    #[test]
    #[ignore = "prints the screens; run with --ignored --nocapture to look at them"]
    fn print_the_screens() {
        let (rows, cols) = (16, 84);
        let screen = Screen::new(rows, cols);
        let mut state = matches(
            vec![
                session("luneta", Kind::Live, 2 * 3600),
                session("dotfiles", Kind::Live, 5 * 3600),
                session("despesas-old", Kind::Resurrectable, 12 * 86400),
                session("api-spike", Kind::Resurrectable, 40 * 86400),
            ],
            Some(1),
        );
        state.contents.insert("dotfiles".to_string(), contents(3, "editor", "nvim"));
        let peeks = peeked(
            "dotfiles",
            7,
            "  1 //! luneta: a personal zellij session picker.\n  2 \n  3 mod agents;\n\
             \n\"src/main.rs\" 1005L, 41k\n",
        );
        let notes = vec![Note::dim("you are in \"notes\" — not listed")];
        let rect = screen.results.as_ref().unwrap();
        let body = search_body(&state, rect, notes.len());
        let right = count(state.selected, state.rows.len());
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
        agents.rebuild("", Some("notes"), None, Duration::ZERO, Selection::SnapToTop);
        let rect = screen.results.as_ref().unwrap();
        let notes = agent_note_texts(&agents, help_width(cols));
        let body = agent_body(&agents, "", rect, notes.len(), 0);
        let right = count(agents.selected, agents.rows.len());
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
        dirs.rebuild("", &[], &[], None, Selection::SnapToTop);
        dirs.ingest_listing(
            "/home/lorenzo/Projects/misc/luneta".to_string(),
            Some(0),
            EZA.as_bytes(),
            b"",
        );
        let rect = screen.results.as_ref().unwrap();
        let notes = dir_note_texts(&dirs);
        let body = dir_body(&dirs, "", rect, notes.len());
        let right = count(dirs.selected, dirs.rows.len());
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

    /// The two boxes, the prompt below them, and the help row below that.
    fn print_pane(boxes: Vec<String>, screen: &Screen, title: &str, prompt: Prompt, help: Text) {
        for line in boxes {
            println!("{line}");
        }
        let input = &screen.input;
        println!("{}", input.top(title, "").line);
        // Borders added as `draw_row` adds them. See `picture`.
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

    const ZOXIDE: &str = "9268 /home/lorenzo/Projects/misc/luneta\n\
        4102 /home/lorenzo/Projects/misc/homelab\n\
        1877 /home/lorenzo/Projects/Work/bipa\n\
        18 /home/lorenzo/.local/bin\n";

    /// What eza prints for this repo, copied from a run of the command in
    /// [`crate::dirs::LIST`]. Blue for a directory and yellow for a file, an icon before each
    /// name, and the `/` that `--classify` adds.
    const EZA: &str = "\x1b[34m\u{f4d4} \x1b[1msrc\x1b[0m/\n\
        \x1b[34m\u{e5ff} \x1b[1mtarget\x1b[0m/\n\
        \x1b[33m\u{e6a8} \x1b[1;4mCargo.toml\x1b[0m\n\
        \x1b[33m\u{e673} \x1b[1;4mMakefile\x1b[0m\n\
        \x1b[33m\u{f00ba} \x1b[1;4mREADME.md\x1b[0m\n";
}
