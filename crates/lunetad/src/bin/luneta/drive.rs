//! What the CLI has zellij do, for an agent you tell what you want in words:
//! open a pane, a tab or a session, take you to one, and close them, as the
//! menu's New and Delete groups do. luneta's skill, `skills/luneta`, says how
//! an agent goes about it.
//!
//! A command run in a pane of zellij knows the session and the pane it runs
//! in: a pane opens beside that one, a tab in that session, and `go` moves
//! the client that last typed in it, which is yours where you just asked an
//! agent there for something. zellij moves nobody where none has typed in it
//! since it came, and says nothing of it: `go` looks for the client where it
//! sent it, and says so when it is not there.
//!
//! A close says what it would close, and which agents it would end, and
//! closes nothing until it is told `--yes`, so that an agent asks you first.
//! What holds the pane it runs in goes last, as the agent that ran it goes
//! with it.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Status};

use crate::close::{self, Act, Deletes, Doing, Plan, Seat, Subject};
use crate::commands;
use crate::zellij::{self, Open, SessionName};

/// A session, or a pane of one, as the CLI names them: `<session>`, or
/// `<session>:<pane>`, as `luneta agents` writes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub session: String,
    pub pane: Option<u32>,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pane {
            Some(pane) => write!(f, "{}:{pane}", self.session),
            None => f.write_str(&self.session),
        }
    }
}

/// An address as it is spelled: a pane's id after the last `:`, where what
/// follows it is a number, and the session's name before it; or else a
/// session's name alone, which may hold a `:`, as zellij's may.
pub fn address(spelled: &str) -> Result<Address, String> {
    if let Some((session, pane)) = spelled.rsplit_once(':')
        && let Ok(pane) = pane.parse()
        && !session.is_empty()
    {
        return Ok(Address { session: session.to_owned(), pane: Some(pane) });
    }
    if spelled.is_empty() {
        return Err("no session named".to_owned());
    }
    Ok(Address { session: spelled.to_owned(), pane: None })
}

/// What `luneta open` opens.
#[derive(Subcommand)]
pub enum Opening {
    /// Open a pane beside another, in its tab, with nobody moved, and say
    /// which it is, as `<session>:<pane>`.
    Pane {
        /// The pane to open it beside, as `<session>:<pane>`. Without one,
        /// the pane this runs in.
        #[arg(long, value_parser = address)]
        beside: Option<Address>,
        /// The directory it starts in. Without one, this one.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// What runs in it, after `--`. Without it, your shell.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Open a tab of a session, with nobody moved, and say which pane it
    /// has, as `<session>:<pane>`. zellij makes a tab as big as a client of
    /// its session, so a client must be in it.
    Tab {
        /// The session to open it in. Without one, the session this runs in.
        #[arg(long)]
        session: Option<String>,
        /// The directory it starts in. Without one, this one.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// What it is named. Without one, zellij numbers it.
        #[arg(long)]
        name: Option<String>,
        /// What runs in it, after `--`. Without it, your shell.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Make a session, take you there as `go` does, and say its name.
    /// zellij's client makes it, so that what runs in it has your
    /// environment, not this one's.
    Session {
        /// Without one, it is named for its directory, as the picker names
        /// one, with a `-2` where a session runs by that name.
        #[arg(long)]
        name: Option<String>,
        /// The directory it starts in. Without one, this one.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
}

/// Opens what `what` says, and says where.
pub fn open(what: &Opening) -> ExitCode {
    said(match what {
        Opening::Pane { beside, cwd, command } => pane(beside.as_ref(), cwd.as_deref(), command),
        Opening::Tab { session, cwd, name, command } => {
            tab(session.as_deref(), cwd.as_deref(), name.as_deref(), command)
        },
        Opening::Session { name, cwd } => session(name.as_deref(), cwd.as_deref()),
    })
}

/// Opens a pane running `command` beside `beside`, or the pane this runs in,
/// in `cwd`, and names it.
fn pane(
    beside: Option<&Address>,
    cwd: Option<&Path>,
    command: &[String],
) -> Result<String, String> {
    let here = || zellij::here().map(|(session, pane)| Address { session, pane: Some(pane) });
    let beside = beside.cloned().or_else(here);
    let beside =
        beside.ok_or("this runs in no pane of zellij: name one to open beside with --beside")?;
    let Some(pane) = beside.pane else {
        return Err(format!("{beside} is a session: name a pane of it as {beside}:<pane>"));
    };
    let (session, command) = (beside.session, command.to_vec());
    let open = Open::Pane { session: session.clone(), beside: pane, cwd: directory(cwd)?, command };
    let opened = zellij::opening(&open)?;
    Ok(Address { session, pane: Some(opened) }.to_string())
}

/// Opens a tab of `session`, or of the session this runs in, named `name`,
/// running `command` in `cwd`, and names its pane.
fn tab(
    session: Option<&str>,
    cwd: Option<&Path>,
    name: Option<&str>,
    command: &[String],
) -> Result<String, String> {
    let session = match session {
        Some(session) => session.to_owned(),
        None => zellij::here().ok_or("this runs in no pane of zellij: name a --session")?.0,
    };
    runs(&session)?;
    if zellij::clients(&session)?.is_empty() {
        return Err(format!(
            "no client is in {session}, and zellij makes a tab with no room for its panes in a \
             session with none: open a pane beside one of its panes, or go there first"
        ));
    }
    let (name, command) = (name.map(str::to_owned), command.to_vec());
    let open = Open::Tab { session: session.clone(), cwd: directory(cwd)?, name, command };
    let opened = zellij::opening(&open)?;
    Ok(Address { session, pane: Some(opened) }.to_string())
}

/// Makes a session named `name`, or for `cwd`, that starts in `cwd`, takes
/// the client that last typed in the session this runs in there, and names
/// it.
fn session(name: Option<&str>, cwd: Option<&Path>) -> Result<String, String> {
    let (at, _) = zellij::here().ok_or(UNSEATED)?;
    let cwd = directory(cwd)?;
    let running = lunetad::zellij_sessions(&lunetad::zellij_folder()).unwrap_or_default();
    let names: Vec<&str> = running.iter().map(|(name, _)| name.as_str()).collect();
    let session = match name {
        Some(name) => {
            let name = SessionName::parse(name).map_err(|why| format!("no session {name}: {why}"))?;
            if names.contains(&name.as_str()) {
                return Err(format!("{name} runs already: `luneta go {name}` takes you there"));
            }
            name
        },
        None => zellij::session_for(Path::new(&cwd))
            .ok_or_else(|| format!("no session can be named for {cwd}"))?
            .free(&names),
    };
    zellij::taken(&at, session.as_str(), None, Some(&cwd))?;
    Ok(session.to_string())
}

/// What `go` and `open session` say outside zellij.
const UNSEATED: &str = "this runs in no pane of zellij, and zellij moves the client that last \
                        typed in the session it is asked from";

/// Takes the client that last typed in the session this runs in to `to`.
pub fn go(to: &Address) -> ExitCode {
    let went = zellij::here().ok_or_else(|| UNSEATED.to_owned()).and_then(|(at, _)| {
        runs(&to.session)?;
        zellij::taken(&at, &to.session, to.pane, None)
    });
    said(went.map(|()| String::new()))
}

/// Says what closing each of `what` closes, and the agents it ends, and
/// closes them where `yes`: what holds the pane this runs in last. Nothing
/// closes where any of them is gone.
pub fn close(what: &[Address], yes: bool, folder: &Path) -> ExitCode {
    let listed = commands::listed_agents(folder);
    let agents = listed.as_ref().ok().map(|listed| listed.agents.as_slice());
    let mut plans = Vec::new();
    for address in what {
        match plan(address, agents) {
            Ok(plan) => plans.push(plan),
            Err(why) => {
                eprintln!("luneta: {address}: {why}");
                return ExitCode::FAILURE;
            },
        }
    }
    match &listed {
        Err(why) => eprintln!("luneta: {why}"),
        Ok(listed) if listed.unread > 0 => eprintln!("luneta: {}", commands::unread(listed.unread)),
        Ok(_) => {},
    }
    let here = zellij::here();
    let holds_here = |plan: &Plan| {
        let holds = |(session, pane): &(String, u32)| {
            session_of(&plan.act) == session && plan.act.takes(*pane)
        };
        here.as_ref().is_some_and(holds)
    };
    plans.sort_by_key(holds_here);
    let mut failed = false;
    for plan in &plans {
        println!("{}: {}", says(&plan.act), plan.ends());
        for line in ending(&plan.act, agents.unwrap_or_default(), here.as_ref()) {
            println!("  {line}");
        }
        if yes && let Err(why) = act(&plan.act) {
            eprintln!("luneta: {why}");
            failed = true;
        }
    }
    if !yes {
        eprintln!("luneta: closed nothing; run it again with --yes to close what it says");
    }
    if failed || !yes { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// What closing `address` does, and the agents it ends among `agents`, as
/// far as luneta can tell them: none while lunetad does not list them. A
/// pane takes along what zellij says closing it takes.
fn plan(address: &Address, agents: Option<&[Agent]>) -> Result<Plan, String> {
    let session = address.session.as_str();
    runs(session)?;
    let deletes = match address.pane {
        Some(pane) => Deletes::Pane { pane, closes: zellij::closes_now(session, pane)? },
        None => Deletes::Session,
    };
    let in_session = |agent: &Agent| {
        let zellij = agent.zellij.as_ref()?;
        (zellij.session.as_deref() == Some(session)).then(|| (zellij.pane, doing(agent)))
    };
    let agents = agents.map(|agents| agents.iter().filter_map(in_session).collect());
    // zellij ends a client in a session it kills, and luneta takes its own
    // to another.
    let seat = Seat::Elsewhere;
    close::plan(&Subject { session, seat, deletes, agents, refuge: None })
}

/// What an agent is doing, as far as closing it goes.
fn doing(agent: &Agent) -> Doing {
    match &agent.status {
        Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
            Ok(Status::Busy) => Doing::Busy,
            Ok(Status::Idle) => Doing::Idle,
            Ok(Status::Waiting) => Doing::Waiting,
            Ok(Status::Shell) => Doing::Shell,
            // A daemon newer than this CLI's.
            Ok(Status::Unspecified) | Err(_) => Doing::Unknown,
        },
        Some(Reported::OtherStatus(_)) | None => Doing::Unknown,
    }
}

/// The session `act` closes in.
fn session_of(act: &Act) -> &str {
    match act {
        Act::Pane { session, .. }
        | Act::Tab { session, .. }
        | Act::Kill(session)
        | Act::Leave { session, .. } => session,
    }
}

/// What `act` does, in words.
fn says(act: &Act) -> String {
    match act {
        Act::Pane { session, pane } => format!("close pane {session}:{pane}"),
        Act::Tab { session, panes, .. } => {
            let panes: Vec<String> = panes.iter().map(|pane| format!("{session}:{pane}")).collect();
            format!("close the tab of {}", panes.join(", "))
        },
        Act::Kill(session) | Act::Leave { session, .. } => format!("kill session {session}"),
    }
}

/// The agents among `agents` that `act` ends, a line to each, as `luneta
/// agents` writes them: the one in the pane this runs in, `here`, says so.
fn ending(act: &Act, agents: &[Agent], here: Option<&(String, u32)>) -> Vec<String> {
    let session = session_of(act);
    let ends = |agent: &&Agent| {
        let zellij = agent.zellij.as_ref();
        zellij.is_some_and(|zellij| {
            zellij.session.as_deref() == Some(session) && act.takes(zellij.pane)
        })
    };
    let mut ended: Vec<(String, &Agent)> =
        agents.iter().filter(ends).map(|agent| (commands::seat(agent), agent)).collect();
    ended.sort_by(|(a, _), (b, _)| a.cmp(b));
    let width = ended.iter().map(|(seat, _)| seat.len()).max().unwrap_or(0);
    let is_here = |agent: &Agent| {
        let zellij = agent.zellij.as_ref();
        here.zip(zellij).is_some_and(|((session, pane), zellij)| {
            zellij.session.as_deref() == Some(session.as_str()) && zellij.pane == *pane
        })
    };
    let line = |(seat, agent): (String, &Agent)| {
        let line = commands::line(&seat, width, agent);
        if is_here(agent) { format!("{line}  (this runs here)") } else { line }
    };
    ended.into_iter().map(line).collect()
}

/// Has zellij do `act`.
fn act(act: &Act) -> Result<(), String> {
    match act {
        Act::Pane { session, pane } => zellij::pane_closed(session, *pane),
        Act::Tab { session, tab, .. } => zellij::tab_closed(session, *tab),
        Act::Kill(session) | Act::Leave { session, .. } => zellij::killed(session),
    }
}

/// Whether a session named `session` runs, by its socket, or why not.
fn runs(session: &str) -> Result<(), String> {
    let running = lunetad::zellij_sessions(&lunetad::zellij_folder()).unwrap_or_default();
    if running.iter().any(|(name, _)| name == session) {
        return Ok(());
    }
    Err(format!("no session {session} runs"))
}

/// The directory to open in: `cwd`, from this one where it is relative, or
/// else this one, as zellij takes it, in words.
fn directory(cwd: Option<&Path>) -> Result<String, String> {
    let here = env::current_dir().map_err(|error| format!("no directory to open in: {error}"))?;
    let dir = cwd.map_or_else(|| here.clone(), |cwd| here.join(cwd));
    if !dir.is_dir() {
        return Err(format!("{} is no directory", dir.display()));
    }
    let shown = dir.display().to_string();
    dir.into_os_string().into_string().map_err(|_| format!("{shown} is not UTF-8, as zellij needs"))
}

/// Says what was done, where there is something to say, or why it was not.
fn said(done: Result<String, String>) -> ExitCode {
    match done {
        Ok(said) => {
            if !said.is_empty() {
                println!("{said}");
            }
            ExitCode::SUCCESS
        },
        Err(why) => {
            eprintln!("luneta: {why}");
            ExitCode::FAILURE
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane's id follows the last `:`, as `luneta agents` writes it, and a
    /// session's name may hold a `:` of its own, as zellij lets it.
    #[test]
    fn an_address_is_a_session_and_a_pane_after_its_last_colon() {
        let at = |session: &str, pane| Ok(Address { session: session.to_owned(), pane });
        assert_eq!(address("work"), at("work", None));
        assert_eq!(address("work:2"), at("work", Some(2)));
        assert_eq!(address("a:b"), at("a:b", None));
        assert_eq!(address("a:b:3"), at("a:b", Some(3)));
        assert_eq!(address("work:"), at("work:", None));
        assert_eq!(address(":3"), at(":3", None));
        assert!(address("").is_err());
    }
}
