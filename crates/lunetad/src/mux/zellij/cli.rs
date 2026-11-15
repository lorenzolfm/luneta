use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

use luneta_protocol::said::Said;
use serde::Deserialize;

use super::vars::{PANE_VAR, PANE_VARS};
use super::SessionName;
use crate::mux::{ClientWords, Laid, PaneId, TabId};
use crate::program::{self, Ran};
use crate::Directory;

mod clients;

pub use clients::{Client, Listed};

pub const LEFT_OUT: [&str; 5] = {
    let [in_a_pane, session, pane] = PANE_VARS;
    [in_a_pane, session, pane, "WINDOWID", "ALACRITTY_WINDOW_ID"]
};

pub const ANSWER: Duration = Duration::from_secs(2);

fn quiet(command: &mut Command) {
    for name in LEFT_OUT {
        command.env_remove(name);
    }
    command.stdin(Stdio::null());
}

#[derive(Debug)]
pub enum Failure {
    Unrun(io::Error),
    Stuck,
    Unreached(String),
    Focused(String),
    Refused(String),
    Unread(String),
    Own(String),
    Taken(String),
    Unsupported(&'static str),
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
            Failure::Own(said) | Failure::Taken(said) => write!(f, "luneta-mux: {said}"),
            Failure::Unsupported(what) => {
                write!(f, "luneta's own sessions cannot {what} yet")
            },
        }
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.to_string()
    }
}

pub fn answered(command: &mut Command) -> Result<String, Failure> {
    let said = answer_of(ran(command)?)?;
    let unread = |_| Failure::Unread("zellij said what is not UTF-8".to_owned());
    String::from_utf8(said).map_err(unread)
}

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

fn done(mut command: Command) -> Result<(), Failure> {
    quiet(&mut command);
    answered(&mut command).map(drop)
}

const ASKS: usize = 3;

const NONE_ANSWERED: &str = "There is no active session!";

const OTHERS_ANSWERED: &str = "not found. The following sessions are active:";

const FOCUSED: &str = "is already focused";

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

fn done_once_reached(mut command: Command) -> Result<(), Failure> {
    quiet(&mut command);
    heard(|| answered(&mut command), |_| Ok(()))
}

#[must_use]
pub fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zellij is not on the PATH".to_owned();
    }
    format!("zellij could not be run: {error}")
}

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
            cwd: self.pane_cwd.and_then(Said::new),
            command: self.pane_command.and_then(Said::new),
        }
    }
}

fn listed(said: &[u8]) -> Result<Vec<Laid>, serde_json::Error> {
    let wire: Vec<Wire> = serde_json::from_slice(said)?;
    Ok(wire.into_iter().map(Wire::laid).collect())
}

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

fn zellij() -> Command {
    let mut command = Command::new(PROGRAM);
    quiet(&mut command);
    command
}

fn option(name: &str, value: impl fmt::Display) -> String {
    format!("{name}={value}")
}

fn session_option(session: &SessionName) -> String {
    option("--session", session.as_str())
}

fn action(session: &SessionName) -> Command {
    let mut command = zellij();
    command.arg(session_option(session)).arg("action");
    command
}

fn ran(command: &mut Command) -> Result<Ran, Failure> {
    program::run(command, ANSWER).map_err(Failure::Unrun)
}

pub fn panes_listed(session: &SessionName) -> Result<Vec<Laid>, Failure> {
    let mut command = action(session);
    command.args(["list-panes", "--all", "--json"]);
    let said = answer_of(ran(&mut command)?)?;
    let unread = |error| Failure::Unread(format!("zellij listed what does not read: {error}"));
    listed(&said).map_err(unread)
}

fn clients_said(session: &SessionName) -> Result<clients::Said, Failure> {
    let mut command = action(session);
    command.arg("list-clients");
    heard(|| answered(&mut command), clients::said)
}

pub fn clients(session: &SessionName) -> Result<Listed, Failure> {
    clients_said(session)?.clients(session)
}

pub fn has_clients(session: &SessionName) -> Result<bool, Failure> {
    Ok(clients_said(session)?.any())
}

pub fn client_rows(session: &SessionName) -> Result<Vec<Client>, Failure> {
    Ok(clients_said(session)?.rows())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs<'a> {
    Shell(&'a OsStr),
    ZellijShell,
    Command { program: &'a str, args: &'a [String] },
}

impl<'a> Runs<'a> {
    #[must_use]
    pub fn of(command: &'a [String], otherwise: Runs<'a>) -> Runs<'a> {
        match command.split_first() {
            Some((program, args)) => Runs::Command { program, args },
            None => otherwise,
        }
    }

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

fn terminal_said(said: &str) -> Result<u32, String> {
    let pane = said.trim().strip_prefix("terminal_").and_then(|id| id.parse().ok());
    pane.ok_or_else(|| format!("zellij opened no pane it named: {:?}", said.trim()))
}

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

fn tab_said(said: &str) -> Result<TabId, String> {
    let tab = said.trim().parse().map(TabId);
    tab.map_err(|_| format!("zellij opened no tab it named: {:?}", said.trim()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asks {
    Once,
    OnceReached,
}

fn asked(command: Command, asks: Asks) -> Result<(), Failure> {
    match asks {
        Asks::Once => done(command),
        Asks::OnceReached => done_once_reached(command),
    }
}

pub fn pane_closed(session: &SessionName, pane: PaneId, asks: Asks) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["close-pane", "--pane-id", &pane.to_string()]);
    asked(command, asks)
}

pub fn tab_closed(session: &SessionName, tab: TabId, asks: Asks) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["close-tab-by-id", &tab.to_string()]);
    asked(command, asks)
}

pub fn fullscreen_toggled(session: &SessionName, pane: PaneId) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["toggle-fullscreen", "--pane-id", &pane.to_string()]);
    done_once_reached(command)
}

pub fn panes_stacked<const N: usize>(
    session: &SessionName,
    panes: [PaneId; N],
) -> Result<(), Failure> {
    const { assert!(N >= 2) };
    let mut command = action(session);
    command.args(["stack-panes", "--"]).args(panes.iter().map(ToString::to_string));
    done_once_reached(command)
}

pub fn session_renamed(session: &SessionName, to: &SessionName) -> Result<(), Failure> {
    let mut command = action(session);
    command.args(["rename-session", "--", to.as_str()]);
    done(command)
}

pub fn screen_dumped(session: &SessionName, pane: u32) -> Result<String, Failure> {
    let mut command = action(session);
    let terminal = PaneId::Terminal(pane).to_string();
    command.args(["dump-screen", "--pane-id", &terminal, "--ansi"]);
    answered(&mut command)
}

pub fn text_dumped(session: &SessionName, pane: u32) -> Result<String, Failure> {
    let mut command = action(session);
    command.args(["dump-screen", "--full", "--pane-id"]).arg(PaneId::Terminal(pane).to_string());
    answered(&mut command)
}

const NO_SESSIONS: &str = "No active zellij sessions found.";

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

const KILLING: &str = "will be force killed and deleted";

const UNKEPT: i32 = 2;

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

pub fn session_deleted(session: &SessionName) -> Result<(), Failure> {
    let mut command = zellij();
    command.args(["delete-session", "--", session.as_str()]);
    done(command)
}

fn go(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Command {
    assert_ne!(at, to);
    let pane = pane.map(|id| PaneId::Terminal(id).to_string());
    let mut command = action(at);
    command.arg("switch-session");
    command.args(pane.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
    command.args(["--", to.as_str()]);
    command
}

fn focus(session: &SessionName, pane: PaneId) -> Command {
    let mut command = action(session);
    command.args(["focus-pane-id", &pane.to_string()]);
    command
}

pub fn pane_focused(session: &SessionName, pane: PaneId) -> Result<(), Failure> {
    match done_once_reached(focus(session, pane)) {
        Ok(()) | Err(Failure::Focused(_)) => Ok(()),
        Err(failure) => Err(failure),
    }
}

pub fn went(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Result<(), Failure> {
    if at == to {
        return pane.map_or(Ok(()), |pane| pane_focused(at, PaneId::Terminal(pane)));
    }
    match answer_of(ran(&mut go(at, to, pane))?) {
        Ok(_) | Err(Failure::Focused(_)) => Ok(()),
        Err(failure) => Err(failure),
    }
}

fn start(at: &SessionName, to: &SessionName, cwd: Option<&Directory>) -> Command {
    let mut command = action(at);
    command.arg("switch-session");
    command.args(cwd.map(|cwd| option("--cwd", cwd)));
    command.args(["--", to.as_str()]);
    command
}

pub fn started(at: &SessionName, to: &SessionName, cwd: Option<&Directory>) -> Result<(), Failure> {
    answer_of(ran(&mut start(at, to, cwd))?).map(drop)
}

const PROGRAM: &str = "zellij";

#[must_use]
pub fn words(args: Vec<String>) -> ClientWords {
    ClientWords { program: PROGRAM, args, unset: &LEFT_OUT }
}

#[must_use]
pub fn client_making(session: &SessionName) -> Vec<String> {
    vec![session_option(session)]
}

pub fn client_rejoining(session: &SessionName) -> Result<Vec<String>, String> {
    Ok(vec!["attach".to_owned(), attached(session)?])
}

#[must_use]
pub fn attaches_to(session: &SessionName) -> bool {
    !session.as_str().starts_with('-')
}

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
    }

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
