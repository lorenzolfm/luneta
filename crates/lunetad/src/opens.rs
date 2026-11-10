//! What zellij opens in a session of the machine it runs on, for a key of
//! luneta's menu or a command of its CLI: a pane beside another, or a tab,
//! with no client moved, and the terminal it opened, for luneta to take its
//! client to. luneta has zellij open it on its own machine; a daemon has it
//! open here for a luneta on another machine, whose menu was for a row of
//! this one.

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::time::SystemTime;

use crate::api::open_request::{self, Opens};
use crate::api::{On, OpenRequest};
use crate::cli::{self, laid, tabs, terminal_of, Laid, Runs};
use crate::closes::{self, Close};
use crate::zellij::{self, SessionName};

/// What names your shell, in luneta's variables, as in those of the zellij
/// it starts, which runs it where its config names no other. What runs luneta
/// can name another there: Claude Code names bash in it for each command an
/// agent runs, `luneta open tab` among them. So a tab luneta opens with no
/// command runs the shell the user database names yours, as login starts it,
/// and the one this names only where the database names none. It runs it
/// even where zellij's config names another, its `default_shell`; a pane
/// luneta opens with none runs that one.
const SHELL_VAR: &str = "SHELL";

/// The shell zellij runs where `SHELL_VAR` names none.
const NO_SHELL: &str = "/bin/sh";

/// What a key of the menu, or the CLI, opens in a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// `command`, or your shell where there is none, in a pane of its own
    /// beside the pane `beside` of `session`, in its tab, and in `cwd`.
    Pane { session: SessionName, beside: u32, cwd: String, command: Vec<String> },
    /// The tab `tab`, in `session`. zellij makes a tab as big as a client of
    /// its session, and one it opens in a session with none has no room for
    /// its panes: in another session than luneta's client's, it opens once
    /// that client has come. Where `alone_since` says a time, it takes the
    /// place of the tabs zellij made the session with, where the session
    /// started then or after, by its machine's clock: as one zellij made for
    /// a client asked to make it then, which has come to it. One that started
    /// before keeps them, as one renamed to its name since does, and the tab
    /// opens beside them.
    Tab { session: SessionName, tab: Tab, alone_since: Option<SystemTime> },
}

impl Open {
    /// The question that has zellij open it on the machine `on` names, or
    /// on the daemon's own, where it names none.
    #[must_use]
    pub fn asked(&self, on: Option<On>) -> OpenRequest {
        let (session, cwd, command, opens) = match self {
            Open::Pane { session, beside, cwd, command } => {
                (session, cwd, command, Opens::Beside(*beside))
            },
            Open::Tab { session, tab: Tab { cwd, name, command }, alone_since } => {
                let alone_since = alone_since.map(Into::into);
                let tab = open_request::Tab { name: name.clone(), alone_since };
                (session, cwd, command, Opens::Tab(tab))
            },
        };
        let (session, cwd, command) = (session.to_string(), cwd.clone(), command.clone());
        OpenRequest { on, session, cwd, command, opens: Some(opens) }
    }
}

/// A tab luneta opens, in `cwd`, named `name` or as zellij numbers it, with
/// `command` in it, or your shell where there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub cwd: String,
    pub name: Option<String>,
    pub command: Vec<String>,
}

/// What zellij opened: the terminal, for luneta to take its client to, and
/// what it left as it was of a tab that was to open alone, a line each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub terminal: u32,
    pub notes: Vec<String>,
}

/// Opens `open`, and says which terminal it opened. zellij opens a pane
/// `--no-focus` beside the one the command runs in, which it reads in
/// `PANE_VAR`, whatever tab that is in and whether a client is on it, and
/// says it as `terminal_<id>`. It says a tab's id, and the tab's terminal is
/// the one it lists in that tab. No tab opens in a session no client is in,
/// where it would have no room for its panes, as `Open::Tab` says, alone or
/// beside the session's tabs.
pub fn opening(open: &Open) -> Result<Opened, String> {
    let terminal = |terminal| Opened { terminal, notes: Vec::new() };
    match open {
        Open::Pane { session, beside, cwd, command: runs } => {
            cli::pane_opened(session, *beside, cwd, Runs::Command(runs)).map(terminal)
        },
        Open::Tab { session, tab, alone_since } => {
            if cli::clients(session.as_str())?.is_empty() {
                return Err(format!(
                    "{}: go there first, or open a pane beside one of its panes",
                    no_client(session)
                ));
            }
            if let Some(since) = alone_since {
                return alone(session, tab, *since);
            }
            let (opened, laid) = tab_opened(session, tab)?;
            terminal_of(&laid, opened).map(terminal).ok_or_else(|| NO_PANE.to_owned())
        },
    }
}

/// Opens your shell beside the pane `beside` of `session`, in its tab, and in
/// `cwd`, as `opening` opens a pane, with no client moved, and says the
/// terminal it opened: as a command, which closes as it ends, as a pane of
/// zellij's own shell does. zellij starts its own shell in the directory of
/// the pane it opens one beside, whatever `--cwd` says, and a command where
/// it says.
pub fn shell_beside(session: &SessionName, beside: u32, cwd: &str) -> Result<u32, String> {
    let shell = shell(login_shell(), env::var_os(SHELL_VAR));
    cli::pane_opened(session, beside, cwd, Runs::Shell(&shell))
}

/// Why no tab opens in `session`, which no client is in.
#[must_use]
pub fn no_client(session: &SessionName) -> String {
    format!(
        "no client is in {session}, and zellij makes a tab with no room for its panes in a session \
         with none"
    )
}

/// What luneta says of a tab zellij opened with no pane in it.
const NO_PANE: &str = "zellij opened a tab with no pane";

/// Opens `tab` in `session`, in place of the tabs zellij made the session
/// with, which luneta closes, so that the session holds the tab alone, where
/// zellij made the session at `since` or after, as `made_since` tells; beside
/// them where it did not, which the notes say. They are those zellij lists
/// before the tab opens, and they hold what its layout laid out and no more,
/// as far as luneta can tell: nothing zellij has says so, but the session is
/// new, and luneta's menu has the keys until the tab opens, so that only a
/// tab another client or an agent opened there by then goes with them. Each
/// goes with the terminals zellij lists in it once the tab has opened: one
/// that holds others by the time zellij would close it stays, which the
/// notes say too.
fn alone(session: &SessionName, tab: &Tab, since: SystemTime) -> Result<Opened, String> {
    let kept = match made_since(session, since) {
        Some(true) => None,
        Some(false) => Some(format!("{session} ran already, and keeps its tabs")),
        None => {
            Some(format!("luneta cannot tell whether zellij made {session}, which keeps its tabs"))
        },
    };
    let made = match kept {
        None => tabs(&laid(session.as_str())?),
        Some(_) => BTreeSet::new(),
    };
    let (opened, laid) = tab_opened(session, tab)?;
    let terminal = terminal_of(&laid, opened).ok_or(NO_PANE)?;
    let mut notes: Vec<String> = kept.into_iter().collect();
    for made in made {
        if let Err(why) = closes::closed_as_listed(session, &Close::tab(&laid, made)) {
            notes.push(why);
        }
    }
    Ok(Opened { terminal, notes })
}

/// Whether the session named `session`, which runs, started at `since` or
/// after, by its socket: one zellij made as luneta asked then, rather than
/// one that ran already, under that name or another, renamed to it since,
/// as a rename keeps the socket's time. A socket's time is of the kernel's
/// coarse clock, a tick behind at most, and zellij takes far longer than a
/// tick to make a session. None where none runs, or where luneta cannot
/// read all of zellij's folder, which could hold it.
fn made_since(session: &SessionName, since: SystemTime) -> Option<bool> {
    let running = zellij::running(&zellij::folder())?;
    let (_, started) = running.into_iter().find(|(name, _)| name == session.as_str())?;
    Some(started >= since)
}

/// Opens `tab` in `session`, as `opening` does, and says its id, and the
/// panes of the session with it in. A tab with no command of its own runs
/// your shell as its command, and closes as it ends, as a pane of zellij's
/// own shell does: zellij starts a command in the tab's `--cwd`, but its own
/// shell where the session's template for a tab says, and the template of a
/// session zellij brought back says the directory the session was in.
pub fn tab_opened(session: &SessionName, tab: &Tab) -> Result<(u32, Vec<Laid>), String> {
    let shell = shell(login_shell(), env::var_os(SHELL_VAR));
    let runs = runs(&tab.command, &shell);
    let opened = cli::tab_opened(session, &tab.cwd, tab.name.as_deref(), runs)?;
    Ok((opened, laid(session.as_str())?))
}

/// What a tab whose command is `command` runs: `shell`, your shell, where it
/// has none.
fn runs<'a>(command: &'a [String], shell: &'a OsStr) -> Runs<'a> {
    if command.is_empty() { Runs::Shell(shell) } else { Runs::Command(command) }
}

/// Your shell: `login`, the one the user database names yours, or, where it
/// names none, `named`, the one `SHELL_VAR` says, or, where neither names
/// one, unset or empty, the one zellij runs then.
fn shell(login: Option<OsString>, named: Option<OsString>) -> OsString {
    let some = |shell: &OsString| !shell.is_empty();
    login.filter(some).or_else(|| named.filter(some)).unwrap_or_else(|| NO_SHELL.into())
}

/// The shell the user database names yours, the one login starts, or none
/// where it has no entry for you.
fn login_shell() -> Option<OsString> {
    let you = uzers::get_user_by_uid(rustix::process::getuid().as_raw())?;
    Some(uzers::os::unix::UserExt::shell(&you).as_os_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab with no command runs your shell as one, which zellij starts in
    /// the tab's directory also in a session it brought back, and closes as
    /// it ends. A command runs as it is.
    #[test]
    fn a_tab_with_no_command_runs_your_shell_as_one() {
        let fish = OsStr::new("/bin/fish");
        assert_eq!(runs(&[], fish), Runs::Shell(fish));
        let claude = ["claude".to_owned(), "go".to_owned()];
        assert_eq!(runs(&claude, fish), Runs::Command(&claude));
    }

    /// Your shell is the one the user database names, over the one
    /// `SHELL_VAR` names, as Claude Code names bash there for an agent's
    /// commands; that one where the database names none; and where neither
    /// names one, unset or empty, `/bin/sh`, the one zellij runs then.
    #[test]
    fn your_shell_is_the_one_the_user_database_names() {
        let (fish, bash) = (|| Some("/bin/fish".into()), || Some("/bin/bash".into()));
        assert_eq!(shell(fish(), bash()), "/bin/fish");
        assert_eq!(shell(None, bash()), "/bin/bash");
        assert_eq!(shell(Some(OsString::new()), bash()), "/bin/bash");
        assert_eq!(shell(None, Some(OsString::new())), "/bin/sh");
        assert_eq!(shell(None, None), "/bin/sh");
    }
}
