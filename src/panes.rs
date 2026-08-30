//! What is on a pane's screen, for the box that shows it.
//!
//! 🔴 **The CLI, not the plugin API, and that is forced.** `zellij-tile` has
//! `get_pane_scrollback(PaneId, bool)`, which is a blocking host call needing no process and no
//! temporary file — and it can only ever answer for *this* session's panes, because a plugin
//! runs inside one server and a `PaneId` means nothing to another. Every session on the picker's
//! list is a different session (the current one is dropped at the source), so the one screen
//! this is for is the one screen that API cannot serve. `zellij --session NAME action
//! dump-screen` connects to that session's own server over its socket, which is the only route
//! there is. It also costs no new permission: `RunCommands` is already granted for zoxide.
//!
//! ⚠️ **`--path` and then read it back, because the reply is asynchronous.** `dump-screen`
//! documents `--path` as optional and promises STDOUT without it; in 0.45.1 that prints nothing
//! — the CLI returns before the server's answer arrives. With `--path` the *server* writes the
//! file, and the CLI still returns first, so [`SCRIPT`] waits for the file to have something in
//! it rather than reading it straight away. Measured: without the wait, every dump came back
//! empty; with it, every one came back whole.
//!
//! 🔴 **`--ansi`, and the colours are the pane's own.** A dump without it is the pane's *text*,
//! and a preview of text is a preview of a different screen: a diff without its red and green,
//! a test run without its failure, a prompt without the git branch that is the only coloured
//! thing on the line. So the dump keeps its styling and the renderer puts it through untouched
//! — the one place in the picker that paints in colours it did not choose, because they are the
//! answer to the question the box is asking. [`sgr_only`] is what makes that safe: a pane can
//! write *anything*, and only the sequences that set colour survive contact with this module.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar;

/// One pane's screen, dumped to a temporary file and read back.
///
/// `$1` is the session, `$2` the pane (`terminal_7`). Passed as **arguments** rather than
/// interpolated into the script, so a session named `; rm -rf ~` is a session name and not a
/// command. `zellij`'s own output goes to stderr so it cannot land in the dump; the trap takes
/// the temporary file with it whichever way the script leaves.
///
/// The wait is bounded at a second and gives up quietly: a pane with nothing on it produces an
/// empty file that will never fill, and that is a real answer — [`Peek::Ready`] with no lines.
const SCRIPT: &str = r#"set -e
f=$(mktemp)
trap 'rm -f "$f"' EXIT
zellij --session "$1" action dump-screen --pane-id "$2" --ansi --path "$f" >&2
i=0
while [ ! -s "$f" ] && [ "$i" -lt 20 ]; do sleep 0.05; i=$((i+1)); done
cat "$f""#;

/// The command, up to its two arguments. `$0` is named so that `sh` reports errors as ours.
pub const DUMP: [&str; 4] = ["sh", "-c", SCRIPT, "luneta"];

/// Marks our own `RunCommandResult`, on the same channel as the other two commands.
pub const CONTEXT_VALUE: &str = "pane";
/// Which pane a reply is about, carried out and back — see [`crate::dirs::PATH_KEY`], which
/// exists for the same reason and would tell the same story.
pub const PANE_KEY: &str = "luneta_pane";

/// How a pane is named in the cache, and in the context of the command that asks about it.
///
/// Session **and** pane, because one session has many and two sessions have their own numbering.
/// A tab is the separator on the grounds that it is the one character a session name will not
/// have in it; nothing ever parses this back apart, so the worst a collision could do is show
/// one pane's screen under another's name.
pub fn key(session: &str, pane: u32) -> String {
    format!("{}\t{}", session, pane)
}

/// How the pane is spelled to `dump-screen`. Terminals only — a plugin pane dumps empty.
pub fn pane_id(pane: u32) -> String {
    format!("terminal_{}", pane)
}

/// What the preview box has of one pane's screen.
pub enum Peek {
    /// The dump has been asked for and has not come back.
    Reading,
    /// The screen, blank top and bottom already trimmed, newest line last. Empty when the pane
    /// had nothing on it.
    Ready(Vec<String>),
    /// The pane could not be dumped. Carries what to put on screen.
    Failed(String),
}

/// The most lines kept for one pane.
///
/// A preview box is a few dozen rows at the very most and the box shows the *tail*, so anything
/// past this could never be drawn. A pane 200 columns wide by 60 rows is otherwise several
/// kilobytes of string per session the cursor rests on.
const MAX_LINES: usize = 64;

/// How many panes' screens are worth keeping. Past this the cache is dropped whole — see
/// [`crate::dirs`], where the same policy is argued at length.
const MAX_PEEKS: usize = 32;

/// What each pane the cursor has rested on had on its screen.
///
/// ⚠️ A **snapshot**, taken when the cursor landed and then held. A pane's screen is the one
/// thing in this plugin that genuinely changes from moment to moment, so this is the same bargain
/// the agent list makes and for the same reason: re-reading it under the cursor would mean
/// forking a process a second per row you are reading. The cache is dropped whole when the
/// picker is opened again, which is when it would otherwise start lying.
#[derive(Default)]
pub struct Peeks {
    screens: BTreeMap<String, Peek>,
}

impl Peeks {
    pub fn get(&self, key: &str) -> Option<&Peek> {
        self.screens.get(key)
    }

    /// Claim a pane for a dump, or refuse because there is nothing to ask.
    ///
    /// The claim *is* the [`Peek::Reading`] entry, so a second caller is refused — one process
    /// per pane rather than one per tick. A failure holds its claim too: a pane that could not
    /// be dumped will not dump any better on the next tick.
    pub fn claim(&mut self, key: &str) -> bool {
        if self.screens.contains_key(key) {
            return false;
        }
        if self.screens.len() >= MAX_PEEKS {
            self.screens.clear();
        }
        self.screens.insert(key.to_string(), Peek::Reading);
        true
    }

    /// Take a dump's reply.
    ///
    /// Blank lines are trimmed from both ends and only the tail is kept, because a terminal is
    /// read from the bottom: the rows below the prompt are the pane's empty half, not its
    /// newest half, and a preview that showed them would show you nothing for most panes.
    pub fn ingest(&mut self, key: String, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.screens.insert(
                key,
                Peek::Failed(match reason.is_empty() {
                    true => "the pane could not be read".to_string(),
                    false => reason.to_string(),
                }),
            );
            return;
        }
        self.screens.insert(key, Peek::Ready(trim(&String::from_utf8_lossy(stdout))));
    }

    /// Drop every screen. See the ⚠️ on the type: they are snapshots, and this is the moment
    /// they stop being worth keeping.
    pub fn forget(&mut self) {
        self.screens.clear();
    }
}

/// A dump, cut down to the lines worth drawing.
///
/// Each line keeps its colours and loses everything else — see [`sgr_only`], which is where the
/// pane's bytes stop being arbitrary.
///
/// ⚠️ A trailing run of spaces goes, as it always did: it is what a pane leaves behind rather
/// than something anyone can see, and [`fit`] would otherwise mark a line of blanks as cut off
/// with a `…` at the right border. The cost is the right end of a full-width coloured bar —
/// vim's status line, tmux's — which stops at its last word rather than at the box. `dump-screen`
/// does not pad its lines out, so this is a guard against a pane that does it to itself.
fn trim(dump: &str) -> Vec<String> {
    let mut lines: Vec<String> = dump.lines().map(sgr_only).collect();
    while lines.last().is_some_and(|line| columns(line) == 0) {
        lines.pop();
    }
    let blank = lines.iter().take_while(|line| columns(line) == 0).count();
    lines.drain(..blank);
    if lines.len() > MAX_LINES {
        lines.drain(..lines.len() - MAX_LINES);
    }
    lines
}

/// The escape character every sequence below opens with.
const ESC: char = '\u{1b}';

/// A line of a dump, split into the characters that take a column and the escapes that do not.
///
/// Everything that reads a pane's line — measuring it, cutting it, stripping it — has to walk it
/// this way, and a walk that gets the boundaries slightly differently from its neighbour is how
/// a cut lands in the middle of a colour. So it is done once, here, and the three below are
/// folds over the result.
enum Part {
    /// One character the reader can see.
    Ch(char),
    /// One escape sequence, whole, from the `ESC` to its last byte.
    Escape(String),
}

fn parts(line: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            ESC => parts.push(Part::Escape(escape(&mut chars))),
            ch if ch.is_control() => {},
            ch => parts.push(Part::Ch(ch)),
        }
    }
    parts
}

/// The rest of one escape sequence, consumed from `chars`, `ESC` excluded.
///
/// Three shapes, because the terminal has three: `CSI` (`ESC [`) runs to a byte in `@`–`~`;
/// `OSC` and its relatives run to a `BEL` or a `ST`; anything else is the escape and its
/// intermediates. Only the first shape is ever kept, but all three have to be *measured*
/// correctly — a sequence walked off by one character leaves its tail behind as text.
fn escape(chars: &mut std::str::Chars) -> String {
    let mut seq = String::new();
    match chars.next() {
        Some('[') => {
            seq.push('[');
            for ch in chars.by_ref() {
                seq.push(ch);
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    break;
                }
            }
        },
        Some(ch @ (']' | 'P' | 'X' | '^' | '_')) => {
            seq.push(ch);
            while let Some(ch) = chars.next() {
                if ch == '\u{7}' {
                    break;
                }
                if ch == ESC {
                    // `ST` is `ESC \`; the backslash goes with it.
                    chars.next();
                    break;
                }
            }
        },
        Some(ch) => {
            seq.push(ch);
            // An `nF` escape carries its intermediates before the byte that ends it: `ESC ( B`.
            if ('\u{20}'..='\u{2f}').contains(&ch) {
                for next in chars.by_ref() {
                    seq.push(next);
                    if !('\u{20}'..='\u{2f}').contains(&next) {
                        break;
                    }
                }
            }
        },
        None => {},
    }
    seq
}

/// A pane's line with its colours kept and everything else thrown away.
///
/// 🔴 A pane can write *anything*, and this is the border it writes it across. Only `SGR` — the
/// `ESC [ … m` that sets colour and weight — survives; a cursor move, a scroll region, a screen
/// clear, an `OSC` that renames the tab would each be obeyed by the terminal drawing this
/// plugin, and every one of them means a pane the picker is only *looking* at gets to redraw the
/// picker. The rest of the line is characters, minus the control ones.
fn sgr_only(line: &str) -> String {
    let mut kept: Vec<Part> = parts(line)
        .into_iter()
        .filter(|part| match part {
            Part::Escape(seq) => seq.starts_with('[') && seq.ends_with('m'),
            Part::Ch(_) => true,
        })
        .collect();
    while kept.last().is_some_and(|part| matches!(part, Part::Ch(' '))) {
        kept.pop();
    }
    kept.iter().map(render).collect()
}

/// How many columns of a box a line would take up. Escapes take none — that is the whole point
/// of measuring this way rather than with `UnicodeWidthStr::width`, which would count them.
pub fn columns(line: &str) -> usize {
    parts(line).iter().map(width).sum()
}

/// Cut a line to `max` columns, marking the cut with `…` and keeping the colours whole.
///
/// [`crate::layout::truncate`]'s job, done over [`parts`] instead of over characters: the marker
/// is paid for out of the budget, so what comes back never takes more than `max` columns. Every
/// escape is kept, including the ones past the cut — they cost nothing to draw, and dropping the
/// tail of a line is not a reason to drop the reset that ends it.
pub fn fit(line: &str, max: usize) -> String {
    let parts = parts(line);
    if parts.iter().map(width).sum::<usize>() <= max {
        return parts.iter().map(render).collect();
    }
    let mut out = String::new();
    let mut used = 0;
    let mut cut = false;
    for part in &parts {
        if !cut && used + width(part) > max.saturating_sub(1) {
            cut = true;
            out.push('…');
        }
        if cut && matches!(part, Part::Ch(_)) {
            continue;
        }
        used += width(part);
        out.push_str(&render(part));
    }
    out
}

fn width(part: &Part) -> usize {
    match part {
        Part::Ch(ch) => ch.width().unwrap_or(0),
        Part::Escape(_) => 0,
    }
}

fn render(part: &Part) -> String {
    match part {
        Part::Ch(ch) => ch.to_string(),
        Part::Escape(seq) => format!("{}{}", ESC, seq),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(peeks: &Peeks, key: &str) -> Vec<String> {
        match peeks.get(key) {
            Some(Peek::Ready(lines)) => lines.clone(),
            _ => panic!("no screen"),
        }
    }

    fn dumped(stdout: &[u8]) -> Peeks {
        let mut peeks = Peeks::default();
        peeks.ingest("k".to_string(), Some(0), stdout, b"");
        peeks
    }

    /// The blank half of the pane goes, from both ends. A shell sitting at its prompt is two
    /// lines of screen and fifty of nothing, and the fifty are what you would see otherwise.
    #[test]
    fn a_dump_is_trimmed_to_what_is_on_the_screen() {
        let peeks = dumped(b"\n\n> cargo test\nok\n   \n\n\n");
        assert_eq!(ready(&peeks, "k"), ["> cargo test", "ok"]);
        assert_eq!(ready(&dumped(b""), "k"), Vec::<String>::new());
        assert_eq!(ready(&dumped(b"\n\n\n"), "k"), Vec::<String>::new());
    }

    /// Blank lines *inside* the screen are part of it, and stay.
    #[test]
    fn a_gap_in_the_middle_is_screen_and_not_padding() {
        assert_eq!(ready(&dumped(b"one\n\ntwo\n"), "k"), ["one", "", "two"]);
    }

    /// The tail is what is kept, because a terminal is read from the bottom.
    #[test]
    fn an_overlong_dump_keeps_its_last_lines() {
        let dump: String = (0..MAX_LINES * 2).map(|i| format!("line-{}\n", i)).collect();
        let lines = ready(&dumped(dump.as_bytes()), "k");
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[MAX_LINES - 1], format!("line-{}", MAX_LINES * 2 - 1));
    }

    /// Colour survives; everything else does not. `SGR` is the pane's answer to what the
    /// preview box is asking, and a bell or a tab is not.
    #[test]
    fn a_dump_keeps_its_colours_and_nothing_else() {
        assert_eq!(ready(&dumped(b"a\x1b[31mred\x07\tb"), "k"), ["a\x1b[31mredb"]);
    }

    /// 🔴 The escapes that would let a previewed pane redraw the picker. Each is dropped
    /// *whole*: a sequence walked off by one character leaves its tail behind as text, which is
    /// the failure this is really guarding against.
    #[test]
    fn an_escape_that_is_not_a_colour_is_dropped_whole() {
        // A cursor move, a screen clear, a scroll region, a cursor-hide.
        assert_eq!(ready(&dumped(b"\x1b[9;9Ha\x1b[2Jb\x1b[1;5rc\x1b[?25ld"), "k"), ["abcd"]);
        // An `OSC` that renames the tab, terminated both ways it can be.
        assert_eq!(ready(&dumped(b"\x1b]0;title\x07a\x1b]2;more\x1b\\b"), "k"), ["ab"]);
        // A `DCS` payload — the shape the host's own components arrive in.
        assert_eq!(ready(&dumped(b"a\x1bPztext;1,2\x1b\\b"), "k"), ["ab"]);
        // A two-character escape, and one carrying an intermediate.
        assert_eq!(ready(&dumped(b"a\x1b7b\x1b(Bc"), "k"), ["abc"]);
    }

    /// Escapes take no columns, so a coloured line is measured and cut by what is on it.
    #[test]
    fn a_coloured_line_is_measured_by_what_can_be_seen() {
        let line = "\x1b[31mred\x1b[m";
        assert_eq!(columns(line), 3);
        assert_eq!(columns("日本"), 4);
        assert_eq!(fit(line, 3), line);
    }

    /// The cut lands between characters and keeps every colour, including the ones past it —
    /// the reset that ends a line is what stops it bleeding into the border.
    #[test]
    fn a_cut_line_keeps_its_colours() {
        assert_eq!(fit("\x1b[31mredder\x1b[m", 4), "\x1b[31mred…\x1b[m");
        assert_eq!(columns(&fit("\x1b[31mredder\x1b[m", 4)), 4);
        // A wide character is not split in half to make the budget.
        assert_eq!(fit("日本語", 4), "日…");
    }

    /// A pane is asked about once, whatever the answer was.
    #[test]
    fn a_pane_is_only_ever_asked_about_once() {
        let mut peeks = Peeks::default();
        assert!(peeks.claim("k"));
        assert!(!peeks.claim("k"));
        peeks.ingest("k".to_string(), Some(1), b"", b"zellij: no such session\n");
        assert!(!peeks.claim("k"));
        match peeks.get("k") {
            Some(Peek::Failed(reason)) => assert_eq!(reason, "zellij: no such session"),
            _ => panic!("expected a failure"),
        }
        peeks.forget();
        assert!(peeks.claim("k"));
    }

    /// Session and pane, so two panes of one session are two entries.
    #[test]
    fn a_key_names_a_pane_and_not_a_session() {
        assert_ne!(key("misc", 1), key("misc", 2));
        assert_ne!(key("misc", 1), key("notes", 1));
        assert_eq!(pane_id(7), "terminal_7");
    }
}
