use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Subcommand;
use lunetad::api::{agent, Agent, UnreadFile, Zellij};
use lunetad::asks::{self, Turn, User, AGENTS_VAR};
use lunetad::closes;
use lunetad::mux::{Client, PaneId, TerminalKey, ZellijPane};
use lunetad::opens;
use lunetad::prunes;
use lunetad::seats::{self, UnreadSeat, Where};
use lunetad::turns::{self, IntoPanes};
use lunetad::Directory;

use crate::agents::{self, Harness};
use crate::close::{self, Act, Doing, Plan, Untold, Whose};
use crate::commands;
use crate::sidebar::PaneKey;
use crate::zellij::{self, pane_address, Close, Closing, Open, Pane, Seat, SessionName, Tab};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Session(String),
    Pane(Pane),
}

impl Address {
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

pub fn address(spelled: &str) -> Result<Address, String> {
    if let Ok(pane) = pane_address(spelled) {
        return Ok(Address::Pane(pane));
    }
    if spelled.is_empty() {
        return Err("no session named".to_owned());
    }
    Ok(Address::Session(spelled.to_owned()))
}

struct Running {
    zellij: Vec<(SessionName, SystemTime)>,
}

impl Running {
    fn now() -> Result<Running, String> {
        Ok(Running { zellij: zellij::running()? })
    }

    fn started(&self, session: &str) -> Result<SystemTime, String> {
        started(session, &self.zellij)
    }

    fn names(&self) -> Vec<&str> {
        self.zellij.iter().map(|(name, _)| name.as_str()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Here {
    pane: Pane,
    started: SystemTime,
}

impl Here {
    fn whose(&self) -> Whose {
        Whose::Zellij(self.started)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Seated {
    In(Here),
    Nowhere,
    Away,
    Untold,
}

const AWAY: &str = "luneta's ask box asked from another machine, where luneta's zellij is, so \
                    no pane of this machine's is the user's, and zellij here moves none of \
                    their clients";

const UNTOLD_PANE: &str = "luneta's ask box cannot tell which pane the user asked from just now";

impl Seated {
    fn here(self) -> Result<Option<Here>, String> {
        match self {
            Seated::In(here) => Ok(Some(here)),
            Seated::Nowhere => Ok(None),
            Seated::Away => Err(AWAY.to_owned()),
            Seated::Untold => Err(UNTOLD_PANE.to_owned()),
        }
    }
}

fn seat(agents: Option<&[Agent]>, running: &Running) -> Result<Seated, String> {
    let (named, pane) = match zellij::here()? {
        None => return Ok(Seated::Nowhere),
        Some(Seat::Asked { user: User::Away, .. }) => return Ok(Seated::Away),
        Some(Seat::Asked { user: User::Untold, .. }) => return Ok(Seated::Untold),
        Some(Seat::Asked { user: User::Pane { session, pane }, .. }) => {
            let asked = Pane { session: session.to_string(), pane };
            if let Ok(started) = started(&asked.session, &running.zellij) {
                return Ok(Seated::In(Here { pane: asked, started }));
            }
            return Err(format!(
                "luneta's ask box asked from pane {asked}, and no session runs by that name \
                 now: it was renamed since"
            ));
        },
        Some(Seat::Opened(ZellijPane { session, pane })) => (session, pane),
    };
    let in_pane = |zellij: &&Zellij| zellij.named == named && zellij.pane == pane;
    let zellij = agents.unwrap_or_default().iter().filter_map(|agent| agent.zellij());
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
    if let Ok(started) = started(&session, &running.zellij) {
        return Ok(Seated::In(Here { pane: Pane { session, pane }, started }));
    }
    Err(lost(if session == named {
        "and no session runs by that name now: it was renamed since".to_owned()
    } else {
        format!("which lunetad says is {session} now, and no session runs by that name")
    }))
}

fn listed(folder: &Path) -> Option<Vec<Agent>> {
    commands::listed_agents(folder).ok().map(|listed| listed.agents)
}

pub fn here(folder: &Path) -> ExitCode {
    let seat = Running::now().and_then(|running| seat(listed(folder).as_deref(), &running));
    let here = seat.and_then(Seated::here);
    let none = "this runs in no pane of zellij";
    said(here.and_then(|here| Ok(here.ok_or(none)?.pane.to_string())))
}

#[derive(Subcommand)]
pub enum Opening {
    #[command(
        about = "Open a pane beside another, in its tab, with nobody moved, and say which it is, \
                 as `<session>:<pane>`"
    )]
    Pane {
        #[arg(
            long,
            value_parser = pane_address,
            help = "The pane to open it beside, as `<session>:<pane>`. Without one, the pane this \
                    runs in"
        )]
        beside: Option<Pane>,
        #[arg(long, help = "The directory it starts in. Without one, this one")]
        cwd: Option<PathBuf>,
        #[arg(
            last = true,
            help = "What runs in it, after `--`. Without it, your shell. In luneta's ask box, \
                    only `claude` or `pi`, as `claude [--name <name>] [<prompt>]`"
        )]
        command: Vec<String>,
    },
    #[command(
        about = "Open a tab of a session, with nobody moved, and say which pane it has, as \
                 `<session>:<pane>`. zellij makes a tab as big as a client of its session, so a \
                 client must be in it"
    )]
    Tab {
        #[arg(long, help = "The session to open it in. Without one, the session this runs in")]
        session: Option<String>,
        #[arg(long, help = "The directory it starts in. Without one, this one")]
        cwd: Option<PathBuf>,
        #[arg(long, help = "What it is named. Without one, zellij numbers it")]
        name: Option<String>,
        #[arg(
            last = true,
            help = "What runs in it, after `--`. Without it, your shell. In luneta's ask box, \
                    only `claude` or `pi`, as `claude [--name <name>] [<prompt>]`"
        )]
        command: Vec<String>,
    },
    #[command(
        about = "Make a session, take you there as `go` does, and say its name. zellij's client \
                 makes it, so that what runs in it has your environment, not this one's"
    )]
    Session {
        #[arg(
            long,
            value_parser = SessionName::parse,
            help = "Without one, it is named for its directory, as the picker names one, with a \
                    `-2` where a session runs by that name"
        )]
        name: Option<SessionName>,
        #[arg(long, help = "The directory it starts in. Without one, this one")]
        cwd: Option<PathBuf>,
    },
}

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

fn seat_here(folder: &Path, running: &Running) -> Result<Option<Here>, String> {
    seat(listed(folder).as_deref(), running)?.here()
}

fn may_run(command: &[String]) -> Result<(), String> {
    match turn_here()? {
        None => Ok(()),
        Some(_) => runs(command, asks::agents_in(zellij::var(AGENTS_VAR)?.as_deref())?),
    }
}

const PI_COMMANDS: [&str; 8] =
    ["install", "remove", "uninstall", "update", "list", "config", "auth", "mcp"];

fn runs(command: &[String], agents: Harness) -> Result<(), String> {
    const ONLY: &str = "in luneta's ask box, a pane runs your shell, `claude [--name <name>] \
                        [<prompt>]` or `pi [--name <name>] [<prompt>]`, and nothing else";
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    let unknown = || format!("{ONLY}: it does not run {}", words.join(" "));
    let (agent, name, prompt) = match words[..] {
        [] => return Ok(()),
        [agent] => (agent, None, None),
        [agent, "--name", name] => (agent, Some(name), None),
        [agent, "--name", name, prompt] => (agent, Some(name), Some(prompt)),
        [agent, prompt] => (agent, None, Some(prompt)),
        _ => return Err(unknown()),
    };
    let harness = agents::named(agent).ok_or_else(unknown)?;
    if harness == Harness::Pi && agents != Harness::Pi {
        let why = "runs only once you pick pi with Tab in luneta's menu";
        return Err(format!("{ONLY}: {agent} {why}"));
    }
    if name.into_iter().chain(prompt).any(|word| word.starts_with('-')) {
        let why = "takes no option but --name, nor a word with a - first";
        return Err(format!("{ONLY}: {agent} {why}"));
    }
    if prompt.is_some_and(|prompt| prompt.starts_with('!')) {
        let why = "takes no prompt with a ! first, a line for your shell";
        return Err(format!("{ONLY}: {agent} {why}"));
    }
    let read = match harness {
        Harness::ClaudeCode => None,
        Harness::Pi => Some(('@', "takes no prompt with an @ first, a file it reads")),
    };
    if let Some((first, why)) = read
        && prompt.is_some_and(|prompt| prompt.starts_with(first))
    {
        return Err(format!("{ONLY}: {agent} {why}"));
    }
    if harness == Harness::Pi
        && name.is_none()
        && prompt.is_some_and(|prompt| PI_COMMANDS.contains(&prompt))
    {
        let why = "takes no prompt that is a command of its own";
        return Err(format!("{ONLY}: {agent} {why}"));
    }
    Ok(())
}

fn pane(
    beside: Option<&Pane>,
    cwd: Option<&Path>,
    command: &[String],
    folder: &Path,
) -> Result<String, String> {
    const UNSEATED: &str = "this runs in no pane of zellij: name one to open beside with --beside";
    may_run(command)?;
    let running = Running::now()?;
    let Pane { session, pane } = if let Some(beside) = beside {
        running.started(&beside.session)?;
        beside.clone()
    } else {
        seat_here(folder, &running)?.ok_or(UNSEATED)?.pane
    };
    let cwd = directory(cwd)?;
    let named = session_named(&session)?;
    let open = Open::Pane { session: named, beside: pane, cwd, command: command.to_vec() };
    let opened = zellij::opening(&open)?.terminal;
    Ok(Pane { session, pane: opened }.to_string())
}

fn tab(
    session: Option<&str>,
    cwd: Option<&Path>,
    name: Option<&str>,
    command: &[String],
    folder: &Path,
) -> Result<String, String> {
    const UNNAMED: &str = "this runs in no pane of zellij: name a --session";
    may_run(command)?;
    let running = Running::now()?;
    let session = if let Some(session) = session {
        running.started(session)?;
        session.to_owned()
    } else {
        seat_here(folder, &running)?.ok_or(UNNAMED)?.pane.session
    };
    let (name, command) = (name.map(str::to_owned), command.to_vec());
    let tab = Tab { cwd: directory(cwd)?, name, command };
    let open = Open::Tab { session: session_named(&session)?, tab, alone_since: None };
    let opened = zellij::opening(&open)?.terminal;
    Ok(Pane { session, pane: opened }.to_string())
}

fn session(
    name: Option<&SessionName>,
    cwd: Option<&Path>,
    folder: &Path,
) -> Result<String, String> {
    let running = Running::now()?;
    let at = seated(listed(folder).as_deref(), &running)?;
    let cwd = directory(cwd)?;
    let names = running.names();
    let session = match name {
        Some(name) if names.contains(&name.as_str()) => {
            return Err(format!("{name} runs already: `luneta go {name}` takes you there"));
        },
        Some(name) => name.clone(),
        None => zellij::session_for(Path::new(cwd.as_str()))
            .ok_or_else(|| format!("no session can be named for {cwd}"))?
            .free(&names),
    };
    zellij::made(&at, &session, &cwd)?;
    Ok(session.to_string())
}

const UNSEATED: &str = "this runs in no pane of zellij, and a multiplexer moves the client that \
                        last typed in the session it is asked from";

fn seated(agents: Option<&[Agent]>, running: &Running) -> Result<SessionName, String> {
    let here = seat(agents, running)?.here()?.ok_or(UNSEATED)?;
    session_named(&here.pane.session)
}

pub fn go(to: &Address, folder: &Path) -> ExitCode {
    let went = Running::now().and_then(|running| {
        let at = seated(listed(folder).as_deref(), &running)?;
        running.started(to.session())?;
        let pane = match to {
            Address::Session(_) => None,
            Address::Pane(pane) => Some(pane.pane),
        };
        zellij::taken(&at, &session_named(to.session())?, pane)
    });
    said(went.map(|()| String::new()))
}

#[derive(Subcommand)]
pub enum Turning {
    #[command(
        about = "Turn a pane into a tab of its own, named as the tab it leaves is, if someone \
                 named that one, with nobody moved. zellij makes a tab as big as a client of its \
                 session, so a client must be in it"
    )]
    Pane {
        #[arg(value_parser = pane_address, help = "The pane, as `<session>:<pane>`")]
        pane: Pane,
    },
    #[command(
        about = "Turn the panes of a tab into panes of another tab of its session, with nobody \
                 moved. The tab they left goes"
    )]
    Tab {
        #[arg(
            value_parser = pane_address,
            help = "A pane of the tab they leave, as `<session>:<pane>`"
        )]
        pane: Pane,
        #[arg(
            long,
            value_parser = pane_address,
            help = "A pane of the tab they go into, as `<session>:<pane>`: two tabs can share a \
                    name, and a pane is of one"
        )]
        into: Pane,
    },
}

pub fn turn(what: &Turning) -> ExitCode {
    said(match what {
        Turning::Pane { pane } => pane_into_tab(pane),
        Turning::Tab { pane, into } => tab_into_panes(pane, into),
    })
}

fn pane_into_tab(given: &Pane) -> Result<String, String> {
    let session = session_named(&given.session)?;
    let started = started(&given.session, &Running::now()?.zellij)?;
    let Some(leaving) = turns::pane_leaving(&session, started, given.pane)? else {
        return Err(opens::no_client(&session));
    };
    let before = zellij::clients_on_panes(&session)?;
    let turned = leaving.turn(&session);
    take_back(&session, &before, &[leaving.pane()]);
    fullscreen_again(&session, leaving.fullscreen());
    turned?;
    Ok(given.to_string())
}

fn tab_into_panes(given: &Pane, into: &Pane) -> Result<String, String> {
    if into.session != given.session {
        return Err(format!(
            "{into} is of another session than {given}, and a tab turns into panes of another \
             tab of its own session"
        ));
    }
    let session = session_named(&given.session)?;
    let started = started(&given.session, &Running::now()?.zellij)?;
    let moving = turns::panes_moving(&session, started, given.pane, into.pane)?;
    let before = zellij::clients_on_panes(&session)?;
    let came = moving.turn(&session);
    take_back(&session, &before, moving.panes());
    fullscreen_again(&session, moving.fullscreen());
    match came? {
        IntoPanes::Done => Ok(given.to_string()),
        IntoPanes::Partly { moved, of, why } => Err(format!(
            "turned {moved} of the {of} panes of the tab of {given} into panes of the tab of \
             {into}, and those stay moved: {why}"
        )),
    }
}

fn take_back(session: &SessionName, before: &[Client], moved: &[PaneId]) {
    if let Err(why) = taken_back(session, before, moved) {
        eprintln!("luneta: the user may not be on the pane they were on before: {why}");
    }
}

fn fullscreen_again(session: &SessionName, pane: Option<PaneId>) {
    if let Some(Err(why)) = pane.map(|pane| turns::fullscreen_again(session, pane)) {
        eprintln!("luneta: a pane the turn took out of fullscreen stays so: {why}");
    }
}

fn taken_back(session: &SessionName, before: &[Client], moved: &[PaneId]) -> Result<(), String> {
    let mut now = zellij::clients_on_panes(session)?;
    if let Some(was) = moved_from(before, &now, moved) {
        zellij::focused(session, was.pane)?;
        now = zellij::clients_on_panes(session)?;
    }
    let is_elsewhere = |was: &Client| now.iter().any(|now| now.id == was.id && now.pane != was.pane);
    if before.iter().any(is_elsewhere) {
        return Err(
            "a client left its pane as the panes moved, and zellij moves back only the client \
             that last typed in the session"
                .to_owned(),
        );
    }
    Ok(())
}

fn moved_from(before: &[Client], after: &[Client], moved: &[PaneId]) -> Option<Client> {
    let taken = |was: &&Client| {
        let now = after.iter().find(|now| now.id == was.id);
        now.is_some_and(|now| now.pane != was.pane && moved.contains(&now.pane))
    };
    let taken: Vec<&Client> = before.iter().filter(taken).collect();
    let outside = taken.iter().find(|was| !moved.contains(&was.pane));
    outside.or(taken.first()).map(|was| **was)
}

pub const PLANNED: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agreed(u64);

impl Agreed {
    fn of(plans: &[Plan], sessions: &[(&str, SystemTime)], turn: Option<u64>) -> Self {
        let mut hasher = DefaultHasher::new();
        for plan in plans {
            (says(&plan.act), plan.ends()).hash(&mut hasher);
        }
        sessions.hash(&mut hasher);
        turn.hash(&mut hasher);
        Agreed(hasher.finish())
    }

    pub fn of_prune(plan: &prunes::Plan, turn: Option<u64>) -> Self {
        let mut hasher = DefaultHasher::new();
        plan.deletes().hash(&mut hasher);
        turn.hash(&mut hasher);
        Agreed(hasher.finish())
    }
}

impl fmt::Display for Agreed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

pub fn agreed(spelled: &str) -> Result<Agreed, String> {
    let read = u64::from_str_radix(spelled, 16);
    read.map(Agreed).map_err(|_| format!("{spelled} is no plan luneta said"))
}

pub fn turn_here() -> Result<Option<Turn>, String> {
    Ok(zellij::here()?.and_then(|seat| seat.turn()))
}

enum Wanted {
    Session,
    Panes(BTreeSet<u32>),
}

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

pub fn close(what: &[Address], yes: Option<Agreed>, folder: &Path) -> ExitCode {
    let turn = match turn_here() {
        Ok(turn) => turn,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let running = match Running::now() {
        Ok(running) => running,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let listed = commands::listed_agents(folder);
    let agents = listed.as_ref().ok().map(|listed| listed.agents.as_slice());
    let unread = listed.as_ref().map_or(&[][..], |listed| listed.unread_files.as_slice());
    let placed = match agents.map(placed).transpose() {
        Ok(placed) => placed.unwrap_or_default(),
        Err(why) => {
            eprintln!(
                "luneta: {why}, so a close cannot tell which agents it ends, and closes nothing"
            );
            return ExitCode::FAILURE;
        },
    };
    let known = match &listed {
        Err(_) => Err(Untold::Away),
        Ok(listed) => {
            let seats = seats::unread(listed.unread, &listed.unread_files);
            let seats = seats.map_err(Untold::Unplaced);
            seats.map(|unread_seats| Told { agents: &placed, unread_seats })
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
    let here = match here {
        Seated::In(here) => Some(here),
        Seated::Untold => {
            eprintln!(
                "luneta: {UNTOLD_PANE}, so a close cannot take theirs last, and closes nothing"
            );
            return ExitCode::FAILURE;
        },
        Seated::Away | Seated::Nowhere => None,
    };
    let (mut plans, mut sessions) = (Vec::new(), Vec::new());
    for (session, wanted) in wanted(what) {
        let planned = running.started(session).and_then(|started| {
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
        let holds = |here: &Here| {
            let act = &plan.act;
            act.whose() == here.whose()
                && session_of(act) == here.pane.session
                && act.takes(here.pane.pane)
        };
        here.as_ref().is_some_and(holds)
    };
    plans.sort_by_key(holds_here);
    let agreed = Agreed::of(&plans, &sessions, turn.map(Turn::key));
    let takes = match turn {
        None => Some(agreed),
        Some(turn) => turn.before().map(|before| Agreed::of(&plans, &sessions, Some(before))),
    };
    let told = yes.is_some() && yes == takes;
    let mut failed = false;
    for plan in &plans {
        println!("{}: {}", says(&plan.act), plan.ends());
        for line in ending(&plan.act, &placed, unread, here.as_ref()) {
            println!("  {line}");
        }
        if !told {
            continue;
        }
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

struct Told<'a> {
    agents: &'a [Placed<'a>],
    unread_seats: Vec<(Where<'a>, u32)>,
}

fn plans_in(
    session: &str,
    started: SystemTime,
    wanted: &Wanted,
    told: Result<&Told, &Untold>,
) -> Result<Vec<Plan>, String> {
    let whose = Whose::Zellij(started);
    let named = session_named(session)?;
    let owned = || named.clone();
    let acts = match wanted {
        Wanted::Session => vec![Act::Kill { session: owned(), whose }],
        Wanted::Panes(panes) => match zellij::closes_together(&named, panes)? {
            Closing::Session => vec![Act::Kill { session: owned(), whose }],
            Closing::Parts { tabs, panes } => {
                let tabs = tabs
                    .into_iter()
                    .map(|(tab, panes)| Act::Tab { session: owned(), whose, tab, panes });
                let panes = panes.into_iter().map(|pane| {
                    Act::Pane { session: owned(), whose, pane: PaneKey::Zellij(pane) }
                });
                tabs.chain(panes).collect()
            },
        },
    };
    let in_session = |placed: &Placed| {
        placed.spot.pane_in(session).map(|pane| (pane, Doing::from(placed.agent)))
    };
    let at = Where::Zellij(session);
    let agents: Result<Vec<_>, Untold> = match told {
        Ok(Told { agents, unread_seats }) => {
            let agents = agents.iter().filter_map(in_session);
            Ok(agents.chain(close::unread_in(unread_seats, at)).collect())
        },
        Err(untold) => Err(*untold),
    };
    let agents = agents.as_deref().map_err(|untold| *untold);
    Ok(acts.into_iter().map(|act| Plan::of(act, agents)).collect())
}

fn session_of(act: &Act) -> &str {
    match act {
        Act::Pane { session, .. }
        | Act::Tab { session, .. }
        | Act::Workstream { session, .. }
        | Act::Project { session, .. }
        | Act::Kill { session, .. }
        | Act::Leave { session, .. } => session.as_str(),
    }
}

fn says(act: &Act) -> String {
    match act {
        Act::Pane { session, pane, .. } => format!("close pane {session}:{pane}"),
        Act::Tab { session, panes, .. } => {
            let panes: Vec<String> = panes.iter().map(|pane| format!("{session}:{pane}")).collect();
            format!("close the tab of {}", panes.join(", "))
        },
        Act::Workstream { session, workstream, .. } => {
            format!("close workstream {workstream} of {session}")
        },
        Act::Project { session, .. } => format!("delete project {session}"),
        Act::Kill { session, .. } | Act::Leave { session, .. } => {
            format!("delete session {session}")
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot<'a> {
    Zellij { session: &'a str, pane: u32 },
    Terminal(TerminalKey),
    Unplaced,
}

impl<'a> Spot<'a> {
    fn zellij(zellij: Option<&'a Zellij>) -> Self {
        match zellij.and_then(|zellij| Some((zellij.session.as_deref()?, zellij.pane))) {
            Some((session, pane)) => Spot::Zellij { session, pane },
            None => Spot::Unplaced,
        }
    }

    fn unread(file: &'a UnreadFile) -> Self {
        match UnreadSeat::of(file) {
            UnreadSeat::Zellij { session, pane } => Spot::Zellij { session, pane },
            UnreadSeat::Terminal(key) => Spot::Terminal(key),
            UnreadSeat::Own(_) | UnreadSeat::NoPane | UnreadSeat::Unplaced => Spot::Unplaced,
        }
    }

    fn pane_in(self, session: &str) -> Option<u32> {
        match self {
            Spot::Zellij { session: at, pane } => (at == session).then_some(pane),
            Spot::Terminal(_) | Spot::Unplaced => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Placed<'a> {
    agent: &'a Agent,
    spot: Spot<'a>,
}

fn placed<'a>(agents: &'a [Agent]) -> Result<Vec<Placed<'a>>, String> {
    let place = |agent: &'a Agent| {
        let spot = match &agent.pane {
            Some(agent::Pane::Terminal(key)) => Spot::Terminal(
                TerminalKey::of(key).map_err(|why| format!("lunetad lists an agent in {why}"))?,
            ),
            Some(agent::Pane::Zellij(_) | agent::Pane::Own(_)) | None => {
                Spot::zellij(agent.zellij())
            },
        };
        Ok(Placed { agent, spot })
    };
    agents.iter().map(place).collect()
}

type Ends<'a, T> = (String, &'a T, Spot<'a>);

fn ending(
    act: &Act,
    agents: &[Placed],
    unread: &[UnreadFile],
    here: Option<&Here>,
) -> Vec<String> {
    let session = session_of(act);
    let ends = |spot: Spot| spot.pane_in(session).is_some_and(|pane| act.takes(pane));
    let seat = |placed: &Placed| match placed.spot {
        Spot::Terminal(key) => commands::terminal_seat(key),
        Spot::Zellij { .. } | Spot::Unplaced => commands::seat(placed.agent.zellij()),
    };
    let agents: Vec<Ends<Agent>> = agents
        .iter()
        .filter(|placed| ends(placed.spot))
        .map(|placed| (seat(placed), placed.agent, placed.spot))
        .collect();
    let unread: Vec<Ends<UnreadFile>> = unread
        .iter()
        .map(|file| (file, Spot::unread(file)))
        .filter(|(_, spot)| ends(*spot))
        .map(|(file, spot)| {
            let seat = match spot {
                Spot::Terminal(key) => commands::terminal_seat(key),
                Spot::Zellij { .. } | Spot::Unplaced => commands::seat(file.zellij.as_ref()),
            };
            (seat, file, spot)
        })
        .collect();
    let seats = agents.iter().map(|(seat, ..)| seat).chain(unread.iter().map(|(seat, ..)| seat));
    let width = seats.map(String::len).max().unwrap_or(0);
    let is_here = |spot: Spot| {
        let Some(here) = here else { return false };
        spot.pane_in(&here.pane.session) == Some(here.pane.pane)
    };
    let mark = |line: String, spot: Spot| {
        if is_here(spot) { format!("{line}  (this runs here)") } else { line }
    };
    let agents = agents.into_iter().map(|(seat, agent, spot)| {
        let line = mark(commands::line(&seat, width, agent), spot);
        (seat, line)
    });
    let unread = unread.into_iter().map(|(seat, file, spot)| {
        let line = mark(commands::unread_line(&seat, width, file), spot);
        (seat, line)
    });
    let mut ended: Vec<(String, String)> = agents.chain(unread).collect();
    ended.sort_by(|(a, _), (b, _)| a.cmp(b));
    ended.into_iter().map(|(_, line)| line).collect()
}

fn act(act: &Act) -> Result<(), String> {
    match act {
        Act::Pane { session, whose, pane } => {
            closes::closed(session, whose.started(session)?, &Close::Pane(pane.id()))
        },
        Act::Tab { session, whose, tab, panes } => {
            let close = Close::Tab { id: *tab, panes: panes.clone() };
            closes::closed(session, whose.started(session)?, &close)
        },
        Act::Workstream { .. } => {
            unreachable!("only the sidebar plans a workstream's delete, and lunetad acts on it")
        },
        Act::Project { .. } => {
            unreachable!("only the sidebar plans a project's delete, and lunetad acts on it")
        },
        Act::Kill { session, whose } => closes::killed(session, whose.started(session)?),
        Act::Leave { session, started, .. } => closes::killed(session, *started),
    }
}

fn session_named(session: &str) -> Result<SessionName, String> {
    SessionName::running(session).map_err(|why| format!("{session:?} names no session: {why}"))
}

fn started(session: &str, running: &[(SessionName, SystemTime)]) -> Result<SystemTime, String> {
    let found = running.iter().find(|(name, _)| name.as_str() == session);
    found.map(|(_, started)| *started).ok_or_else(|| format!("no session {session} runs"))
}

pub fn directory(cwd: Option<&Path>) -> Result<Directory, String> {
    let here = env::current_dir().map_err(|error| format!("no directory to open in: {error}"))?;
    let dir = plain(&cwd.map_or_else(|| here.clone(), |cwd| here.join(cwd)));
    if !dir.is_dir() {
        return Err(format!("{} is no directory", dir.display()));
    }
    let shown = dir.display().to_string();
    let dir = dir.into_os_string().into_string();
    Directory::parse(dir.map_err(|_| format!("{shown} is not UTF-8, as zellij needs"))?)
}

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

    use lunetad::mux::{self, OwnKey, OwnPane};

    use super::*;

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

    #[test]
    fn an_own_pane_by_no_name_is_no_pane_address() {
        let key = OwnKey { id: mux::OwnId::new(3).unwrap(), made: mux::OwnMade::from_nanos(0) };
        let seat = commands::own_seat(OwnPane::new(key, 2).unwrap());
        assert_eq!(seat, "?:2 (own 3)");
        assert!(pane_address(&seat).is_err(), "{seat}");
        let in_own = |session, pane| {
            let pane = Some(agent::Pane::Own(lunetad::api::Own { session, pane }));
            Agent { pane, ..Agent::default() }
        };
        let session = |id, made_at| Some(lunetad::api::OwnSession { id, made_at });
        let made = || Some(SystemTime::UNIX_EPOCH.into());
        let unread = [
            (in_own(None, 2), "?:2 (own ?)", "of no session"),
            (in_own(session(0, made()), 2), "?:2 (own ?)", "0 is no session's id"),
            (in_own(session(3, None), 2), "?:2 (own 3)", "no made_at"),
            (in_own(session(3, made()), 0), "?:? (own 3)", "0 is no pane's id"),
        ];
        for (agent, spelled, said) in unread {
            let (seat, why) = commands::seat_of(&agent).unwrap_err();
            assert_eq!(seat, spelled);
            assert!(why.contains(said), "{why}");
            assert!(pane_address(&seat).is_err(), "{seat}");
        }
    }

    #[test]
    fn the_client_zellij_took_is_the_one_on_a_moved_pane_it_came_to() {
        let (on, plugin) = (PaneId::Terminal, PaneId::Plugin);
        let at = |id, pane| Client { id, pane };
        let moved = [on(1), on(2), plugin(1)];
        let was = Some(at(1, on(7)));
        let taken = |before: &[Client], after: &[Client]| moved_from(before, after, &moved);
        assert_eq!(taken(&[at(1, on(7))], &[at(1, on(1))]), was);
        assert_eq!(taken(&[at(1, on(7))], &[at(1, on(7))]), None);
        assert_eq!(taken(&[at(1, on(1))], &[at(1, on(1))]), None);
        assert_eq!(taken(&[at(1, plugin(1))], &[at(1, plugin(1))]), None);
        assert_eq!(taken(&[at(1, on(1))], &[at(1, on(3))]), None);
        assert_eq!(taken(&[at(1, on(1))], &[at(1, plugin(3))]), None);
        assert_eq!(taken(&[], &[at(1, on(1))]), None);
        assert_eq!(taken(&[at(1, on(7))], &[]), None);
        assert_eq!(taken(&[at(1, on(7))], &[at(1, plugin(1))]), was);
        let from_a_plugin = Some(at(1, plugin(5)));
        assert_eq!(taken(&[at(1, plugin(5))], &[at(1, plugin(1))]), from_a_plugin);
        assert_eq!(taken(&[at(1, plugin(5))], &[at(1, on(1))]), from_a_plugin);
        assert_eq!(taken(&[at(1, on(5))], &[at(1, plugin(5))]), None);
        let (lands, came) = (at(2, on(1)), at(1, on(7)));
        let after = [at(1, on(2)), lands];
        assert_eq!(taken(&[came, at(2, on(2))], &after), Some(came));
        assert_eq!(taken(&[at(2, on(2)), came], &after), Some(came));
        let either = taken(&[at(1, on(1)), at(2, on(2))], &[at(1, on(2)), at(2, on(1))]);
        assert!(either.is_some());
    }

    #[test]
    fn a_directory_given_with_dots_is_the_one_they_lead_to() {
        let scratch = tempfile::tempdir().unwrap();
        let app = scratch.path().join("app");
        fs::create_dir_all(app.join("src")).unwrap();
        let opens = |given: PathBuf| String::from(directory(Some(&given)).unwrap());
        let app_said = app.to_str().unwrap();
        assert_eq!(opens(app.join(".")), app_said);
        assert_eq!(opens(app.join("./src/..")), app_said);
        assert_eq!(opens(app.join("src/../../app/")), app_said);
        assert_eq!(opens(PathBuf::from("/../..")), "/");
        let named = zellij::session_for(Path::new(&opens(app.join("."))));
        assert_eq!(named.map(|name| name.to_string()), Some("app".to_owned()));
    }
}
