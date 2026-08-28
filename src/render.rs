//! Drawing the picker.
//!
//! Written from scratch rather than ported. Upstream's `ui/components.rs` is 1847 lines, and
//! most of it is machinery this picker no longer needs: the tab and pane drill-down is cut, and
//! with it the four-column layout and the five-tier width-reduction algorithm that fed it.
//! What is left is three columns of fixed shape, so the reduction is a three-step ladder.
//!
//! Styling is plain SGR — no `Styling` palette, and so no `ModeUpdate` subscription. Reverse
//! video and bold take the terminal's own theme, which is the right answer for a personal tool.

use unicode_width::UnicodeWidthStr;

use crate::layouts::LayoutList;
use crate::sessions::{format_age, MatchSet, Row};

const RESET: &str = "\u{1b}[m";
const BOLD: &str = "\u{1b}[1m";
const DIM: &str = "\u{1b}[2m";
const REVERSE: &str = "\u{1b}[7m";

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

/// Build the whole frame, then emit it in one `print!` with **no trailing newline**.
///
/// This is not a style preference. A plugin pane is exactly `rows` tall with no scrollback of
/// its own, so printing `rows` lines each terminated by `\n` scrolls the first line off the top
/// — which silently ate the `Session:` prompt. Upstream sidesteps it with absolute cursor
/// positioning (`\u{1b}[{y};{x}H`); building the frame is the same fix without the coordinates.
pub fn render_search(state: &MatchSet, rows: usize, cols: usize) {
    let mut lines = Vec::with_capacity(rows);
    lines.extend(prompt_lines(state, cols));
    lines.extend(hint_lines(state, cols));

    let list_rows = rows.saturating_sub(lines.len());
    if list_rows > 0 {
        lines.extend(list_lines(state, list_rows, cols));
    }
    lines.truncate(rows);

    print!("{}{}", RESET, lines.join("\n"));
}

/// The search term. Trailing `_` is a cursor stand-in: a plugin's real cursor is off by default
/// and turning it on would mean tracking its position through every re-render.
fn prompt_lines(state: &MatchSet, _cols: usize) -> Vec<String> {
    vec![
        format!("{}Session:{} {}{}{}_{}", DIM, RESET, BOLD, state.search_term, RESET, RESET),
        String::new(),
    ]
}

/// The hint line. Dim, **outside** the result list: never a row, never
/// selectable, never indexed — which is what lets it talk about the current session without
/// putting the current session back into the match set it was deliberately taken out of.
///
/// It answers two questions the list cannot:
///
/// - *"where did my own session go?"* — from inside `despesas`, typing `desp` gives a blank
///   list and no explanation. Shown only when the term actually reaches for it; with an empty
///   term you can see the list and you know where you are.
/// - *"what will `Enter` do, now that nothing is highlighted?"* — the no-highlight half of the
///   contract is invisible otherwise. This is also where name validation surfaces, live, rather
///   than as an error on `Enter`.
fn hint_lines(state: &MatchSet, cols: usize) -> Vec<String> {
    let mut hints: Vec<String> = Vec::new();

    if state.current_matches {
        if let Some(current) = &state.current_session {
            hints.push(format!("you are in \"{}\" (not listed)", current));
        }
    }

    // Only when there is no highlight: with one, the highlighted row already says what Enter
    // does, and a second sentence about creating would contradict it.
    if state.selected.is_none() {
        if state.is_own_name() {
            // A no-op, not an error and not an offer to create a name that is
            // already taken by the session you are sitting in.
            hints.push("already attached · Enter does nothing".to_string());
        } else if let Some(reason) = state.name_error() {
            hints.push(format!("invalid · {}", reason));
        } else if state.search_term.is_empty() {
            hints.push("Enter to create a new session".to_string());
        } else {
            hints.push(format!("Enter to create \"{}\"", state.search_term));
        }
    }

    if hints.is_empty() {
        return vec![];
    }
    let mut lines: Vec<String> = hints
        .iter()
        .map(|text| format!("{}{}{}", DIM, truncate(text, cols), RESET))
        .collect();
    lines.push(String::new());
    lines
}

/// The confirm step. Nothing has been created at this point — this screen is what makes that
/// true, and `Esc` backs out of it with the typed name intact.
pub fn render_layouts(state: &MatchSet, layouts: &LayoutList, rows: usize, cols: usize) {
    let name = if state.search_term.is_empty() {
        "<auto-named>".to_string()
    } else {
        state.search_term.clone()
    };
    let mut lines = vec![
        format!("{}New session:{} {}{}{}", DIM, RESET, BOLD, truncate(&name, cols), RESET),
        String::new(),
        format!("{}{}{}", DIM, truncate("layout — Enter to create, Esc to go back", cols), RESET),
    ];

    if layouts.layouts.is_empty() {
        lines.push(format!("{}  (none — the host will use its default){}", DIM, RESET));
    } else {
        let budget = rows.saturating_sub(lines.len());
        let (start, end) = viewport(layouts.selected, layouts.layouts.len(), budget);
        for (i, layout) in layouts.layouts.iter().enumerate().take(end).skip(start) {
            let selected = i == layouts.selected;
            let mut line = String::new();
            line.push_str(if selected { REVERSE } else { "" });
            line.push_str("  ");
            line.push_str(&truncate(layout.name(), cols.saturating_sub(2)));
            line.push_str(RESET);
            lines.push(truncate_styled(&line, cols));
        }
    }

    lines.truncate(rows);
    print!("{}{}", RESET, lines.join("\n"));
}

fn list_lines(state: &MatchSet, max_rows: usize, cols: usize) -> Vec<String> {
    if state.rows.is_empty() {
        let text = if state.search_term.is_empty() {
            "no sessions".to_string()
        } else {
            format!("no match for '{}'", state.search_term)
        };
        return vec![format!("{}{}{}", DIM, truncate(&text, cols), RESET)];
    }

    // One row of the budget is kept back for the "+N more" line whenever the list overflows.
    let overflows = state.rows.len() > max_rows;
    let visible_rows = if overflows { max_rows.saturating_sub(1) } else { max_rows };
    let (start, end) = viewport(state.selected.unwrap_or(0), state.rows.len(), visible_rows);
    let window = &state.rows[start..end];

    // Widths are measured over the *visible* window only. Measuring the whole list would make
    // the name column jump as you scroll, for the sake of names you cannot see.
    let name_width = window.iter().map(|r| r.name.width()).max().unwrap_or(0);
    let tag_width = window.iter().map(|r| r.kind.full_tag().len()).max().unwrap_or(0);
    let age_width = window.iter().map(|r| format_age(r.age).width()).max().unwrap_or(0);

    let fit = choose_fit(name_width, tag_width, age_width, cols);
    let tag_cell_width = if matches!(fit, Fit::Full) { tag_width } else { 3 };
    let name_width = name_width.min(name_column_budget(&fit, tag_width, age_width, cols));

    let mut lines: Vec<String> = window
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected = state.selected == Some(start + offset);
            render_row(row, selected, &fit, name_width, tag_cell_width, cols)
        })
        .collect();

    if overflows {
        let hidden = state.rows.len() - window.len();
        lines.push(format!("{}  +{} more{}", DIM, hidden, RESET));
    }
    lines
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

fn choose_fit(name_width: usize, tag_width: usize, age_width: usize, cols: usize) -> Fit {
    const GUTTER: usize = 2; // leading indent
    const GAP: usize = 2; // between columns
    let full = GUTTER + name_width + GAP + tag_width + GAP + age_width;
    if full <= cols {
        return Fit::Full;
    }
    let abbr = GUTTER + name_width + GAP + 3 + GAP + age_width;
    if abbr <= cols {
        return Fit::AbbrTag;
    }
    Fit::NoAge
}

/// How many columns the name may use once the fixed columns have taken theirs. A name is
/// truncated rather than wrapped: a wrapped name would break the one-row-per-session invariant
/// the selection index depends on.
fn name_column_budget(fit: &Fit, tag_width: usize, age_width: usize, cols: usize) -> usize {
    let fixed = match fit {
        Fit::Full => 2 + 2 + tag_width + 2 + age_width,
        Fit::AbbrTag => 2 + 2 + 3 + 2 + age_width,
        Fit::NoAge => 2 + 2 + 3,
    };
    cols.saturating_sub(fixed).max(4)
}

fn render_row(
    row: &Row,
    selected: bool,
    fit: &Fit,
    name_width: usize,
    tag_width: usize,
    cols: usize,
) -> String {
    let name = truncate(&row.name, name_width);
    let name_pad = name_width.saturating_sub(name.width());

    let tag = match fit {
        Fit::Full => row.kind.full_tag(),
        Fit::AbbrTag | Fit::NoAge => row.kind.abbr_tag(),
    };
    // [ATTACH] and [RESURRECT] differ by three columns, so the tag cell is padded too —
    // otherwise the age column steps right on every resurrectable row.
    let tag_pad = tag_width.saturating_sub(tag.len());

    let mut line = String::new();
    line.push_str(if selected { REVERSE } else { "" });
    line.push_str("  ");
    // Highlight the matched characters, but only when nothing is inverting the row already —
    // bold inside reverse video is a smear, not an emphasis.
    line.push_str(&highlight(&name, &row.indices, selected));
    line.push_str(&" ".repeat(name_pad));
    line.push_str("  ");
    line.push_str(&dim_unless(tag, selected));
    line.push_str(&" ".repeat(tag_pad));
    if !matches!(fit, Fit::NoAge) {
        line.push_str("  ");
        line.push_str(&dim_unless(&format_age(row.age), selected));
    }
    line.push_str(RESET);
    truncate_styled(&line, cols)
}

fn highlight(name: &str, indices: &[usize], selected: bool) -> String {
    if indices.is_empty() || selected {
        return name.to_string();
    }
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if indices.contains(&i) {
            out.push_str(BOLD);
            out.push(ch);
            out.push_str(RESET);
        } else {
            out.push(ch);
        }
    }
    out
}

fn dim_unless(text: &str, selected: bool) -> String {
    if selected {
        text.to_string()
    } else {
        format!("{}{}{}", DIM, text, RESET)
    }
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

/// Truncate to a visible width while stepping over SGR sequences, so a cut never lands inside
/// an escape and leaks `[2m` into the pane.
fn truncate_styled(line: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            out.push(ch);
            for esc in chars.by_ref() {
                out.push(esc);
                if esc == 'm' {
                    break;
                }
            }
            continue;
        }
        let w = ch.to_string().width();
        if width + w > max {
            out.push_str(RESET);
            return out;
        }
        out.push(ch);
        width += w;
    }
    out
}
