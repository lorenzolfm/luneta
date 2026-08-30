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

use std::collections::BTreeMap;

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
zellij --session "$1" action dump-screen --pane-id "$2" --path "$f" >&2
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
/// ⚠️ Control characters are dropped rather than passed on. `dump-screen` gives plain text —
/// verified against a real pane, not assumed — but a `Text` carrying an escape sequence would be
/// a hole in the box's right border at best, so the one place a pane's own bytes enter the
/// renderer takes them out.
fn trim(dump: &str) -> Vec<String> {
    let mut lines: Vec<String> = dump
        .lines()
        .map(|line| line.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|line| line.trim_end().to_string())
        .collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let blank = lines.iter().take_while(|line| line.is_empty()).count();
    lines.drain(..blank);
    if lines.len() > MAX_LINES {
        lines.drain(..lines.len() - MAX_LINES);
    }
    lines
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

    /// A `Text` carrying an escape sequence would put a hole in the box, so the bytes are
    /// cleaned where they come in.
    #[test]
    fn control_characters_never_reach_the_renderer() {
        assert_eq!(ready(&dumped(b"a\x1b[31mred\x07\tb"), "k"), ["a[31mredb"]);
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
