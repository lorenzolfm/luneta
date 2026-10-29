//! What the CLI has zellij do, for an agent you tell what you want in words:
//! open a pane, a tab or a session, take you to one, and close them, as the
//! menu's New and Delete groups do. luneta's skill, `skills/luneta`, says how
//! an agent goes about it.
//!
//! A command run in a pane of zellij knows the session and the pane it runs
//! in, by the name zellij gave the session as the pane opened, which lunetad
//! follows through a rename: a pane opens beside that one, a tab in that
//! session, and `go` moves the client that last typed in it, which is yours
//! where you just asked an agent there for something. zellij moves nobody
//! where none has typed in it since it came, and says nothing of it: `go`
//! looks for the client where it sent it, and says so when it is not there.
//! The agent of luneta's ask box runs in no pane, and goes by the one you
//! asked from, where luneta's zellij is, which typed there last. Claude Code
//! runs what that agent runs without asking you, so a pane it opens runs
//! your shell or Claude Code, and no other command.
//!
//! A close says what it would close, and which agents it would end, and
//! closes nothing until it is told yes to that plan, by the word it prints
//! for it, so that an agent asks you first, and closes no more than you
//! agreed to where what it closes has changed since. In luneta's ask box,
//! where Claude Code runs what its agent runs without asking you, a plan
//! takes its yes only in the turn after the one that said it, so that your
//! answer comes between them. What it is given in a session closes together:
//! the panes that are all that holds a tab open close the tab. What holds the
//! pane it runs in goes last, as the agent that ran it goes with it.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::SystemTime;

use clap::Subcommand;
use lunetad::api::{Agent, UnreadFile, Zellij};

use crate::close::{self, Act, Doing, Plan, Untold};
use crate::commands;
use crate::zellij::{self, pane_address, Closing, Open, Pane, Seat, SessionName, Tab};

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

/// The pane this runs in, its session by the name it has now among those
/// `running`: none outside zellij. Or why luneta cannot tell which session
/// that is.
///
/// zellij names the session in the variables of this pane by the name it had
/// as the pane opened, and a rename leaves that behind. lunetad follows the
/// rename: of `agents`, those it lists, the ones in this pane, by that name
/// and the pane's id, say the session they run in now, and it is that one
/// where they all say the same. Where no agent is in this pane, as where
/// lunetad lists none, it is the session that runs by the name zellij gave
/// it, which is another where one was made by that name since this one was
/// renamed.
///
/// The agent of luneta's ask box runs in no pane, and goes by the one you
/// asked from, by the name its session had as you asked: where it runs by
/// that name still. lunetad's agents have no say in it. Their names are
/// those their sessions had as their panes opened, and an agent in a pane of
/// that id, of another session that had that name then, would say that one.
fn seat(
    agents: Option<&[Agent]>,
    running: &[(String, SystemTime)],
) -> Result<Option<Pane>, String> {
    let (named, pane) = match zellij::here()? {
        None => return Ok(None),
        Some(Seat::Asked(asked)) if running.iter().any(|(name, _)| *name == asked.session) => {
            return Ok(Some(asked));
        },
        Some(Seat::Asked(asked)) => {
            return Err(format!(
                "luneta's ask box asked from pane {asked}, and no session runs by that name \
                 now: it was renamed since"
            ));
        },
        Some(Seat::Opened { named, pane }) => (named, pane),
    };
    let in_pane = |zellij: &&Zellij| zellij.named == named && zellij.pane == pane;
    let zellij = agents.unwrap_or_default().iter().filter_map(|agent| agent.zellij.as_ref());
    let told: BTreeSet<Option<&str>> =
        zellij.filter(in_pane).map(|zellij| zellij.session.as_deref()).collect();
    let lost = |why: String| {
        format!(
            "this runs in pane {pane} of the session zellij named {named} as the pane opened, \
             {why}"
        )
    };
    let session = match Vec::from_iter(told).as_slice() {
        [] => named.clone(),
        [Some(now)] => (*now).to_owned(),
        _ => return Err(lost("and lunetad cannot tell which session that is now".to_owned())),
    };
    if running.iter().any(|(name, _)| *name == session) {
        return Ok(Some(Pane { session, pane }));
    }
    Err(lost(if session == named {
        "and no session runs by that name now: it was renamed since".to_owned()
    } else {
        format!("which lunetad says is {session} now, and no session runs by that name")
    }))
}

/// The agents lunetad lists, or none while it lists none: none runs, or it
/// does not answer.
fn listed(folder: &Path) -> Option<Vec<Agent>> {
    commands::listed_agents(folder).ok().map(|listed| listed.agents)
}

/// Says where this runs, as `seat` tells it.
pub fn here(folder: &Path) -> ExitCode {
    let seat = zellij::running().and_then(|running| seat(listed(folder).as_deref(), &running));
    said(seat.and_then(|seat| Ok(seat.ok_or("this runs in no pane of zellij")?.to_string())))
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
        /// What runs in it, after `--`. Without it, your shell. In luneta's
        /// ask box, only `claude [--name <name>] [<prompt>]`.
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
        /// What runs in it, after `--`. Without it, your shell. In luneta's
        /// ask box, only `claude [--name <name>] [<prompt>]`.
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

/// Opens what `what` says, and says where. What runs in the pane this runs
/// in, lunetad in `folder` says, as `seat` reads it.
pub fn open(what: &Opening, folder: &Path) -> ExitCode {
    said(match what {
        Opening::Pane { beside, cwd, command } => {
            pane(beside.as_ref(), cwd.as_deref(), command, folder)
        },
        Opening::Tab { session, cwd, name, command } => {
            tab(session.as_deref(), cwd.as_deref(), name.as_deref(), command, folder)
        },
        Opening::Session { name, cwd } => session(name.as_ref(), cwd.as_deref(), folder),
    })
}

/// The pane this runs in, as `seat` tells it, where there is no other: none
/// outside zellij.
fn seat_here(folder: &Path) -> Result<Option<Pane>, String> {
    seat(listed(folder).as_deref(), &zellij::running()?)
}

/// Whether a pane this opens may run `command`, as `runs` says: in luneta's
/// ask box, only Claude Code. Or why not.
fn may_run(command: &[String]) -> Result<(), String> {
    match Turn::here()? {
        None => Ok(()),
        Some(_) => runs(command),
    }
}

/// Whether `command` is your shell, as an empty one is, or Claude Code on a
/// name and a prompt, `claude [--name <name>] [<prompt>]`, none of which it
/// takes for an option, nor the prompt for a line of your shell. A pane runs
/// what it is given without asking you, and Claude Code, in your permission
/// mode, asks you or looks at what it does. Or why not.
fn runs(command: &[String]) -> Result<(), String> {
    const ONLY: &str = "in luneta's ask box, a pane runs your shell or `claude [--name <name>] \
                        [<prompt>]`, and nothing else";
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    let (name, prompt) = match words[..] {
        [] | ["claude"] => (None, None),
        ["claude", "--name", name] => (Some(name), None),
        ["claude", "--name", name, prompt] => (Some(name), Some(prompt)),
        ["claude", prompt] => (None, Some(prompt)),
        _ => return Err(format!("{ONLY}: it does not run {}", words.join(" "))),
    };
    if name.into_iter().chain(prompt).any(|word| word.starts_with('-')) {
        return Err(format!("{ONLY}: claude takes no option but --name, nor a word with a - first"));
    }
    if prompt.is_some_and(|prompt| prompt.starts_with('!')) {
        return Err(format!("{ONLY}: claude takes no prompt with a ! first, a line for your shell"));
    }
    Ok(())
}

/// Opens a pane running `command` beside `beside`, or the pane this runs in,
/// in `cwd`, and names it.
fn pane(
    beside: Option<&Pane>,
    cwd: Option<&Path>,
    command: &[String],
    folder: &Path,
) -> Result<String, String> {
    may_run(command)?;
    let beside = match beside {
        Some(beside) => Some(beside.clone()),
        None => seat_here(folder)?,
    };
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
    folder: &Path,
) -> Result<String, String> {
    const UNNAMED: &str = "this runs in no pane of zellij: name a --session";
    may_run(command)?;
    let session = match session {
        Some(session) => session.to_owned(),
        None => seat_here(folder)?.ok_or(UNNAMED)?.session,
    };
    started(&session, &zellij::running()?)?;
    if zellij::clients(&session)?.is_empty() {
        return Err(format!(
            "no client is in {session}, and zellij makes a tab with no room for its panes in a \
             session with none: open a pane beside one of its panes, or go there first"
        ));
    }
    let (name, command) = (name.map(str::to_owned), command.to_vec());
    let tab = Tab { cwd: directory(cwd)?, name, command };
    let open = Open::Tab { session: session.clone(), tab };
    let opened = zellij::opening(&open)?;
    Ok(Pane { session, pane: opened }.to_string())
}

/// Makes a session named `name`, or for `cwd`, that starts in `cwd`, takes
/// the client that last typed in the session this runs in there, and names
/// it.
fn session(
    name: Option<&SessionName>,
    cwd: Option<&Path>,
    folder: &Path,
) -> Result<String, String> {
    let running = zellij::running()?;
    let at = seated(listed(folder).as_deref(), &running)?;
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
/// client that last typed in it, by the name it has now, as `seat` tells it
/// among `agents`.
fn seated(agents: Option<&[Agent]>, running: &[(String, SystemTime)]) -> Result<String, String> {
    Ok(seat(agents, running)?.ok_or(UNSEATED)?.session)
}

/// Takes the client that last typed in the session this runs in to `to`.
/// What runs in the pane this runs in, lunetad in `folder` says.
pub fn go(to: &Address, folder: &Path) -> ExitCode {
    let went = zellij::running().and_then(|running| {
        let at = seated(listed(folder).as_deref(), &running)?;
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
/// plan. In luneta's ask box, it is of the turn it is said in too. A new
/// build of luneta may hash them otherwise, and so asks again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agreed(u64);

impl Agreed {
    /// The word for `plans`, in the sessions that `started` when they did,
    /// said in the turn of the ask box `turn` is the key of, if any.
    fn of(plans: &[Plan], started: &[(&str, SystemTime)], turn: Option<u64>) -> Self {
        let mut hasher = DefaultHasher::new();
        for plan in plans {
            (says(&plan.act), plan.ends()).hash(&mut hasher);
        }
        started.hash(&mut hasher);
        turn.hash(&mut hasher);
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

/// What names the turn of luneta's ask box a command runs in, in the
/// variables of the box's agent, and what names the turn before it: a key of
/// each, in hex. The agent knows neither, as Claude Code runs no line of its
/// that reads a variable or sets one, in its `dontAsk` mode.
pub const TURN_VAR: &str = "LUNETA_TURN";
pub const BEFORE_VAR: &str = "LUNETA_TURN_BEFORE";

/// A turn of the agent of luneta's ask box, by its key, and the key of the
/// turn before it, where one went before. A plan a close says in a turn is of
/// its key, and the one it takes a yes to, of the key before, so that a close
/// takes a yes only to a plan said in the turn before, what you answered the
/// agent between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn {
    pub key: u64,
    pub before: Option<u64>,
}

impl Turn {
    /// The turn this runs in, as the ask box names it in the variables of
    /// its agent: none outside the box. Or why they do not read.
    fn here() -> Result<Option<Turn>, String> {
        let (key, before) = (zellij::var(TURN_VAR)?, zellij::var(BEFORE_VAR)?);
        turn_in(key.as_deref(), before.as_deref())
    }

    /// Names it in the variables of `command`, for a close it runs to read.
    pub fn name_in(self, command: &mut Command) {
        command.env(TURN_VAR, format!("{:016x}", self.key));
        match self.before {
            Some(before) => command.env(BEFORE_VAR, format!("{before:016x}")),
            None => command.env_remove(BEFORE_VAR),
        };
    }
}

/// The turn `key` and `before` name, as `TURN_VAR` and `BEFORE_VAR` do: none
/// where neither does.
fn turn_in(key: Option<&str>, before: Option<&str>) -> Result<Option<Turn>, String> {
    let read = |name, spelled: &str| {
        let read = u64::from_str_radix(spelled, 16);
        read.map_err(|_| format!("{name} is no key of a turn of luneta's ask box"))
    };
    match (key, before) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(format!("{BEFORE_VAR} is set, and {TURN_VAR} is not")),
        (Some(key), before) => Ok(Some(Turn {
            key: read(TURN_VAR, key)?,
            before: before.map(|before| read(BEFORE_VAR, before)).transpose()?,
        })),
    }
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
/// closes them where `yes` is the plan it says, or in luneta's ask box, the
/// plan it said in the turn before: what holds the pane this runs in last,
/// and not once anything before it failed to close. Nothing closes where any
/// of them is gone, nor where luneta cannot tell which session this runs in,
/// as `seat` says, nor in luneta's ask box where it cannot tell which pane
/// you asked from.
pub fn close(what: &[Address], yes: Option<Agreed>, folder: &Path) -> ExitCode {
    let turn = match Turn::here() {
        Ok(turn) => turn,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let running = match zellij::running() {
        Ok(running) => running,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let listed = commands::listed_agents(folder);
    let agents = listed.as_ref().ok().map(|listed| listed.agents.as_slice());
    let unread = listed.as_ref().map_or(&[][..], |listed| listed.unread_files.as_slice());
    let known = match &listed {
        Err(_) => Err(Untold::Away),
        Ok(listed) => {
            let seats = listed.unread_seats().map_err(Untold::Unplaced);
            seats.map(|unread| Told { agents: &listed.agents, unread })
        },
    };
    let here = match seat(agents, &running) {
        Ok(here) => here,
        Err(why) => {
            eprintln!(
                "luneta: {why}; a close takes what holds this pane last, so it closes nothing \
                 where it cannot tell what does"
            );
            return ExitCode::FAILURE;
        },
    };
    // The ask box's agent runs in no pane, and goes by the one you asked
    // from. Without it, a close could take yours along, and first.
    if turn.is_some() && here.is_none() {
        eprintln!(
            "luneta: luneta's ask box cannot tell which pane the user asked from just now, so a \
             close cannot take theirs last, and closes nothing"
        );
        return ExitCode::FAILURE;
    }
    let (mut plans, mut sessions) = (Vec::new(), Vec::new());
    for (session, wanted) in wanted(what) {
        let planned = started(session, &running).and_then(|started| {
            Ok((started, plans_in(session, started, &wanted, known.as_ref())?))
        });
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
        Ok(listed) => {
            if let Some(count) = listed.unread_count() {
                eprintln!("luneta: {}", commands::unread(count));
            }
        },
    }
    let holds_here = |plan: &Plan| {
        let holds = |here: &Pane| session_of(&plan.act) == here.session && plan.act.takes(here.pane);
        here.as_ref().is_some_and(holds)
    };
    plans.sort_by_key(holds_here);
    let agreed = Agreed::of(&plans, &sessions, turn.map(|turn| turn.key));
    // The ask box's agent learns the plan of its turn as it is said, and
    // can be told yes to it only once the turn ends, with your answer.
    let takes = match turn {
        None => Some(agreed),
        Some(turn) => turn.before.map(|before| Agreed::of(&plans, &sessions, Some(before))),
    };
    let told = yes.is_some() && yes == takes;
    let mut failed = false;
    for plan in &plans {
        println!("{}: {}", says(&plan.act), plan.ends());
        for line in ending(&plan.act, agents.unwrap_or_default(), unread, here.as_ref()) {
            println!("  {line}");
        }
        if !told {
            continue;
        }
        // The agent that ran this ends with what holds its pane, and could
        // not say what failed.
        if failed && holds_here(plan) {
            eprintln!(
                "luneta: did not {}, as it ends what runs here, and what went before it did \
                 not all close",
                says(&plan.act)
            );
        } else if let Err(why) = act(&plan.act) {
            eprintln!("luneta: {why}");
            failed = true;
        }
    }
    match yes {
        _ if told && failed => ExitCode::FAILURE,
        _ if told => ExitCode::SUCCESS,
        // Out of the ask box, a yes to the plan it says is one it was told.
        Some(yes) if yes == agreed => {
            eprintln!(
                "luneta: closed nothing, as luneta's ask box takes a yes to a plan only in the \
                 turn after the one that said it, with the user's answer between them: end your \
                 turn asking them, and once they say yes, run it again with --yes {agreed}"
            );
            ExitCode::from(PLANNED)
        },
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

/// The agents lunetad tells of: those it lists, and where the agents of the
/// session files it cannot read run, as `unread_seats` gives them.
struct Told<'a> {
    agents: &'a [Agent],
    unread: Vec<(&'a str, u32)>,
}

/// What closing what is `wanted` of `session`, the one that started at
/// `started`, does, an act at a time, and the agents each ends among those
/// lunetad `told` of, or why luneta cannot tell them. Its panes close
/// together, as zellij says closing them closes.
fn plans_in(
    session: &str,
    started: SystemTime,
    wanted: &Wanted,
    told: Result<&Told, &Untold>,
) -> Result<Vec<Plan>, String> {
    let owned = || session.to_owned();
    let acts = match wanted {
        Wanted::Session => vec![Act::Kill { session: owned(), started }],
        Wanted::Panes(panes) => match zellij::closes_together(session, panes)? {
            Closing::Session => vec![Act::Kill { session: owned(), started }],
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
    let agents: Result<Vec<_>, Untold> = match told {
        Ok(Told { agents, unread }) => {
            let agents = agents.iter().filter_map(in_session);
            Ok(agents.chain(close::unread_in(unread, session)).collect())
        },
        Err(untold) => Err(*untold),
    };
    let agents = agents.as_deref().map_err(|untold| *untold);
    Ok(acts.into_iter().map(|act| Plan::of(act, agents)).collect())
}

/// The session `act` closes in.
fn session_of(act: &Act) -> &str {
    match act {
        Act::Pane { session, .. }
        | Act::Tab { session, .. }
        | Act::Kill { session, .. }
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
        Act::Kill { session, .. } | Act::Leave { session, .. } => {
            format!("delete session {session}")
        },
    }
}

/// The agents among `agents` that `act` ends, and those of the session files
/// among `unread` that lunetad cannot read, a line to each, as `luneta
/// agents` writes them: the one in the pane this runs in, `here`, says so.
fn ending(act: &Act, agents: &[Agent], unread: &[UnreadFile], here: Option<&Pane>) -> Vec<String> {
    let session = session_of(act);
    let ends = |zellij: Option<&Zellij>| {
        zellij.is_some_and(|zellij| {
            zellij.session.as_deref() == Some(session) && act.takes(zellij.pane)
        })
    };
    let seated = |zellij: Option<&Zellij>| ends(zellij).then(|| commands::seat(zellij));
    let agents: Vec<(String, &Agent)> = agents
        .iter()
        .filter_map(|agent| Some((seated(agent.zellij.as_ref())?, agent)))
        .collect();
    let unread: Vec<(String, &UnreadFile)> =
        unread.iter().filter_map(|file| Some((seated(file.zellij.as_ref())?, file))).collect();
    let seats = agents.iter().map(|(seat, _)| seat).chain(unread.iter().map(|(seat, _)| seat));
    let width = seats.map(String::len).max().unwrap_or(0);
    let is_here = |zellij: Option<&Zellij>| {
        here.zip(zellij).is_some_and(|(here, zellij)| {
            zellij.session.as_deref() == Some(here.session.as_str()) && zellij.pane == here.pane
        })
    };
    let mark = |line: String, zellij: Option<&Zellij>| {
        if is_here(zellij) { format!("{line}  (this runs here)") } else { line }
    };
    let agents = agents.into_iter().map(|(seat, agent)| {
        let line = mark(commands::line(&seat, width, agent), agent.zellij.as_ref());
        (seat, line)
    });
    let unread = unread.into_iter().map(|(seat, file)| {
        let line = mark(commands::unread_line(&seat, width, file), file.zellij.as_ref());
        (seat, line)
    });
    let mut ended: Vec<(String, String)> = agents.chain(unread).collect();
    ended.sort_by(|(a, _), (b, _)| a.cmp(b));
    ended.into_iter().map(|(_, line)| line).collect()
}

/// Has zellij do `act`.
fn act(act: &Act) -> Result<(), String> {
    match act {
        Act::Pane { session, pane } => zellij::pane_closed(session, *pane),
        Act::Tab { session, tab, .. } => zellij::tab_closed(session, *tab),
        Act::Kill { session, started } | Act::Leave { session, started, .. } => {
            zellij::killed(session, *started)
        },
    }
}

/// When `session` started, of the sessions `running`, or that none runs by
/// that name.
fn started(session: &str, running: &[(String, SystemTime)]) -> Result<SystemTime, String> {
    let found = running.iter().find(|(name, _)| name == session);
    found.map(|(_, started)| *started).ok_or_else(|| format!("no session {session} runs"))
}

/// The directory to open in: `cwd`, from this one where it is relative, or
/// else this one, as zellij takes it, in words. Its `.` and `..` are taken
/// out, as `plain` does, so that a session is named for the directory they
/// lead to.
fn directory(cwd: Option<&Path>) -> Result<String, String> {
    let here = env::current_dir().map_err(|error| format!("no directory to open in: {error}"))?;
    let dir = plain(&cwd.map_or_else(|| here.clone(), |cwd| here.join(cwd)));
    if !dir.is_dir() {
        return Err(format!("{} is no directory", dir.display()));
    }
    let shown = dir.display().to_string();
    dir.into_os_string().into_string().map_err(|_| format!("{shown} is not UTF-8, as zellij needs"))
}

/// `path` with its `.` and `..` taken out by where they stand in it, as a
/// shell's `cd` takes them: a `..` takes out the directory before it, and at
/// the root, none.
fn plain(path: &Path) -> PathBuf {
    let mut plain = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {},
            Component::ParentDir => {
                plain.pop();
            },
            part => plain.push(part),
        }
    }
    plain
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
    use std::fs;

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

    /// A command of the ask box's agent runs in the turn its variables name,
    /// after the one they name before it, if any, and one of anything else in
    /// none. A key that does not read, and a turn before with none after it,
    /// are none of luneta's.
    #[test]
    fn a_turn_of_the_ask_box_is_the_keys_its_variables_name() {
        let turn = |key, before| Ok(Some(Turn { key, before }));
        assert_eq!(turn_in(None, None), Ok(None));
        assert_eq!(turn_in(Some("00000000000000ff"), None), turn(255, None));
        assert_eq!(turn_in(Some("ff"), Some("0a")), turn(255, Some(10)));
        for (key, before) in
            [(Some(""), None), (Some("forged"), None), (Some("ff"), Some("")), (None, Some("ff"))]
        {
            assert!(turn_in(key, before).is_err(), "{key:?} {before:?}");
        }

        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        let mut command = Command::new("luneta");
        Turn { key: 255, before: Some(10) }.name_in(&mut command);
        assert_eq!(var(&command, TURN_VAR), Some(Some("00000000000000ff".to_owned())));
        assert_eq!(var(&command, BEFORE_VAR), Some(Some("000000000000000a".to_owned())));
        Turn { key: 10, before: None }.name_in(&mut command);
        assert_eq!(var(&command, TURN_VAR), Some(Some("000000000000000a".to_owned())));
        assert_eq!(var(&command, BEFORE_VAR), Some(None));
    }

    /// A directory given with a `.` or a `..` in it is the one it leads to,
    /// and names its session for that: `app/.` would name none, and in a
    /// worktree, `luneta-.`.
    #[test]
    fn a_directory_given_with_dots_is_the_one_they_lead_to() {
        let scratch = tempfile::tempdir().unwrap();
        let app = scratch.path().join("app");
        fs::create_dir_all(app.join("src")).unwrap();
        let opens = |given: PathBuf| directory(Some(&given)).unwrap();
        let app_said = app.to_str().unwrap();
        assert_eq!(opens(app.join(".")), app_said);
        assert_eq!(opens(app.join("./src/..")), app_said);
        assert_eq!(opens(app.join("src/../../app/")), app_said);
        assert_eq!(opens(PathBuf::from("/../..")), "/");
        let named = zellij::session_for(Path::new(&opens(app.join("."))));
        assert_eq!(named.map(|name| name.to_string()), Some("app".to_owned()));
    }
}
