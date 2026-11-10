//! zellij's CLI as luneta and its daemon run it, on the machine they run on:
//! with none of the variables of a pane it may run in, and nothing to read,
//! what it says as it answers or fails, and a session's panes as it lists
//! them.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde::Deserialize;

use crate::clients;
use crate::program::{self, Ran};
use crate::zellij::SessionName;

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

/// How long zellij has to answer: a server that runs answers at once.
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
    let said = answer_of(ran)?;
    String::from_utf8(said).map_err(|_| "zellij said what is not UTF-8".to_owned())
}

/// What zellij's CLI said on its stdout as it ended as `ran` says, or why not.
/// One can end well having asked nothing: where the session it was asked of did
/// not answer it and others did, it says so, lists them on its stdout, and ends
/// as that list does (`send_action_to_session` in zellij 0.45.1's `commands.rs`,
/// and `list_sessions`, which exits 0).
pub(crate) fn answer_of(ran: Ran) -> Result<Vec<u8>, String> {
    if let Ran::Ended { stderr, .. } = &ran {
        let stderr = String::from_utf8_lossy(stderr);
        if let Some(line) = stderr.lines().find(|line| line.contains(OTHERS_ANSWERED)) {
            return Err(format!("zellij: {}", line.trim()));
        }
    }
    ran.answer().map_err(|why| format!("zellij: {why}"))
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

/// How many times luneta asks zellij for what a pane closing can cut short.
const ASKS: usize = 3;

/// What zellij's CLI says where no session answered as it first reached the
/// sockets: something else answered the one it was asked of, and it has asked
/// nothing of it then.
const NONE_ANSWERED: &str = "There is no active session!";

/// What it says where others did, after the session's name.
const OTHERS_ANSWERED: &str = "not found. The following sessions are active:";

/// What `read` makes of what `ask`, a call of zellij's CLI that has something
/// to say each time, said, or why not; asked again where a pane closing cut it
/// short. zellij tells every connection to a session's server it may go on as a
/// pane closes (`ClosePane` in zellij-server 0.45.1's `pty.rs`), not only a
/// client's. A call that waits for its answer takes that for one, and ends well
/// with nothing said, though what it asked for goes on: a pane it asked to open
/// opens. What it would have said goes by its connection's id, which zellij
/// gives the next that connects, so that another call can hear it. Or it asked
/// nothing, as `unreached` says. So only what `read` takes is an answer. Measured
/// beside panes opening and closing, one call in six was cut short.
pub fn heard<T>(
    mut ask: impl FnMut() -> Result<String, String>,
    read: impl Fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    let mut last = String::new();
    for _ in 0..ASKS {
        match ask() {
            Ok(said) => match read(&said) {
                Ok(it) => return Ok(it),
                Err(unread) => last = unread,
            },
            Err(why) if unreached(&why) => last = why,
            Err(why) => return Err(why),
        }
    }
    Err(last)
}

/// Has `command`, a zellij that says nothing as it does what it was asked, do
/// it, as `done` does, asked again where a pane closing kept it from asking,
/// as `heard` says.
pub fn done_once_reached(mut command: Command) -> Result<(), String> {
    quiet(&mut command);
    heard(|| answered(&mut command), |_| Ok(()))
}

/// Whether `why` zellij's CLI failed says it asked nothing of the server, as it
/// says by how many other sessions answered. A session that is truly gone says
/// so too, and asked again says the same.
fn unreached(why: &str) -> bool {
    why.contains(NONE_ANSWERED) || why.contains(OTHERS_ANSWERED)
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
    /// Whether it is shown over the other panes of its tab. Only a turn goes by
    /// it, to show it so again, as a pane leaving the tab or coming into it ends
    /// that.
    #[serde(default)]
    pub is_fullscreen: bool,
    /// What its frame calls it, by which a turn finds a stand-in of its own.
    #[serde(default)]
    pub title: String,
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
    /// The pane it is, as zellij's CLI names it.
    #[must_use]
    pub fn pane_id(&self) -> PaneId {
        if self.is_plugin { PaneId::Plugin(self.id) } else { PaneId::Terminal(self.id) }
    }

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

/// A pane as zellij's CLI names it. It numbers terminals and plugins apart, so
/// that a session can have one of each by an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneId {
    Terminal(u32),
    Plugin(u32),
}

impl PaneId {
    /// The terminal it is, or none: it is a plugin's.
    #[must_use]
    pub fn terminal(self) -> Option<u32> {
        match self {
            PaneId::Terminal(id) => Some(id),
            PaneId::Plugin(_) => None,
        }
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaneId::Terminal(id) => write!(f, "terminal_{id}"),
            PaneId::Plugin(id) => write!(f, "plugin_{id}"),
        }
    }
}

/// The panes of `session` as zellij lists them now, the plugins' you can go
/// to too: it leaves out the bars.
pub fn laid(session: &str) -> Result<Vec<Laid>, String> {
    let mut command = action(session);
    quiet(&mut command);
    command.args(["list-panes", "--json"]);
    heard(
        || answered(&mut command),
        |said| {
            serde_json::from_str(said)
                .map_err(|error| format!("zellij listed panes that do not read: {error}"))
        },
    )
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

/// zellij's CLI.
fn zellij() -> Command {
    Command::new("zellij")
}

/// The option that has zellij's CLI ask `session`, joined to its name: apart,
/// it takes a name that starts with `-` for an option.
pub(crate) fn session_option(session: &str) -> String {
    format!("--session={session}")
}

/// zellij's `action` command on `session`.
fn action(session: &str) -> Command {
    let mut command = zellij();
    command.arg(session_option(session)).arg("action");
    command
}

/// How `command`, a zellij, ended within `ANSWER`, or why it could not be run.
fn ran(command: &mut Command) -> Result<Ran, String> {
    program::run(command, ANSWER).map_err(|error| format!("zellij could not be run: {error}"))
}

/// What `zellij action list-panes --all --json` said of `session`, or why
/// zellij did not list them.
pub fn panes_listed(session: &str) -> Result<Vec<u8>, String> {
    let mut command = action(session);
    command.args(["list-panes", "--all", "--json"]);
    ran(&mut command)?.answer().map_err(|why| format!("zellij: {why}"))
}

/// The clients of `session`, as zellij lists them now, or why not.
pub fn clients(session: &str) -> Result<clients::Listed, String> {
    let mut command = zellij();
    quiet(&mut command);
    clients::list(command, session)
}

/// What a pane or a tab luneta opens runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs<'a> {
    /// Your shell, as a command, so that it closes as it ends.
    Shell(&'a OsStr),
    /// zellij's own shell, as it opens one where it is given no command.
    ZellijShell,
    /// A command, as it is.
    Command(&'a [String]),
}

impl Runs<'_> {
    /// Has `command`, a zellij that opens a pane or a tab, run it.
    fn add_to(self, command: &mut Command) {
        match self {
            Runs::Shell(shell) => {
                assert!(!shell.is_empty());
                command.args(["--close-on-exit", "--"]).arg(shell);
            },
            Runs::ZellijShell => {},
            Runs::Command(runs) => {
                assert_ne!(runs.len(), 0);
                command.arg("--").args(runs);
            },
        }
    }
}

/// Has zellij open a pane `--no-focus` beside the pane `beside` of `session`,
/// in its tab, and in `cwd`, that runs `runs`, and says which terminal it
/// opened. zellij reads the pane it opens one beside in `PANE_VAR`.
pub fn pane_opened(
    session: &SessionName,
    beside: u32,
    cwd: &str,
    runs: Runs,
) -> Result<u32, String> {
    let mut command = action(session.as_str());
    quiet(&mut command);
    command.env(PANE_VAR, beside.to_string());
    command.args(["new-pane", "--no-focus", "--cwd", cwd]);
    runs.add_to(&mut command);
    terminal_said(&answered(&mut command)?)
}

/// Has zellij open a pane named `name` in the tab `tab` of `session`, with no
/// client moved, that runs `runs` and closes as it ends, and says which: as
/// it says it, or as `found` finds it where a pane closing cut its answer
/// short, as `heard` says. One that does not show is asked for again, and the
/// first opened then has a second beside it, which ends on its own.
pub fn pane_opened_once_heard(
    session: &SessionName,
    tab: u32,
    name: &str,
    runs: &[&str],
    found: impl Fn() -> Option<PaneId>,
) -> Result<PaneId, String> {
    assert_ne!(runs.len(), 0);
    let mut command = action(session.as_str());
    quiet(&mut command);
    command.args(["new-pane", "--tab-id", &tab.to_string(), "--no-focus", "--close-on-exit"]);
    command.arg(format!("--name={name}")).arg("--").args(runs);
    heard(
        || answered(&mut command),
        |said| terminal_said(said).map(PaneId::Terminal).or_else(|unnamed| found().ok_or(unnamed)),
    )
}

/// The terminal zellij `said` it opened a pane as, `terminal_<id>`, or why
/// there is none.
fn terminal_said(said: &str) -> Result<u32, String> {
    let pane = said.trim().strip_prefix("terminal_").and_then(|id| id.parse().ok());
    pane.ok_or_else(|| format!("zellij opened no pane it named: {:?}", said.trim()))
}

/// Has zellij open a tab `--no-focus` in `session`, in `cwd`, named `name` or
/// as zellij numbers it, that runs `runs`, and says its id.
pub fn tab_opened(
    session: &SessionName,
    cwd: &str,
    name: Option<&str>,
    runs: Runs,
) -> Result<u32, String> {
    let mut command = action(session.as_str());
    quiet(&mut command);
    command.args(["new-tab", "--no-focus", "--cwd", cwd]);
    command.args(name.into_iter().flat_map(|name| ["--name", name]));
    runs.add_to(&mut command);
    tab_said(&answered(&mut command)?)
}

/// Has zellij open a tab in `session`, named `name` or as zellij numbers it,
/// with no client moved, that runs `runs` and closes as it ends, and says its
/// id. One zellij opened as a pane closing cut its answer short has a second
/// open beside it, as `heard` says, and ends on its own. The name goes joined
/// to its option: apart, zellij's CLI takes one that starts with `-` for an
/// option too.
pub fn tab_opened_once_heard(
    session: &SessionName,
    name: Option<&str>,
    runs: &[&str],
) -> Result<u32, String> {
    assert_ne!(runs.len(), 0);
    let mut command = action(session.as_str());
    quiet(&mut command);
    command.args(["new-tab", "--no-focus", "--close-on-exit"]);
    command.args(name.map(|name| format!("--name={name}")));
    command.arg("--").args(runs);
    heard(|| answered(&mut command), tab_said)
}

/// The tab zellij `said` it opened, by its id, or why there is none.
fn tab_said(said: &str) -> Result<u32, String> {
    let tab = said.trim().parse();
    tab.map_err(|_| format!("zellij opened no tab it named: {:?}", said.trim()))
}

/// How often luneta asks zellij for what it says nothing of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asks {
    /// Once, as `done` does.
    Once,
    /// Again where a pane closing kept it from asking, as `done_once_reached`
    /// does.
    OnceReached,
}

/// Has zellij do what `command` asks, as often as `asks` says.
fn asked(command: Command, asks: Asks) -> Result<(), String> {
    match asks {
        Asks::Once => done(command),
        Asks::OnceReached => done_once_reached(command),
    }
}

/// Has zellij close `pane` of `session`.
pub fn pane_closed(session: &SessionName, pane: PaneId, asks: Asks) -> Result<(), String> {
    let mut command = action(session.as_str());
    command.args(["close-pane", "--pane-id", &pane.to_string()]);
    asked(command, asks)
}

/// Has zellij close the tab whose id is `tab` in `session`.
pub fn tab_closed(session: &SessionName, tab: u32, asks: Asks) -> Result<(), String> {
    let mut command = action(session.as_str());
    command.args(["close-tab-by-id", &tab.to_string()]);
    asked(command, asks)
}

/// Has zellij show `pane` of `session` over the other panes of its tab, or
/// no longer, as `done_once_reached` does.
pub fn fullscreen_toggled(session: &SessionName, pane: PaneId) -> Result<(), String> {
    let mut command = action(session.as_str());
    command.args(["toggle-fullscreen", "--pane-id", &pane.to_string()]);
    done_once_reached(command)
}

/// Has zellij stack `panes` of `session` on the first of them, each from
/// whatever tab it is in, as `done_once_reached` does. It stacks nothing
/// where it has no room, and says nothing of it.
pub fn panes_stacked(session: &SessionName, panes: &[PaneId]) -> Result<(), String> {
    assert!(panes.len() >= 2);
    let mut command = action(session.as_str());
    command.args(["stack-panes", "--"]).args(panes.iter().map(ToString::to_string));
    done_once_reached(command)
}

/// Has zellij rename `session` to `to`, or says why it did not.
pub fn session_renamed(session: &SessionName, to: &SessionName) -> Result<(), String> {
    let mut command = action(session.as_str());
    command.args(["rename-session", "--", to.as_str()]);
    done(command)
}

/// The screen of the terminal `pane` of `session`: its lines, in the colours
/// it shows them in, as zellij dumps them; or why zellij did not.
pub fn screen_dumped(session: &SessionName, pane: u32) -> Result<String, String> {
    let mut command = action(session.as_str());
    quiet(&mut command);
    let terminal = PaneId::Terminal(pane).to_string();
    command.args(["dump-screen", "--pane-id", &terminal, "--ansi"]);
    answered(&mut command)
}

/// The text of the terminal `pane` of `session`, its scrollback with it, as
/// zellij dumps it: a line for each line written to it, however many rows it
/// wraps over, with no styles and no spaces at its end.
pub fn text_dumped(session: &SessionName, pane: u32) -> Result<String, String> {
    let mut command = action(session.as_str());
    quiet(&mut command);
    command.args(["dump-screen", "--full", "--pane-id"]).arg(PaneId::Terminal(pane).to_string());
    answered(&mut command)
}

/// What `list-sessions` says where no session runs, nor ended to be brought
/// back, as it fails.
const NO_SESSIONS: &str = "No active zellij sessions found.";

/// What `zellij list-sessions --no-formatting` says, a line for each session,
/// or nothing where there are none, or why it did not say, as in time:
/// zellij's CLI waits on every session's server, and one that hangs would
/// hold it up for good.
pub fn sessions_listed() -> Result<String, String> {
    let mut command = zellij();
    quiet(&mut command);
    command.args(["list-sessions", "--no-formatting"]);
    let ran = program::run(&mut command, ANSWER).map_err(|error| unrun(&error))?;
    if let Ran::Ended { stderr, .. } = &ran
        && String::from_utf8_lossy(stderr).contains(NO_SESSIONS)
    {
        return Ok(String::new());
    }
    let listed = ran.answer().map_err(|why| format!("zellij: {why}"))?;
    Ok(String::from_utf8_lossy(&listed).into_owned())
}

/// What `delete-session --force` says as it kills a session that runs:
/// `assert_dead_session` in zellij-utils 0.45.1's `sessions.rs` says it, on
/// stdout, of one it finds a server of.
const KILLING: &str = "will be force killed and deleted";

/// How `delete-session` ends where zellij keeps nothing of the session:
/// `delete_session` in the same file ends so where it finds no folder of the
/// session to take away.
const UNKEPT: i32 = 2;

/// Has zellij kill `session` and delete what it keeps of it, or says why it
/// did not: where it kept nothing yet, it only kills it.
pub fn session_killed(session: &SessionName) -> Result<(), String> {
    let mut command = zellij();
    quiet(&mut command);
    command.args(["delete-session", "--force", "--", session.as_str()]);
    let ran = program::run(&mut command, ANSWER).map_err(|error| unrun(&error))?;
    let unkept = matches!(&ran, Ran::Ended { status, stdout, .. }
        if status.code() == Some(UNKEPT) && String::from_utf8_lossy(stdout).contains(KILLING));
    if !unkept {
        ran.answer().map_err(|why| format!("zellij: {why}"))?;
    }
    Ok(())
}

/// Has zellij delete what it keeps of `session`, or says why it did not.
pub fn session_deleted(session: &SessionName) -> Result<(), String> {
    let mut command = zellij();
    command.args(["delete-session", "--", session.as_str()]);
    done(command)
}

/// What zellij says as it fails to focus a pane that has the focus already,
/// which is where the client was to go, and no failure.
pub const FOCUSED: &str = "is already focused";

/// `zellij`, a command of zellij's set up as its caller needs, with the
/// action that takes the client that last typed in the session `at` to the
/// pane `pane` of `to`, or to `to` alone, on the pane it last had the focus
/// on. None where the client is there already: in `at`, with no pane to go
/// to.
#[must_use]
pub fn go(
    mut zellij: Command,
    at: &SessionName,
    to: &SessionName,
    pane: Option<u32>,
) -> Option<Command> {
    let pane = pane.map(|id| format!("terminal_{id}"));
    let (at, to) = (at.as_str(), to.as_str());
    match pane {
        Some(pane) if at == to => {
            zellij.arg(session_option(at)).args(["action", "focus-pane-id", &pane]);
        },
        None if at == to => return None,
        pane => {
            zellij.arg(session_option(at)).args(["action", "switch-session"]);
            zellij.args(pane.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
            zellij.args(["--", to]);
        },
    }
    Some(zellij)
}

/// Has zellij take the action `go` makes, or says why it did not, as zellij
/// said it.
pub fn went(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Result<(), String> {
    let Some(mut command) = go(zellij(), at, to, pane) else {
        return Ok(());
    };
    match ran(&mut command)?.answer() {
        Ok(_) => Ok(()),
        Err(said) if said.contains(FOCUSED) => Ok(()),
        Err(said) => Err(format!("zellij: {said}")),
    }
}

/// `zellij`, a command of zellij's set up as its caller needs, with the
/// action that takes the client that last typed in the session `at` to a
/// session it makes, `to`, that starts in `cwd`, or where the client started.
/// zellij takes it to a session that runs by that name instead, and makes
/// none, and brings back one of that name it keeps, which ended: the
/// directory goes unused then. zellij's client makes it, so that it has the
/// client's environment, not the CLI's.
#[must_use]
pub fn start(
    mut zellij: Command,
    at: &SessionName,
    to: &SessionName,
    cwd: Option<&str>,
) -> Command {
    zellij.arg(session_option(at.as_str())).args(["action", "switch-session"]);
    zellij.args(cwd.iter().flat_map(|cwd| ["--cwd", cwd]));
    zellij.args(["--", to.as_str()]);
    zellij
}

/// Has zellij take the action `start` makes, or says why it did not, as
/// zellij said it.
pub fn started(at: &SessionName, to: &SessionName, cwd: Option<&str>) -> Result<(), String> {
    let mut command = start(zellij(), at, to, cwd);
    ran(&mut command)?.answer().map(drop).map_err(|said| format!("zellij: {said}"))
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
            is_fullscreen: false,
            title: String::new(),
            tab_id,
            tab_position: tab_id,
            tab_name: String::new(),
            pane_command: None,
        }
    }

    /// A session whose name starts with `-` goes to zellij as a name: joined to
    /// its option, or after `--`. Apart, zellij's CLI takes it for an option.
    #[test]
    fn a_session_name_that_starts_with_a_dash_is_no_option() {
        let args = |command: &Command| {
            let args = command.get_args().map(|arg| arg.to_string_lossy().into_owned());
            args.collect::<Vec<_>>()
        };
        let at = SessionName::running("-at").unwrap();
        let to = SessionName::running("-to").unwrap();
        let switch = ["--session=-at", "action", "switch-session"];
        let went = go(zellij(), &at, &to, Some(2)).unwrap();
        assert_eq!(args(&went), [&switch[..], &["--pane-id", "terminal_2", "--", "-to"]].concat());
        let started = start(zellij(), &at, &to, Some("/w"));
        assert_eq!(args(&started), [&switch[..], &["--cwd", "/w", "--", "-to"]].concat());
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
