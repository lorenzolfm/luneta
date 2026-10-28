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
//! closes nothing until it is told yes to that plan, by the word it prints
//! for it, so that an agent asks you first, and closes no more than you
//! agreed to where what it closes has changed since. What it is given in a
//! session closes together: the panes that are all that holds a tab open
//! close the tab. What holds the pane it runs in goes last, as the agent that
//! ran it goes with it.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Subcommand;
use lunetad::api::Agent;

use crate::close::{Act, Doing, Plan};
use crate::commands;
use crate::zellij::{self, Closing, Open, SessionName};

/// A pane of a session, as the CLI names it: `<session>:<pane>`, as `luneta
/// agents` writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub session: String,
    pub pane: u32,
}

impl fmt::Display for Pane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.session, self.pane)
    }
}

/// A session, or a pane of one, as the CLI names them: `<session>`, or
/// `<session>:<pane>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Session(String),
    Pane(Pane),
}

impl Address {
    /// The session it is, or is of.
    fn session(&self) -> &str {
        match self {
            Address::Session(session) | Address::Pane(Pane { session, .. }) => session,
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Address::Session(session) => f.write_str(session),
            Address::Pane(pane) => pane.fmt(f),
        }
    }
}

/// A pane as it is spelled: its id after the last `:`, and its session's name
/// before it, which may hold a `:`, as zellij's may.
pub fn pane_address(spelled: &str) -> Result<Pane, String> {
    let pane = spelled.rsplit_once(':').and_then(|(session, pane)| {
        let pane = pane.parse().ok()?;
        (!session.is_empty()).then(|| Pane { session: session.to_owned(), pane })
    });
    pane.ok_or_else(|| format!("{spelled} is no pane: name one as <session>:<pane>"))
}

/// An address as it is spelled: a pane, as `pane_address` reads one, or else
/// a session's name alone.
pub fn address(spelled: &str) -> Result<Address, String> {
    if let Ok(pane) = pane_address(spelled) {
        return Ok(Address::Pane(pane));
    }
    if spelled.is_empty() {
        return Err("no session named".to_owned());
    }
    Ok(Address::Session(spelled.to_owned()))
}

/// The pane this runs in, as zellij names it in the variables of what runs
/// in its panes: none outside zellij.
fn here() -> Option<Pane> {
    zellij::here().map(|(session, pane)| Pane { session, pane })
}

/// What `luneta open` opens.
#[derive(Subcommand)]
pub enum Opening {
    /// Open a pane beside another, in its tab, with nobody moved, and say
    /// which it is, as `<session>:<pane>`.
    Pane {
        /// The pane to open it beside, as `<session>:<pane>`. Without one,
        /// the pane this runs in.
        #[arg(long, value_parser = pane_address)]
        beside: Option<Pane>,
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
        #[arg(long, value_parser = SessionName::parse)]
        name: Option<SessionName>,
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
        Opening::Session { name, cwd } => session(name.as_ref(), cwd.as_deref()),
    })
}

/// Opens a pane running `command` beside `beside`, or the pane this runs in,
/// in `cwd`, and names it.
fn pane(beside: Option<&Pane>, cwd: Option<&Path>, command: &[String]) -> Result<String, String> {
    let beside = beside.cloned().or_else(here);
    let Pane { session, pane } =
        beside.ok_or("this runs in no pane of zellij: name one to open beside with --beside")?;
    let cwd = directory(cwd)?;
    let open = Open::Pane { session: session.clone(), beside: pane, cwd, command: command.to_vec() };
    let opened = zellij::opening(&open)?;
    Ok(Pane { session, pane: opened }.to_string())
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
        None => here().ok_or("this runs in no pane of zellij: name a --session")?.session,
    };
    started(&session, &running()?)?;
    if zellij::clients(&session)?.is_empty() {
        return Err(format!(
            "no client is in {session}, and zellij makes a tab with no room for its panes in a \
             session with none: open a pane beside one of its panes, or go there first"
        ));
    }
    let (name, command) = (name.map(str::to_owned), command.to_vec());
    let open = Open::Tab { session: session.clone(), cwd: directory(cwd)?, name, command };
    let opened = zellij::opening(&open)?;
    Ok(Pane { session, pane: opened }.to_string())
}

/// Makes a session named `name`, or for `cwd`, that starts in `cwd`, takes
/// the client that last typed in the session this runs in there, and names
/// it.
fn session(name: Option<&SessionName>, cwd: Option<&Path>) -> Result<String, String> {
    let running = running()?;
    let at = seated(&running)?;
    let cwd = directory(cwd)?;
    let names: Vec<&str> = running.iter().map(|(name, _)| name.as_str()).collect();
    let session = match name {
        Some(name) if names.contains(&name.as_str()) => {
            return Err(format!("{name} runs already: `luneta go {name}` takes you there"));
        },
        Some(name) => name.clone(),
        None => zellij::session_for(Path::new(&cwd))
            .ok_or_else(|| format!("no session can be named for {cwd}"))?
            .free(&names),
    };
    zellij::made(&at, &session, &cwd)?;
    Ok(session.to_string())
}

/// What `go` and `open session` say outside zellij.
const UNSEATED: &str = "this runs in no pane of zellij, and zellij moves the client that last \
                        typed in the session it is asked from";

/// The session this runs in, of those `running`, for zellij to move the
/// client that last typed in it: the name zellij gave it in the variables of
/// this pane as it opened, which no session has where it was renamed since.
fn seated(running: &[(String, SystemTime)]) -> Result<String, String> {
    let at = here().ok_or(UNSEATED)?.session;
    if running.iter().any(|(name, _)| *name == at) {
        return Ok(at);
    }
    Err(format!(
        "this runs in a pane of the session zellij named {at} as the pane opened, and no \
         session runs by that name now: it was renamed since"
    ))
}

/// Takes the client that last typed in the session this runs in to `to`.
pub fn go(to: &Address) -> ExitCode {
    let went = running().and_then(|running| {
        let at = seated(&running)?;
        started(to.session(), &running)?;
        let pane = match to {
            Address::Session(_) => None,
            Address::Pane(pane) => Some(pane.pane),
        };
        zellij::taken(&at, to.session(), pane)
    });
    said(went.map(|()| String::new()))
}

/// What a close exits with where it closes nothing, as it was not told yes to
/// the plan it says: neither 1, for what failed, nor clap's 2, for what it
/// cannot read.
const PLANNED: u8 = 3;

/// What a close was told yes to: the plans it said, as one word an agent
/// hands back with `--yes`. It is of what each plan does and the agents it
/// ends, by what they are doing, as the menu compares a plan with the one it
/// asked about, and of when each session it closes in started, so that one
/// made again by the same name, its panes numbered from 0 again, is another
/// plan. A new build of luneta may hash them otherwise, and so asks again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agreed(u64);

impl Agreed {
    /// The word for `plans`, in the sessions that `started` when they did.
    fn of(plans: &[Plan], started: &[(&str, SystemTime)]) -> Self {
        let mut hasher = DefaultHasher::new();
        for plan in plans {
            (says(&plan.act), plan.ends()).hash(&mut hasher);
        }
        started.hash(&mut hasher);
        Agreed(hasher.finish())
    }
}

impl fmt::Display for Agreed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// A plan as `--yes` spells it: as a close says it.
pub fn agreed(spelled: &str) -> Result<Agreed, String> {
    let read = u64::from_str_radix(spelled, 16);
    read.map(Agreed).map_err(|_| format!("{spelled} is no plan a close said"))
}

/// What a close is given in one session: the session, or panes of it. The
/// session takes along the panes of it given too, and a pane given twice is
/// one.
enum Wanted {
    Session,
    Panes(BTreeSet<u32>),
}

/// What `what` gives a close, by session.
fn wanted(what: &[Address]) -> BTreeMap<&str, Wanted> {
    let mut wanted = BTreeMap::new();
    for address in what {
        let entry = wanted.entry(address.session());
        match (address, entry.or_insert_with(|| Wanted::Panes(BTreeSet::new()))) {
            (Address::Session(_), wanted) => *wanted = Wanted::Session,
            (Address::Pane(pane), Wanted::Panes(panes)) => {
                panes.insert(pane.pane);
            },
            (Address::Pane(_), Wanted::Session) => {},
        }
    }
    wanted
}

/// Says what closing each of `what` closes, and the agents it ends, and
/// closes them where `yes` is the plan it says: what holds the pane this
/// runs in last. Nothing closes where any of them is gone.
pub fn close(what: &[Address], yes: Option<Agreed>, folder: &Path) -> ExitCode {
    let running = match running() {
        Ok(running) => running,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let listed = commands::listed_agents(folder);
    let agents = listed.as_ref().ok().map(|listed| listed.agents.as_slice());
    let (mut plans, mut sessions) = (Vec::new(), Vec::new());
    for (session, wanted) in wanted(what) {
        let planned = started(session, &running)
            .and_then(|started| Ok((started, plans_in(session, &wanted, agents)?)));
        match planned {
            Ok((started, planned)) => {
                sessions.push((session, started));
                plans.extend(planned);
            },
            Err(why) => {
                eprintln!("luneta: {session}: {why}");
                return ExitCode::FAILURE;
            },
        }
    }
    match &listed {
        Err(why) => eprintln!("luneta: {why}"),
        Ok(listed) if listed.unread > 0 => eprintln!("luneta: {}", commands::unread(listed.unread)),
        Ok(_) => {},
    }
    let here = here();
    let holds_here = |plan: &Plan| {
        let holds = |here: &Pane| session_of(&plan.act) == here.session && plan.act.takes(here.pane);
        here.as_ref().is_some_and(holds)
    };
    plans.sort_by_key(holds_here);
    let agreed = Agreed::of(&plans, &sessions);
    let told = yes == Some(agreed);
    let mut failed = false;
    for plan in &plans {
        println!("{}: {}", says(&plan.act), plan.ends());
        for line in ending(&plan.act, agents.unwrap_or_default(), here.as_ref()) {
            println!("  {line}");
        }
        if told && let Err(why) = act(&plan.act) {
            eprintln!("luneta: {why}");
            failed = true;
        }
    }
    match yes {
        _ if told && failed => ExitCode::FAILURE,
        _ if told => ExitCode::SUCCESS,
        Some(_) => {
            eprintln!(
                "luneta: closed nothing, as it was told yes to another plan than this; run it \
                 again with --yes {agreed} to close what it says now"
            );
            ExitCode::from(PLANNED)
        },
        None => {
            eprintln!("luneta: closed nothing; run it again with --yes {agreed} to close what it says");
            ExitCode::from(PLANNED)
        },
    }
}

/// What closing what is `wanted` of `session` does, an act at a time, and
/// the agents each ends among `agents`, as far as luneta can tell them: none
/// while lunetad does not list them. Its panes close together, as zellij
/// says closing them closes.
fn plans_in(session: &str, wanted: &Wanted, agents: Option<&[Agent]>) -> Result<Vec<Plan>, String> {
    let owned = || session.to_owned();
    let acts = match wanted {
        Wanted::Session => vec![Act::Kill(owned())],
        Wanted::Panes(panes) => match zellij::closes_together(session, panes)? {
            Closing::Session => vec![Act::Kill(owned())],
            Closing::Parts { tabs, panes } => {
                let tabs = tabs.into_iter().map(|(tab, panes)| Act::Tab { session: owned(), tab, panes });
                let panes = panes.into_iter().map(|pane| Act::Pane { session: owned(), pane });
                tabs.chain(panes).collect()
            },
        },
    };
    let in_session = |agent: &Agent| {
        let zellij = agent.zellij.as_ref()?;
        (zellij.session.as_deref() == Some(session)).then(|| (zellij.pane, Doing::from(agent)))
    };
    let agents: Option<Vec<_>> = agents.map(|agents| agents.iter().filter_map(in_session).collect());
    Ok(acts.into_iter().map(|act| Plan::of(act, agents.as_deref())).collect())
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
fn ending(act: &Act, agents: &[Agent], here: Option<&Pane>) -> Vec<String> {
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
        here.zip(zellij).is_some_and(|(here, zellij)| {
            zellij.session.as_deref() == Some(here.session.as_str()) && zellij.pane == here.pane
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

/// The sessions that run, by their sockets, each with when it started; or
/// why luneta cannot tell them: a session's socket could be in what it cannot
/// read.
fn running() -> Result<Vec<(String, SystemTime)>, String> {
    let folder = lunetad::zellij_folder();
    lunetad::zellij_sessions(&folder).ok_or_else(|| {
        let folder = folder.display();
        format!("luneta cannot read all of {folder}, where zellij keeps its sessions' sockets")
    })
}

/// When `session` started, of the sessions `running`, or that none runs by
/// that name.
fn started(session: &str, running: &[(String, SystemTime)]) -> Result<SystemTime, String> {
    let found = running.iter().find(|(name, _)| name == session);
    found.map(|(_, started)| *started).ok_or_else(|| format!("no session {session} runs"))
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
    /// session's name may hold a `:` of its own, as zellij lets it. Where a
    /// pane is wanted, as beside one, a session alone is no pane.
    #[test]
    fn an_address_is_a_session_and_a_pane_after_its_last_colon() {
        let session = |session: &str| Ok(Address::Session(session.to_owned()));
        let pane = |session: &str, pane| Ok(Address::Pane(Pane { session: session.to_owned(), pane }));
        assert_eq!(address("work"), session("work"));
        assert_eq!(address("work:2"), pane("work", 2));
        assert_eq!(address("a:b"), session("a:b"));
        assert_eq!(address("a:b:3"), pane("a:b", 3));
        assert_eq!(address("work:"), session("work:"));
        assert_eq!(address(":3"), session(":3"));
        assert!(address("").is_err());
        assert!(pane_address("work").is_err());
    }
}
