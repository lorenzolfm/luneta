//! What is on a pane's screen, for the box that shows it.
//!
//! The CLI does this, not the plugin API. `zellij-tile` has `get_pane_scrollback(PaneId, bool)`,
//! which needs no process and no temporary file, but it can only answer for the current
//! session: a plugin runs in one server, and a `PaneId` has no meaning in another. Every
//! session in the picker list is a different session, so that call cannot serve this box.
//! `zellij --session NAME action dump-screen` connects to the other server over its socket. It
//! also needs no new permission, because `RunCommands` is already granted for zoxide.
//!
//! The reply is asynchronous, so [`SCRIPT`] writes it to a file with `--path` and then waits.
//! `dump-screen` documents `--path` as optional and promises STDOUT without it, but 0.45.1
//! prints nothing: the CLI returns before the server answers. With `--path` the server writes
//! the file, and the CLI still returns first. Without the wait, every dump was empty. With it,
//! every dump was complete.
//!
//! The dump keeps the colours of the pane, because it uses `--ansi`. Text alone is a different
//! screen: a diff without its red and green, or a prompt without its git branch. The renderer
//! prints these rows unchanged. [`sgr_only`] makes that safe, because only the sequences that
//! set colour pass through this module.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar;

/// One pane's screen, dumped to a temporary file and read back.
///
/// `$1` is the session and `$2` is the pane (`terminal_7`). They are arguments, not text
/// interpolated into the script, so a session named `; rm -rf ~` stays a name. The output of
/// `zellij` goes to stderr and cannot enter the dump. The trap removes the temporary file.
///
/// The wait has a limit of one second and then stops without an error. An empty pane makes an
/// empty file that never fills, and that is a valid answer: [`Peek::Ready`] with no lines.
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
/// Which pane a reply is about, sent with the command and returned with the answer. See
/// [`crate::dirs::PATH_KEY`], which exists for the same reason.
pub const PANE_KEY: &str = "luneta_pane";

/// How a pane is named in the context of the command that asks about it.
///
/// The key holds the session and the pane, because each session numbers its panes separately.
/// The host's context map is `BTreeMap<String, String>`, so a pair has to become one string to
/// make the round trip. That is the only reason this exists: the cache is keyed by the pair.
///
/// The pane comes first so that the session is the remainder: [`parse_key`] splits once from
/// the left and takes everything after the tab, so nothing is assumed about what a session name
/// contains.
///
/// That is a clarity choice and not a bug fixed. Session-first was already injective, tabs in
/// names included — a decimal pane holds no tab, so the last tab was always the separator and
/// `rsplit_once` would have inverted it. The comment that used to sit here, that a session name
/// cannot contain a tab, was thus never load-bearing: nothing parsed the key at all. What this
/// order buys is that the parse below is correct on its own terms, with no argument about which
/// tab is the separator.
pub fn key(session: &str, pane: u32) -> String {
    format!("{}\t{}", pane, session)
}

/// The pair back out of a [`key`], for the reply that carries it back.
///
/// `None` when the value is not one of ours: no tab, or a pane that is not a number. Both are
/// the same failure as a missing key, and the caller drops the reply for the same reason.
pub fn parse_key(key: &str) -> Option<(String, u32)> {
    let (pane, session) = key.split_once('\t')?;
    Some((session.to_string(), pane.parse().ok()?))
}

/// How the pane is named to `dump-screen`. Terminals only, because a plugin pane dumps an
/// empty screen.
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

/// The maximum number of lines kept for one pane.
///
/// The box holds a few dozen rows and shows the tail, so more lines cannot be drawn. A pane of
/// 200 by 60 is several kilobytes of text for each session the cursor stops on.
const MAX_LINES: usize = 64;

/// How many pane screens to keep. Above this the cache is cleared. See [`crate::dirs`].
const MAX_PEEKS: usize = 32;

/// The screen of each pane the cursor has stopped on.
///
/// Each entry is a snapshot, taken when the cursor arrives and then held. A pane screen changes
/// continuously, but a new read for each tick would start one process per second for each row
/// you look at. The agent list makes the same trade. The cache is cleared when the picker
/// becomes visible again, which is when it would otherwise be out of date.
#[derive(Default)]
pub struct Peeks {
    screens: BTreeMap<(String, u32), Peek>,
}

impl Peeks {
    /// The pair allocates a `String` to look itself up: no `Borrow` impl reaches a
    /// `(String, u32)` from a `(&str, u32)`. This is what [`key`] cost before, at the same two
    /// draw-path calls, so it is parity and not a new cost.
    pub fn get(&self, session: &str, pane: u32) -> Option<&Peek> {
        self.screens.get(&(session.to_string(), pane))
    }

    /// Claim a pane for a dump, or refuse because there is nothing to ask.
    ///
    /// The [`Peek::Reading`] entry is the claim, so a second caller is refused. This gives one
    /// process per pane, not one per tick. A failure keeps its claim, because a pane that
    /// cannot be dumped will fail again on the next tick.
    pub fn claim(&mut self, session: &str, pane: u32) -> bool {
        let key = (session.to_string(), pane);
        if self.screens.contains_key(&key) {
            return false;
        }
        if self.screens.len() >= MAX_PEEKS {
            self.screens.clear();
        }
        self.screens.insert(key, Peek::Reading);
        true
    }

    /// Take the reply to a dump.
    ///
    /// Blank lines go from both ends, and only the tail is kept. You read a terminal from the
    /// bottom: the rows below the prompt are empty, and a preview of them shows nothing.
    pub fn ingest(
        &mut self,
        key: (String, u32),
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) {
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

    /// Clear every screen. They are snapshots, and this is the moment they become stale.
    pub fn forget(&mut self) {
        self.screens.clear();
    }
}

/// A dump, reduced to the lines that can be drawn.
///
/// Each line keeps its colours and loses all else. See [`sgr_only`].
///
/// Trailing spaces go. They are invisible, and [`fit`] would otherwise put a `…` at the right
/// border of a line of blanks. The cost is the right end of a full-width coloured bar, such as
/// the status line of vim, which then stops at its last word. `dump-screen` does not pad its
/// lines, so this only guards against a pane that pads its own.
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
/// To measure, cut or strip a line, you must walk it this way. Two walks that disagree about
/// the boundaries put a cut in the middle of a colour, so the split is done once here.
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

/// The rest of one escape sequence, read from `chars`. The `ESC` is not included.
///
/// A terminal has three shapes of sequence. `CSI` (`ESC [`) ends at a byte from `@` to `~`.
/// `OSC` and its relatives end at a `BEL` or a `ST`. All others are the escape and its
/// intermediates. Only the first shape is kept, but all three must be measured correctly: a
/// sequence that is one character short leaves its tail on the screen as text.
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

/// A pane line that keeps its colours and loses all else.
///
/// A pane can write any bytes, and this function is the border they cross. Only `SGR` (the
/// `ESC [ … m` that sets colour and weight) passes. The terminal that draws this plugin would
/// obey a cursor move, a scroll region, a screen clear, or an `OSC` that renames the tab. Each
/// of those lets a pane redraw the picker that is looking at it. Control characters also go.
pub fn sgr_only(line: &str) -> String {
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

/// How many columns of a box a line takes. Escapes take none, which is why this does not use
/// `UnicodeWidthStr::width`.
pub fn columns(line: &str) -> usize {
    parts(line).iter().map(width).sum()
}

/// Cut a line to `max` columns, mark the cut with `…`, and keep the colours complete.
///
/// This does the work of [`crate::layout::truncate`] over [`parts`] instead of over characters.
/// The marker comes out of the budget, so the result never takes more than `max` columns. Every
/// escape is kept, including those after the cut: they take no columns, and the reset at the
/// end of a line must stay.
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

    fn ready(peeks: &Peeks) -> Vec<String> {
        match peeks.get("s", 0) {
            Some(Peek::Ready(lines)) => lines.clone(),
            _ => panic!("no screen"),
        }
    }

    fn dumped(stdout: &[u8]) -> Peeks {
        let mut peeks = Peeks::default();
        peeks.ingest(("s".to_string(), 0), Some(0), stdout, b"");
        peeks
    }

    /// The blank part of the pane goes, from both ends. A shell at its prompt is two lines of
    /// screen and fifty blank lines.
    #[test]
    fn a_dump_is_trimmed_to_what_is_on_the_screen() {
        let peeks = dumped(b"\n\n> cargo test\nok\n   \n\n\n");
        assert_eq!(ready(&peeks), ["> cargo test", "ok"]);
        assert_eq!(ready(&dumped(b"")), Vec::<String>::new());
        assert_eq!(ready(&dumped(b"\n\n\n")), Vec::<String>::new());
    }

    /// Blank lines inside the screen are part of it and stay.
    #[test]
    fn a_gap_in_the_middle_is_screen_and_not_padding() {
        assert_eq!(ready(&dumped(b"one\n\ntwo\n")), ["one", "", "two"]);
    }

    /// The tail is kept, because you read a terminal from the bottom.
    #[test]
    fn an_overlong_dump_keeps_its_last_lines() {
        let dump: String = (0..MAX_LINES * 2).map(|i| format!("line-{}\n", i)).collect();
        let lines = ready(&dumped(dump.as_bytes()));
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[MAX_LINES - 1], format!("line-{}", MAX_LINES * 2 - 1));
    }

    /// Colour passes and all else does not. A bell or a tab is not part of the answer.
    #[test]
    fn a_dump_keeps_its_colours_and_nothing_else() {
        assert_eq!(ready(&dumped(b"a\x1b[31mred\x07\tb")), ["a\x1b[31mredb"]);
    }

    /// The escapes that would let a previewed pane redraw the picker. Each one is dropped
    /// complete: a sequence that is one character short leaves its tail on the screen as
    /// text.
    #[test]
    fn an_escape_that_is_not_a_colour_is_dropped_whole() {
        // A cursor move, a screen clear, a scroll region, a cursor-hide.
        assert_eq!(ready(&dumped(b"\x1b[9;9Ha\x1b[2Jb\x1b[1;5rc\x1b[?25ld")), ["abcd"]);
        // An `OSC` that renames the tab, terminated both ways it can be.
        assert_eq!(ready(&dumped(b"\x1b]0;title\x07a\x1b]2;more\x1b\\b")), ["ab"]);
        // A `DCS` payload, which is the form the host uses for its own components.
        assert_eq!(ready(&dumped(b"a\x1bPztext;1,2\x1b\\b")), ["ab"]);
        // A two-character escape, and one carrying an intermediate.
        assert_eq!(ready(&dumped(b"a\x1b7b\x1b(Bc")), ["abc"]);
    }

    /// Escapes take no columns, so a coloured line is measured by its visible characters.
    #[test]
    fn a_coloured_line_is_measured_by_what_can_be_seen() {
        let line = "\x1b[31mred\x1b[m";
        assert_eq!(columns(line), 3);
        assert_eq!(columns("日本"), 4);
        assert_eq!(fit(line, 3), line);
    }

    /// The cut falls between characters and keeps every colour, including those after it. The
    /// reset at the end of a line stops the colour from reaching the border.
    #[test]
    fn a_cut_line_keeps_its_colours() {
        assert_eq!(fit("\x1b[31mredder\x1b[m", 4), "\x1b[31mred…\x1b[m");
        assert_eq!(columns(&fit("\x1b[31mredder\x1b[m", 4)), 4);
        // A wide character is not split in half to make the budget.
        assert_eq!(fit("日本語", 4), "日…");
    }

    /// A pane is asked about once, whatever the answer is.
    #[test]
    fn a_pane_is_only_ever_asked_about_once() {
        let mut peeks = Peeks::default();
        assert!(peeks.claim("s", 0));
        assert!(!peeks.claim("s", 0));
        peeks.ingest(("s".to_string(), 0), Some(1), b"", b"zellij: no such session\n");
        assert!(!peeks.claim("s", 0));
        match peeks.get("s", 0) {
            Some(Peek::Failed(reason)) => assert_eq!(reason, "zellij: no such session"),
            _ => panic!("expected a failure"),
        }
        peeks.forget();
        assert!(peeks.claim("s", 0));
    }

    /// The cache names a pane and not a session, so two panes of one session are two entries.
    /// This used to be an assertion about [`key`], because the key was the cache key. It is now
    /// an assertion about the cache, which is where the fact lives.
    #[test]
    fn the_cache_names_a_pane_and_not_a_session() {
        let mut peeks = Peeks::default();
        assert!(peeks.claim("misc", 1));
        assert!(peeks.claim("misc", 2));
        assert!(peeks.claim("notes", 1));
        assert!(!peeks.claim("misc", 1));
        assert_eq!(pane_id(7), "terminal_7");
    }

    /// The wire key round-trips, and the session survives being the remainder: a name holding a
    /// tab comes back whole, because the split takes the first tab and the pane before it can
    /// hold none.
    #[test]
    fn a_wire_key_round_trips_whatever_the_session_is_called() {
        assert_eq!(parse_key(&key("misc", 7)), Some(("misc".to_string(), 7)));
        assert_eq!(parse_key(&key("a\tb", 7)), Some(("a\tb".to_string(), 7)));
        assert_eq!(parse_key(&key("", 0)), Some((String::new(), 0)));
        // Not one of ours: no separator, and a pane that is not a number.
        assert_eq!(parse_key("misc"), None);
        assert_eq!(parse_key("seven\tmisc"), None);
    }
}
