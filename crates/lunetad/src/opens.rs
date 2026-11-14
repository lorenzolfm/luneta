use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::time::SystemTime;

use crate::api::open_request::{self, Opens};
use crate::api::{On, OpenRequest};
use crate::mux::{tabs, terminal_of, Called, Laid, Mux, Runs, SessionName, TabId, Zellij};
use crate::closes::{self, Close, Whose};
use crate::Directory;

const SHELL_VAR: &str = "SHELL";

const NO_SHELL: &str = "/bin/sh";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    Pane { session: SessionName, beside: u32, cwd: Directory, command: Vec<String> },
    Tab { session: SessionName, tab: Tab, alone_since: Option<SystemTime> },
}

impl Open {
    #[must_use]
    pub fn session(&self) -> &SessionName {
        match self {
            Open::Pane { session, .. } | Open::Tab { session, .. } => session,
        }
    }

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
        let (session, cwd, command) = (session.to_string(), cwd.to_string(), command.clone());
        OpenRequest { on, session, cwd, command, opens: Some(opens) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub cwd: Directory,
    pub name: Option<String>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub terminal: u32,
    pub notes: Vec<String>,
}

pub fn opening(open: &Open) -> Result<Opened, String> {
    opened(Called::named(open.session())?, open)
}

pub fn opening_of(open: &Open, whose: Whose) -> Result<Opened, String> {
    opened(Called::of(open.session(), whose)?, open)
}

fn opened(called: Called<'_>, open: &Open) -> Result<Opened, String> {
    let terminal = |terminal| Opened { terminal, notes: Vec::new() };
    match open {
        Open::Pane { beside, cwd, command, .. } => {
            let runs = Runs::of(command, Runs::ZellijShell);
            Ok(terminal(called.mux().pane_opened(*beside, cwd, runs)?))
        },
        Open::Tab { session, tab, alone_since } => {
            if let Called::Own(own) = called {
                let own = Mux::Own(&own);
                let (opened, laid) = tab_opened(own, tab)?;
                return terminal_of(&laid, opened).map(terminal).ok_or_else(|| NO_PANE.to_owned());
            }
            if !Mux::Zellij(session).has_clients()? {
                return Err(format!(
                    "{}: go there first, or open a pane beside one of its panes",
                    no_client(session)
                ));
            }
            if let Some(since) = alone_since {
                return alone(session, tab, *since);
            }
            let (opened, laid) = tab_opened(Mux::Zellij(session), tab)?;
            terminal_of(&laid, opened).map(terminal).ok_or_else(|| NO_PANE.to_owned())
        },
    }
}

#[must_use]
pub fn no_client(session: &SessionName) -> String {
    format!(
        "no client is in {session}, and zellij makes a tab with no room for its panes in a session \
         with none"
    )
}

const NO_PANE: &str = "zellij opened a tab with no pane";

fn alone(session: &SessionName, tab: &Tab, since: SystemTime) -> Result<Opened, String> {
    let kept = match made_since(session, since) {
        Some(true) => None,
        Some(false) => Some(format!("{session} ran already, and keeps its tabs")),
        None => {
            Some(format!("luneta cannot tell whether zellij made {session}, which keeps its tabs"))
        },
    };
    let made = match kept {
        None => tabs(&Mux::Zellij(session).laid()?),
        Some(_) => BTreeSet::new(),
    };
    let (opened, laid) = tab_opened(Mux::Zellij(session), tab)?;
    let terminal = terminal_of(&laid, opened).ok_or(NO_PANE)?;
    let mut notes: Vec<String> = kept.into_iter().collect();
    for made in made {
        let close = Close::tab(&laid, made);
        if let Err(why) = closes::closed_as_listed(Mux::Zellij(session), &close) {
            notes.push(why);
        }
    }
    Ok(Opened { terminal, notes })
}

fn made_since(session: &SessionName, since: SystemTime) -> Option<bool> {
    let running = Zellij::socket_folder().running()?;
    let (_, started) = running.into_iter().find(|(name, _)| name == session)?;
    Some(started >= since)
}

fn tab_opened(mux: Mux<'_>, tab: &Tab) -> Result<(TabId, Vec<Laid>), String> {
    let shell = your_shell();
    let runs = runs(&tab.command, &shell);
    let opened = mux.tab_opened(&tab.cwd, tab.name.as_deref(), runs)?;
    Ok((opened, mux.laid()?))
}

#[must_use]
pub fn your_shell() -> OsString {
    shell(login_shell(), env::var_os(SHELL_VAR))
}

fn runs<'a>(command: &'a [String], shell: &'a OsStr) -> Runs<'a> {
    Runs::of(command, Runs::Shell(shell))
}

fn shell(login: Option<OsString>, named: Option<OsString>) -> OsString {
    let some = |shell: &OsString| !shell.is_empty();
    login.filter(some).or_else(|| named.filter(some)).unwrap_or_else(|| NO_SHELL.into())
}

fn login_shell() -> Option<OsString> {
    let you = uzers::get_user_by_uid(rustix::process::getuid().as_raw())?;
    Some(uzers::os::unix::UserExt::shell(&you).as_os_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_with_no_command_runs_your_shell_as_one() {
        let fish = OsStr::new("/bin/fish");
        assert_eq!(runs(&[], fish), Runs::Shell(fish));
        let claude = ["claude".to_owned(), "go".to_owned()];
        let go = Runs::Command { program: "claude", args: &claude[1..] };
        assert_eq!(runs(&claude, fish), go);
    }

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
