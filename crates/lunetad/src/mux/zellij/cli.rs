//! zellij's CLI as luneta and its daemon run it, on the machine they run on:
//! with none of the variables of a pane it may run in, and nothing to read,
//! what it says as it answers or fails, and a session's panes and clients as
//! it lists them; and the words that start its client.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

use super::vars::{PANE_VAR, PANE_VARS};
use super::SessionName;
use crate::mux::{ClientWords, Laid, PaneId, TabId};
use crate::program::{self, Ran};
use crate::Directory;

mod clients;

pub use clients::{Client, Listed};

/// Variables that zellij's client must not have: those of `PANE_VARS`, and
/// the window ids of the terminal luneta's own is made by, which name no
/// window. With `ZELLIJ` set, the client takes itself for one started in a
/// pane of that session, and will not attach, as when luneta itself runs in
/// a pane. Its CLI would take the session and the pane they name for the ones
/// it is asked from. `PANE_VARS` is taken apart whole, so that one more of
/// them will not build until it is left out too.
pub const LEFT_OUT: [&str; 5] = {
    let [in_a_pane, session, pane] = PANE_VARS;
    [in_a_pane, session, pane, "WINDOWID", "ALACRITTY_WINDOW_ID"]
};

/// How long zellij has to answer: a server that runs answers at once.
pub const ANSWER: Duration = Duration::from_secs(2);

/// Has `command` run without what `LEFT_OUT` names, and with nothing to read.
fn quiet(command: &mut Command) {
    for name in LEFT_OUT {
        command.env_remove(name);
    }
    command.stdin(Stdio::null());
}

/// Why zellij's CLI did not do what it was asked, as it said it.
#[derive(Debug)]
pub enum Failure {
    /// It could not be run.
    Unrun(io::Error),
    /// It had not answered within its time, and was killed.
    Stuck,
    /// It asked nothing of the server, as it says by how many other sessions
    /// answered. A session that is truly gone says so too, and asked again
    /// says the same.
    Unreached(String),
    /// The pane it was to focus has the focus already, which is where the
    /// client was to go.
    Focused(String),
    /// It failed, as it said, or as it ended.
    Refused(String),
    /// It answered what does not read, for the reason it holds.
    Unread(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Unrun(error) => f.write_str(&unrun(error)),
            Failure::Stuck => f.write_str("zellij: it did not answer in time"),
            Failure::Unreached(said) | Failure::Focused(said) | Failure::Refused(said) => {
                write!(f, "zellij: {said}")
            },
            Failure::Unread(why) => f.write_str(why),
        }
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.to_string()
    }
}

/// What `command`, a zellij, said on its stdout as it did what it was asked,
/// or why it did not.
pub fn answered(command: &mut Command) -> Result<String, Failure> {
    let said = answer_of(ran(command)?)?;
    let unread = |_| Failure::Unread("zellij said what is not UTF-8".to_owned());
    String::from_utf8(said).map_err(unread)
}

/// What zellij's CLI said on its stdout as it ended as `ran` says, or why not.
/// One can end well having asked nothing: where the session it was asked of did
/// not answer it and others did, it says so, lists them on its stdout, and ends
/// as that list does (`send_action_to_session` in zellij 0.45.1's `commands.rs`,
/// and `list_sessions`, which exits 0).
fn answer_of(outcome: Ran) -> Result<Vec<u8>, Failure> {
    let (status, stdout, stderr) = match outcome {
        Ran::Ended { status, stdout, stderr } => (status, stdout, stderr),
        Ran::Stuck => return Err(Failure::Stuck),
    };
    let stderr = String::from_utf8_lossy(&stderr);
    let lines = || stderr.lines().map(str::trim).filter(|line| !line.is_empty());
    if let Some(line) = lines().find(|line| line.contains(OTHERS_ANSWERED)) {
        return Err(Failure::Unreached(line.to_owned()));
    }
    if status.success() {
        return Ok(stdout);
    }
    let said = lines().next().map_or_else(|| status.to_string(), str::to_owned);
    if lines().any(|line| line.contains(NONE_ANSWERED)) {
        return Err(Failure::Unreached(said));
    }
    if lines().any(|line| line.contains(FOCUSED)) {
        return Err(Failure::Focused(said));
    }
    Err(Failure::Refused(said))
}

/// Has `command`, a zellij, do what it was asked, and says why not when it
/// did not.
fn done(mut command: Command) -> Result<(), Failure> {
    quiet(&mut command);
    answered(&mut command).map(drop)
}

/// How many times luneta asks zellij for what a pane closing can cut short.
const ASKS: usize = 3;

/// What zellij's CLI says where no session answered as it first reached the
/// sockets: something else answered the one it was asked of, and it has asked
/// nothing of it then.
const NONE_ANSWERED: &str = "There is no active session!";

/// What it says where others did, after the session's name.
const OTHERS_ANSWERED: &str = "not found. The following sessions are active:";

/// What zellij says as it fails to focus a pane that has the focus already.
const FOCUSED: &str = "is already focused";

/// What `read` makes of what `ask`, a call of zellij's CLI that has something
/// to say each time, said, or why not; asked again where a pane closing cut it
/// short. zellij tells every connection to a session's server it may go on as a
/// pane closes (`ClosePane` in zellij-server 0.45.1's `pty.rs`), not only a
/// client's. A call that waits for its answer takes that for one, and ends well
/// with nothing said, though what it asked for goes on: a pane it asked to open
/// opens. What it would have said goes by its connection's id, which zellij
/// gives the next that connects, so that another call can hear it. Or it asked
/// nothing, as `Failure::Unreached` says. So only what `read` takes is an
/// answer. Measured beside panes opening and closing, one call in six was cut
/// short.
pub fn heard<T>(
    mut ask: impl FnMut() -> Result<String, Failure>,
    read: impl Fn(&str) -> Result<T, String>,
) -> Result<T, Failure> {
    let mut once = || read(&ask()?).map_err(Failure::Unread);
    for _ in 1..ASKS {
        match once() {
            Err(Failure::Unread(_) | Failure::Unreached(_)) => {},
            done => return done,
        }
    }
    once()
}

/// Has `command`, a zellij that says nothing as it does what it was asked, do
/// it, as `done` does, asked again where a pane closing kept it from asking,
/// as `heard` says.
fn done_once_reached(mut command: Command) -> Result<(), Failure> {
    quiet(&mut command);
    heard(|| answered(&mut command), |_| Ok(()))
}

/// Why zellij could not be run.
#[must_use]
pub fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zellij is not on the PATH".to_owned();
    }
    format!("zellij could not be run: {error}")
}

/// A pane as zellij 0.45 lists it with `action list-panes --json`, with
/// `--all` or without. zellij leaves out where a pane works and what runs in
/// it when it does not know, and writes the rest every time: a listing
/// without one does not read, where a default would have a plugin's pane read
/// as a terminal in the first tab.
#[derive(Deserialize)]
#[expect(clippy::struct_excessive_bools, reason = "zellij lists each as a bool of its own")]
struct Wire {
    id: u32,
    is_plugin: bool,
    is_selectable: bool,
    is_suppressed: bool,
    is_floating: bool,
    is_focused: bool,
    is_fullscreen: bool,
    is_held: bool,
    title: String,
    tab_id: u32,
    tab_position: u32,
    tab_name: String,
    #[serde(default)]
    pane_cwd: Option<String>,
    #[serde(default)]
    pane_command: Option<String>,
}

impl Wire {
    fn laid(self) -> Laid {
        Laid {
            pane: if self.is_plugin { PaneId::Plugin(self.id) } else { PaneId::Terminal(self.id) },
            is_selectable: self.is_selectable,
            is_suppressed: self.is_suppressed,
            is_floating: self.is_floating,
            is_focused: self.is_focused,
            is_fullscreen: self.is_fullscreen,
            is_held: self.is_held,
            title: self.title,
            tab_id: TabId(self.tab_id),
            tab_position: self.tab_position,
            tab_name: self.tab_name,
            cwd: self.pane_cwd,
            command: self.pane_command,
        }
    }
}

/// The panes in what zellij listed, or why it does not read.
fn listed(said: &[u8]) -> Result<Vec<Laid>, serde_json::Error> {
    let wire: Vec<Wire> = serde_json::from_slice(said)?;
    Ok(wire.into_iter().map(Wire::laid).collect())
}

/// The panes of `session` as zellij lists them now, the plugins' you can go
/// to too: it leaves out the bars. Asked again as `heard` says.
pub fn laid(session: &SessionName) -> Result<Vec<Laid>, Failure> {
    let mut command = action(session);
    command.args(["list-panes", "--json"]);
    heard(
        || answered(&mut command),
        |said| {
            listed(said.as_bytes())
                .map_err(|error| format!("zellij listed panes that do not read: {error}"))
        },
    )
}

/// zellij's CLI, as `quiet` has it run.
fn zellij() -> Command {
    let mut command = Command::new(PROGRAM);
    quiet(&mut command);
    command
}

/// The option `name` with `value` joined to it: apart, zellij's CLI takes a
/// value that starts with `-` for an option.
fn option(name: &str, value: impl fmt::Display) -> String {
    format!("{name}={value}")
}

/// The option that has zellij's CLI ask `session`.
fn session_option(session: &SessionName) -> String {
    option("--session", session.as_str())
}

/// zellij's `action` command on `session`.
fn action(session: &SessionName) -> Command {
    let mut command = zellij();
    command.arg(session_option(session)).arg("action");
    command
}

/// How `command`, a zellij, ended within `ANSWER`, or why it could not be run.
fn ran(command: &mut Command) -> Result<Ran, Failure> {
    program::run(command, ANSWER).map_err(Failure::Unrun)
}

/// Every pane of `session` as zellij lists them now, the bars too, asked
/// once, or why zellij did not list them.
pub fn panes_listed(session: &SessionName) -> Result<Vec<Laid>, Failure> {
    let mut command = action(session);
    command.args(["list-panes", "--all", "--json"]);
    let said = answer_of(ran(&mut command)?)?;
    let unread = |error| Failure::Unread(format!("zellij listed what does not read: {error}"));
    listed(&said).map_err(unread)
}

/// What zellij says now as it lists the clients of `session`, asked again as
/// `heard` says.
fn clients_said(session: &SessionName) -> Result<clients::Said, Failure> {
    let mut command = action(session);
    command.arg("list-clients");
    heard(|| answered(&mut command), clients::said)
}

/// The clients of `session`, as zellij lists them now, or why not. Where it
/// lists two rows or more, zellij is asked for the panes too, as `laid` asks,
/// which say what a row a command quotes is: the list fails as they do, and
/// zellij is not asked for the clients again.
pub fn clients(session: &SessionName) -> Result<Listed, Failure> {
    clients_said(session)?.clients(session)
}

/// Whether a client is attached to `session`, as zellij lists them now, or
/// why not. The first row zellij lists is a client's, whatever the rows after
/// it quote, and zellij is asked for no panes.
pub fn has_clients(session: &SessionName) -> Result<bool, Failure> {
    Ok(clients_said(session)?.any())
}

/// Every row zellij lists now of the clients of `session`: each client, and
/// each row a command of many lines quotes, which `clients` skips. Or why
/// zellij did not list them. As many as its clients, or more, and zellij is
/// asked for no panes: of a client that comes to the session, none of these
/// is the one that came.
pub fn client_rows(session: &SessionName) -> Result<Vec<Client>, Failure> {
    Ok(clients_said(session)?.rows())
}

/// What a pane or a tab luneta opens runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs<'a> {
    /// Your shell, as a command, so that it closes as it ends.
    Shell(&'a OsStr),
    /// zellij's own shell, as it opens one where it is given no command.
    ZellijShell,
    /// A command, as it is: `program`, with `args`.
    Command { program: &'a str, args: &'a [String] },
}

impl<'a> Runs<'a> {
    /// `command`, as it is, or `otherwise` where it is empty.
    #[must_use]
    pub fn of(command: &'a [String], otherwise: Runs<'a>) -> Runs<'a> {
        match command.split_first() {
            Some((program, args)) => Runs::Command { program, args },
            None => otherwise,
        }
    }

    /// Has `command`, a zellij that opens a pane or a tab, run it.
    fn add_to(self, command: &mut Command) {
        match self {
            Runs::Shell(shell) => {
                assert!(!shell.is_empty());
                command.args(["--close-on-exit", "--"]).arg(shell);
            },
            Runs::ZellijShell => {},
            Runs::Command { program, args } => {
                command.args(["--", program]).args(args);
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
    cwd: &Directory,
    runs: Runs,
) -> Result<u32, Failure> {
    let mut command = action(session);
    command.env(PANE_VAR, beside.to_string());
    command.args(["new-pane", "--no-focus"]).arg(option("--cwd", cwd));
    runs.add_to(&mut command);
    terminal_said(&answered(&mut command)?).map_err(Failure::Unread)
}

/// Has zellij open a pane named `name` in the tab `tab` of `session`, with no
/// client moved, that runs `runs` and closes as it ends, and says which: as
/// it says it, or as `found` finds it where a pane closing cut its answer
/// short, as `heard` says. One that does not show is asked for again, and the
/// first opened then has a second beside it, which ends on its own.
pub fn pane_opened_once_heard<const N: usize>(
    session: &SessionName,
    tab: TabId,
    name: &str,
    runs: &[&str; N],
    found: impl Fn() -> Option<u32>,
) -> Result<u32, Failure> {
    const { assert!(N > 0) };
    let mut command = action(session);
    command.args(["new-pane", "--tab-id", &tab.to_string(), "--no-focus", "--close-on-exit"]);
    command.arg(option("--name", name)).arg("--").args(runs);
    heard(
        || answered(&mut command),
        |said| terminal_said(said).or_else(|unnamed| found().ok_or(unnamed)),
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
    cwd: &Directory,
    name: Option<&str>,
    runs: Runs,
) -> Result<TabId, Failure> {
    let mut command = action(session);
    command.args(["new-tab", "--no-focus"]).arg(option("--cwd", cwd));
    command.args(name.map(|name| option("--name", name)));
    runs.add_to(&mut command);
    tab_said(&answered(&mut command)?).map_err(Failure::Unread)
}

/// Has zellij open a tab in `session`, named `name` or as zellij numbers it,
/// with no client moved, that runs `runs` and closes as it ends, and says its
/// id. One zellij opened as a pane closing cut its answer short has a second
/// open beside it, as `heard` says, and ends on its own.
pub fn tab_opened_once_heard<const N: usize>(
    session: &SessionName,
    name: Option<&str>,
    runs: &[&str; N],
) -> Result<TabId, Failure> {
    const { assert!(N > 0) };
    let mut command = action(session);
    command.args(["new-tab", "--no-focus", "--close-on-exit"]);
    command.args(name.map(|name| option("--name", name)));
    command.arg("--").args(runs);
    heard(|| answered(&mut command), tab_said)
}

/// The tab zellij `said` it opened, by its id, or why there is none.
fn tab_said(said: &str) -> Result<TabId, String> {
    let tab = said.trim().parse().map(TabId);
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
fn asked(command: Command, asks: Asks) -> Result<(), Failure> {
    match asks {
        Asks::Once => done(command),
        Asks::OnceReached => done_once_reached(command),
    }
}

/// Has zellij close `pane` of `session`.
pub fn pane_closed(session: &SessionName, pane: PaneId, asks: Asks) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["close-pane", "--pane-id", &pane.to_string()]);
    asked(command, asks)
}

/// Has zellij close the tab whose id is `tab` in `session`.
pub fn tab_closed(session: &SessionName, tab: TabId, asks: Asks) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["close-tab-by-id", &tab.to_string()]);
    asked(command, asks)
}

/// Has zellij show `pane` of `session` over the other panes of its tab, or
/// no longer, as `done_once_reached` does.
pub fn fullscreen_toggled(session: &SessionName, pane: PaneId) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["toggle-fullscreen", "--pane-id", &pane.to_string()]);
    done_once_reached(command)
}

/// Has zellij stack `panes` of `session` on the first of them, each from
/// whatever tab it is in, as `done_once_reached` does. It stacks nothing
/// where it has no room, and says nothing of it.
pub fn panes_stacked<const N: usize>(
    session: &SessionName,
    panes: [PaneId; N],
) -> Result<(), Failure> {
    const { assert!(N >= 2) };
    let mut command = action(session);
    command.args(["stack-panes", "--"]).args(panes.iter().map(ToString::to_string));
    done_once_reached(command)
}

/// Has zellij rename `session` to `to`, or says why it did not.
pub fn session_renamed(session: &SessionName, to: &SessionName) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["rename-session", "--", to.as_str()]);
    done(command)
}

/// The screen of the terminal `pane` of `session`: its lines, in the colours
/// it shows them in, as zellij dumps them; or why zellij did not.
pub fn screen_dumped(session: &SessionName, pane: u32) -> Result<String, Failure> {
    let mut command = action(session);
    let terminal = PaneId::Terminal(pane).to_string();
    command.args(["dump-screen", "--pane-id", &terminal, "--ansi"]);
    answered(&mut command)
}

/// The text of the terminal `pane` of `session`, its scrollback with it, as
/// zellij dumps it: a line for each line written to it, however many rows it
/// wraps over, with no styles and no spaces at its end.
pub fn text_dumped(session: &SessionName, pane: u32) -> Result<String, Failure> {
    let mut command = action(session);
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
pub fn sessions_listed() -> Result<String, Failure> {
    let mut command = zellij();
    command.args(["list-sessions", "--no-formatting"]);
    let outcome = ran(&mut command)?;
    if let Ran::Ended { stderr, .. } = &outcome
        && String::from_utf8_lossy(stderr).contains(NO_SESSIONS)
    {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&answer_of(outcome)?).into_owned())
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
pub fn session_killed(session: &SessionName) -> Result<(), Failure> {
    let mut command = zellij();
    command.args(["delete-session", "--force", "--", session.as_str()]);
    let outcome = ran(&mut command)?;
    let unkept = matches!(&outcome, Ran::Ended { status, stdout, .. }
        if status.code() == Some(UNKEPT) && String::from_utf8_lossy(stdout).contains(KILLING));
    if !unkept {
        answer_of(outcome)?;
    }
    Ok(())
}

/// Has zellij delete what it keeps of `session`, or says why it did not.
pub fn session_deleted(session: &SessionName) -> Result<(), Failure> {
    let mut command = zellij();
    command.args(["delete-session", "--", session.as_str()]);
    done(command)
}

/// The command that takes the client that last typed in the session `at` to
/// the pane `pane` of `to`, another session, or to `to` alone, on the pane it
/// last had the focus on.
fn go(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Command {
    assert_ne!(at, to);
    let pane = pane.map(|id| PaneId::Terminal(id).to_string());
    let mut command = action(at);
    command.arg("switch-session");
    command.args(pane.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
    command.args(["--", to.as_str()]);
    command
}

/// The command that takes the client that last typed in `session` to its
/// pane `pane`.
fn focus(session: &SessionName, pane: PaneId) -> Command {
    let mut command = action(session);
    command.args(["focus-pane-id", &pane.to_string()]);
    command
}

/// Has zellij take the client that last typed in `session` to its pane
/// `pane`, a terminal's or a plugin's, as `done_once_reached` does, or says
/// why it did not. A pane already focused is where it was to go.
pub fn pane_focused(session: &SessionName, pane: PaneId) -> Result<(), Failure> {
    match done_once_reached(focus(session, pane)) {
        Ok(()) | Err(Failure::Focused(_)) => Ok(()),
        Err(failure) => Err(failure),
    }
}

/// Has zellij take the client that last typed in `at` where `go` says, or
/// says why it did not. In `at` itself, it goes to the pane as
/// `pane_focused` does, and with no pane to go to, it is there already. A
/// pane already focused is where it was to go.
pub fn went(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Result<(), Failure> {
    if at == to {
        return pane.map_or(Ok(()), |pane| pane_focused(at, PaneId::Terminal(pane)));
    }
    match answer_of(ran(&mut go(at, to, pane))?) {
        Ok(_) | Err(Failure::Focused(_)) => Ok(()),
        Err(failure) => Err(failure),
    }
}

/// The command that takes the client that last typed in the session `at` to
/// a session it makes, `to`, that starts in `cwd`, or where the client
/// started. zellij takes it to a session that runs by that name instead, and
/// makes none, and brings back one of that name it keeps, which ended: the
/// directory goes unused then. zellij's client makes it, so that it has the
/// client's environment, not the CLI's.
fn start(at: &SessionName, to: &SessionName, cwd: Option<&Directory>) -> Command {
    let mut command = action(at);
    command.arg("switch-session");
    command.args(cwd.map(|cwd| option("--cwd", cwd)));
    command.args(["--", to.as_str()]);
    command
}

/// Has zellij take the client where `start` says, or says why it did not.
pub fn started(at: &SessionName, to: &SessionName, cwd: Option<&Directory>) -> Result<(), Failure> {
    answer_of(ran(&mut start(at, to, cwd))?).map(drop)
}

/// The program of zellij's CLI and its client.
const PROGRAM: &str = "zellij";

/// The words that start zellij's client with `args`, as `client_attaching` and
/// those after it give them, without what `LEFT_OUT` names.
#[must_use]
pub fn words(args: Vec<String>) -> ClientWords {
    ClientWords { program: PROGRAM, args, unset: &LEFT_OUT }
}

/// The arguments of zellij's client, after `PROGRAM`: attached to `session`,
/// which it makes where none runs, or without one, in a session of a name
/// zellij makes up; or why it cannot attach, as `attached` says.
pub fn client_attaching(session: Option<&SessionName>) -> Result<Vec<String>, String> {
    let Some(session) = session else {
        return Ok(Vec::new());
    };
    Ok(vec!["attach".to_owned(), "--create".to_owned(), attached(session)?])
}

/// The arguments of zellij's client, as `client_attaching` says, that makes
/// `session` in the directory it starts in, and fails where one runs by that
/// name, or zellij keeps one that ended, to bring back.
#[must_use]
pub fn client_making(session: &SessionName) -> Vec<String> {
    vec![session_option(session)]
}

/// The arguments of zellij's client, as `client_attaching` says, attached to
/// `session`, which runs, or which ended, which zellij brings back. It makes
/// none.
pub fn client_rejoining(session: &SessionName) -> Result<Vec<String>, String> {
    Ok(vec!["attach".to_owned(), attached(session)?])
}

/// Whether zellij's client attaches to `session`. `attach` takes its name
/// bare, as it takes the words after `--` for a command to run in a session
/// it makes, and a name that starts with `-` for an option, as `-c` for
/// `--create`: it attaches to none so named, which only zellij makes, as
/// `SessionName::parse` says.
#[must_use]
pub fn attaches_to(session: &SessionName) -> bool {
    !session.as_str().starts_with('-')
}

/// `session` as `attach` takes its name, or why it takes none, as
/// `attaches_to` says.
fn attached(session: &SessionName) -> Result<String, String> {
    if !attaches_to(session) {
        return Err(format!(
            "zellij attaches to no session whose name starts with '-', as \"{session}\"'s does: \
             rename it"
        ));
    }
    Ok(session.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;

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
        let went = go(&at, &to, Some(2));
        assert_eq!(args(&went), [&switch[..], &["--pane-id", "terminal_2", "--", "-to"]].concat());
        let w = Directory::parse("/w".to_owned()).unwrap();
        let started = start(&at, &to, Some(&w));
        assert_eq!(args(&started), [&switch[..], &["--cwd=/w", "--", "-to"]].concat());
        assert_eq!(client_making(&at), ["--session=-at"]);
        assert!(client_rejoining(&at).is_err());
        assert!(client_attaching(Some(&at)).is_err());
    }

    /// A pane already focused is read apart from any other failure, as `went`
    /// takes it for where the client was to go.
    #[test]
    fn a_pane_already_focused_is_no_refusal() {
        let failed = |stderr: &str| Ran::Ended {
            status: ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        let focused = answer_of(failed("Error: pane terminal_2 is already focused"));
        assert!(matches!(focused, Err(Failure::Focused(_))), "{focused:?}");
        let said = "Error: no pane terminal_2";
        let refused = answer_of(failed(said));
        assert!(matches!(&refused, Err(Failure::Refused(it)) if it == said), "{refused:?}");
    }

    /// zellij numbers terminals and plugins apart, so a plugin's pane listed
    /// by a terminal's id is no terminal.
    #[test]
    fn a_plugin_s_pane_is_read_apart_from_a_terminal_of_its_id() {
        let listed = |id: u32, is_plugin: bool| {
            serde_json::json!({
                "id": id, "is_plugin": is_plugin, "is_selectable": true,
                "is_suppressed": false, "is_floating": false, "is_focused": false,
                "is_fullscreen": false, "is_held": false, "title": "", "tab_id": 0,
                "tab_position": 0, "tab_name": "",
            })
        };
        let said = serde_json::json!([listed(2, true), listed(2, false)]).to_string();
        let laid = super::listed(said.as_bytes()).unwrap();
        let panes: Vec<PaneId> = laid.iter().map(|laid| laid.pane).collect();
        assert_eq!(panes, [PaneId::Plugin(2), PaneId::Terminal(2)]);
    }
}
