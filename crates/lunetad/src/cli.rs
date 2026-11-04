//! zellij's CLI as luneta and its daemon run it, on the machine they run on:
//! with none of the variables of a pane it may run in, and nothing to read,
//! what it says as it answers or fails, and a session's panes as it lists
//! them.

use std::collections::BTreeSet;
use std::io;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde::Deserialize;

use crate::program;

/// What names the session of a zellij pane, in the variables of what runs in
/// it.
pub const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";

/// What names the pane a zellij command runs in, in its variables, which
/// zellij opens a pane beside.
pub const PANE_VAR: &str = "ZELLIJ_PANE_ID";

/// Variables that zellij's client must not have. With `ZELLIJ` set, it takes
/// itself for one started in a pane of that session, and will not attach, as
/// when luneta itself runs in a pane. Its CLI would take the session and the
/// pane they name for the ones it is asked from. The window ids are of the
/// terminal luneta's own is made by, and name no window.
pub const LEFT_OUT: [&str; 5] =
    ["ZELLIJ", SESSION_VAR, PANE_VAR, "WINDOWID", "ALACRITTY_WINDOW_ID"];

/// How long zellij has to open a pane or a tab, or to list a session's
/// panes, or the sessions: a server that runs answers at once.
pub const ANSWER: Duration = Duration::from_secs(2);

/// Has `command` run without what `LEFT_OUT` names, and with nothing to read.
pub fn quiet(command: &mut Command) {
    for name in LEFT_OUT {
        command.env_remove(name);
    }
    command.stdin(Stdio::null());
}

/// What `command`, a zellij of `quiet`'s, said on its stdout as it did what
/// it was asked, or why it did not.
pub fn answered(command: &mut Command) -> Result<String, String> {
    let ran = program::run(command, ANSWER).map_err(|error| unrun(&error))?;
    let said = ran.answer().map_err(|why| format!("zellij: {why}"))?;
    String::from_utf8(said).map_err(|_| "zellij said what is not UTF-8".to_owned())
}

/// Has `command`, a zellij, do what it was asked, and says why not when it
/// did not.
pub fn done(mut command: Command) -> Result<(), String> {
    quiet(&mut command);
    answered(&mut command).map(drop)
}

/// What zellij said as it failed: the last line it wrote on stderr, and how
/// it ended.
#[must_use]
pub fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("");
    format!("zellij: {} ({})", line.trim(), output.status)
}

/// Why zellij could not be run.
#[must_use]
pub fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zellij is not on the PATH".to_owned();
    }
    format!("zellij: {error}")
}

/// A pane as `zellij action list-panes --json` lists it, as much of it as
/// says what it holds open, whether a client coming there is on it, and what
/// runs in it.
#[derive(Clone, Deserialize)]
#[expect(clippy::struct_excessive_bools, reason = "zellij lists each as a bool of its own")]
pub struct Laid {
    pub id: u32,
    pub is_plugin: bool,
    pub is_selectable: bool,
    pub is_suppressed: bool,
    pub is_floating: bool,
    /// Whether a client has the focus on it in its tab, or the last one had.
    /// Only the preview of a session goes by it, and by `tab_position`: a
    /// list without them still says what closing a pane closes.
    #[serde(default)]
    pub is_focused: bool,
    pub tab_id: u32,
    /// Where its tab stands among the session's, from the first.
    #[serde(default)]
    pub tab_position: u32,
    /// What its tab is called, as you pick a tab for another's panes by it.
    #[serde(default)]
    pub tab_name: String,
    /// What runs in its foreground: none where zellij took too long to say,
    /// which does not mean nothing runs.
    pub pane_command: Option<String>,
}

impl Laid {
    /// Whether it is tiled in its tab, for you to go to: a plugin's as well.
    #[must_use]
    pub fn is_tiled(&self) -> bool {
        self.is_selectable && !self.is_suppressed && !self.is_floating
    }

    /// Whether it keeps its tab open: a tiled pane you can go to, a plugin's
    /// as well, or a terminal another took the place of, which zellij brings
    /// back once that one closes. A floating pane goes with the tab, and so
    /// does a bar, which you cannot go to.
    #[must_use]
    pub fn holds_tab(&self) -> bool {
        self.is_tiled() || (self.is_suppressed && !self.is_plugin)
    }
}

/// The panes of `session` as zellij lists them now, the plugins' you can go
/// to too: it leaves out the bars.
pub fn laid(session: &str) -> Result<Vec<Laid>, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    command.args(["--session", session, "action", "list-panes", "--json"]);
    let said = answered(&mut command)?;
    serde_json::from_str(&said)
        .map_err(|error| format!("zellij listed panes that do not read: {error}"))
}

/// The terminal of the tab whose id is `tab` among `laid`, the first it
/// lists: where luneta goes in a tab it opened.
#[must_use]
pub fn terminal_of(laid: &[Laid], tab: u32) -> Option<u32> {
    let terminal = |pane: &&Laid| !pane.is_plugin && pane.tab_id == tab;
    laid.iter().find(terminal).map(|pane| pane.id)
}

/// The ids of the tabs `laid` lists a pane of, a plugin's as well.
#[must_use]
pub fn tabs(laid: &[Laid]) -> BTreeSet<u32> {
    laid.iter().map(|pane| pane.tab_id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid(id: u32, tab_id: u32) -> Laid {
        Laid {
            id,
            is_plugin: false,
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            is_focused: false,
            tab_id,
            tab_position: tab_id,
            tab_name: String::new(),
            pane_command: None,
        }
    }

    /// luneta goes to the terminal of a tab it opened, of that tab.
    #[test]
    fn a_tab_opened_is_gone_to_on_its_terminal() {
        let panes = [laid(1, 0), laid(3, 1)];
        assert_eq!(terminal_of(&panes, 1), Some(3));
    }

    /// The tabs zellij made a session with, which a tab opened there closes,
    /// are every tab it lists, those that hold a plugin's pane alone as well.
    #[test]
    fn a_made_session_s_tabs_are_all_it_lists_a_plugin_s_too() {
        let bar = Laid { is_plugin: true, ..laid(0, 2) };
        let panes = [laid(0, 0), laid(1, 1), laid(2, 1), bar];
        assert_eq!(tabs(&panes), BTreeSet::from([0, 1, 2]));
    }
}
