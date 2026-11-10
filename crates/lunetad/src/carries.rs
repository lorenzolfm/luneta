//! What zellij carries from a session of the machine it runs on to another
//! of it, for luneta's sidebar: a pane, or every pane of its tab. zellij
//! moves no pane from one session to another: each session is a server of
//! its own, which holds the terminals of its panes, and no action of its CLI,
//! nor a call of its plugins', names a session to move one to. So each pane
//! closes, and opens anew in the other session, where what ran in it starts
//! again as `again` says it can: Claude Code's agent at its prompt resumes
//! its session, and a shell at its prompt starts anew where it worked. What
//! cannot start again so stays, and a tab stays whole, every pane of it.
//!
//! A pane closes before it opens anew, and its agent ends before its session
//! resumes there, as two agents of one session would write one transcript.
//! One that does not end in `ENDING` leaves its pane closed, and the command
//! that resumes it said. The daemon carries them, for a luneta on its machine
//! as for one on another: it alone reads the agents, and the command lines
//! they run.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{File, TryLockError};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use luneta_protocol::shell::prompts;
use luneta_protocol::tab;

use crate::api::agent::Status as Reported;
use crate::api::carry_request::Lands as Landing;
use crate::api::shell::Doing;
use crate::api::{
    carry_response, Agent, CarryRequest, CarryResponse, Harness, On, Pane, Session, Status,
};
use crate::cli::{self, tabs, Laid};
use crate::closes::{self, Close, Closes};
use crate::opens::{self, Open, Tab};
use crate::process;
use crate::seats::seated;
use crate::transcripts::SessionId;
use crate::zellij::SessionName;

/// How long an agent has to end once its pane closed, before its session
/// resumes in another: Claude Code ends at once as its terminal hangs up.
const ENDING: Duration = Duration::from_secs(5);

/// How long a move takes at the most: zellij's answers to a few questions,
/// and an agent's end.
pub const CARRYING: Duration = Duration::from_secs(15);

/// The file a move holds a lock on in lunetad's folder, from its first look
/// at zellij to its last open: one moves at a time on a machine, as two of
/// one pane would both close it, zellij taking a close of a pane that is
/// gone, and both open it again.
pub const MOVING: &str = "carry.lock";

/// A session, by its name, and when it started: one that took the name since
/// is another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    pub session: SessionName,
    pub started: SystemTime,
}

impl Seat {
    /// The session `name`, of the question's `field`, that started `at`, or
    /// why the question is invalid: zellij takes no such name, or it says no
    /// time.
    fn of<At: TryInto<SystemTime>>(
        field: &str,
        name: &str,
        at: Option<At>,
    ) -> Result<Seat, String> {
        let session = SessionName::running(name).map_err(|why| format!("{field}: {why}"))?;
        let started = at.and_then(|at| at.try_into().ok());
        let started = started.ok_or_else(|| format!("{field}: it says no time it started at"))?;
        Ok(Seat { session, started })
    }
}

/// What goes: a pane, or every pane of its tab, the tab's name with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whole {
    Pane,
    Tab,
}

/// Where what goes lands in the other session: beside a terminal of it, by
/// its id, in its tab, or in a tab of its own, which opens after the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lands {
    Beside(u32),
    Tab,
}

/// The pane `pane` of `from`, or every pane of its tab, as `whole` says, on
/// its way to `to`, another session, where it `lands`. Where it would end
/// `from`, as the last of its panes does, it goes only where `ending` says it
/// may: you said so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carry {
    from: Seat,
    pane: u32,
    whole: Whole,
    to: Seat,
    lands: Lands,
    ending: bool,
}

impl Carry {
    /// What goes, as its fields say, or why it cannot: it would go to the
    /// session it leaves.
    pub fn new(
        from: Seat,
        pane: u32,
        whole: Whole,
        to: Seat,
        lands: Lands,
        ending: bool,
    ) -> Result<Carry, String> {
        if from.session == to.session {
            return Err(format!("to: {} is the session it leaves", to.session));
        }
        Ok(Carry { from, pane, whole, to, lands, ending })
    }

    /// What it takes along, among `sessions` and their `agents`, as
    /// `planned` says.
    pub fn planned(&self, sessions: &[Session], agents: &[Agent]) -> Result<Planned, String> {
        planned(sessions, agents, &self.from, self.pane, self.whole)
    }

    /// The question that has zellij carry it on the machine `on` names, or on
    /// the daemon's own, where it names none.
    #[must_use]
    pub fn asked(&self, on: Option<On>) -> CarryRequest {
        let Carry { from, pane, whole, to, lands, ending } = self;
        let lands = match lands {
            Lands::Beside(beside) => Landing::Beside(*beside),
            Lands::Tab => Landing::NewTab(()),
        };
        CarryRequest {
            on,
            session: from.session.to_string(),
            started_at: Some(from.started.into()),
            pane: *pane,
            tab: *whole == Whole::Tab,
            to: to.session.to_string(),
            to_started_at: Some(to.started.into()),
            lands: Some(lands),
            ending: *ending,
        }
    }

    /// What `request` carries, on the machine it names, or why it is invalid:
    /// a session by a name zellij would not take, or by no time it started
    /// at, or the one it leaves for the one it goes to, or no place to land.
    pub fn of(request: CarryRequest) -> Result<(Option<On>, Carry), String> {
        let CarryRequest { on, session, started_at, pane, tab, to, to_started_at, lands, ending } =
            request;
        let from = Seat::of("session", &session, started_at)?;
        let to = Seat::of("to", &to, to_started_at)?;
        let lands = match lands {
            Some(Landing::Beside(beside)) => Lands::Beside(beside),
            Some(Landing::NewTab(())) => Lands::Tab,
            None => return Err("lands: neither beside a pane nor in a tab".to_owned()),
        };
        let whole = if tab { Whole::Tab } else { Whole::Pane };
        Ok((on, Carry::new(from, pane, whole, to, lands, ending)?))
    }
}

/// What a pane starts as again where it lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Again {
    /// Claude Code's agent `pid` resumes its session `id`, where it started,
    /// `cwd`.
    Resumed { pid: u32, id: SessionId, cwd: String },
    /// Your shell, where the pane worked.
    Shell { cwd: String },
}

impl Again {
    /// Where it starts.
    #[must_use]
    pub fn cwd(&self) -> &str {
        match self {
            Again::Resumed { cwd, .. } | Again::Shell { cwd } => cwd,
        }
    }
}

/// What a pane runs, as the daemon's host has it, with `agent` in it, if one
/// is, as what it starts as again in another session; or why it cannot.
pub fn again(pane: &Pane, agent: Option<&Agent>) -> Result<Again, String> {
    if let Some(agent) = agent {
        return resumed(agent);
    }
    if pane.held {
        return Err("what ran in it has ended".to_owned());
    }
    let cwd = || pane.cwd.clone().ok_or_else(|| "zellij has yet to say where it works".to_owned());
    match pane.shell.as_ref().and_then(|shell| shell.doing.as_ref()) {
        Some(Doing::Going(going)) => Err(format!("it runs `{}`", going.command)),
        Some(Doing::Stopped(stopped)) => Err(format!("it holds `{}`, stopped", stopped.command)),
        Some(Doing::Ended(_) | Doing::Ready(())) => Ok(Again::Shell { cwd: cwd()? }),
        None => match pane.command.as_deref() {
            None => Err("zellij has yet to say what runs in it".to_owned()),
            Some(command) if prompts(command) => Ok(Again::Shell { cwd: cwd()? }),
            Some(command) => Err(format!("it runs `{command}`")),
        },
    }
}

/// `agent` as its session resumes, once it is at its prompt, or why it
/// cannot: it is at work, or waits for an answer, which would be cut short,
/// or it says nothing of what it does, or is another harness's, or was not
/// started at a prompt.
fn resumed(agent: &Agent) -> Result<Again, String> {
    match agent.runs_in() {
        Harness::ClaudeCode => {},
        Harness::Pi => return Err("luneta cannot resume a pi agent yet".to_owned()),
        Harness::Unspecified => {
            return Err("its agent runs in a harness luneta knows not".to_owned());
        },
    }
    if !agent.at_a_prompt() {
        return Err("its agent was started with no prompt to come back to".to_owned());
    }
    let status = match &agent.status {
        Some(Reported::KnownStatus(known)) => Status::try_from(*known).ok(),
        Some(Reported::OtherStatus(_)) | None => None,
    };
    match status {
        Some(Status::Idle) => {},
        Some(Status::Busy) => return Err("its agent is at work".to_owned()),
        Some(Status::Waiting) => return Err("its agent waits for an answer".to_owned()),
        Some(Status::Shell) => return Err("its agent runs a command".to_owned()),
        Some(Status::Unspecified) | None => {
            return Err("its agent says nothing of what it does".to_owned());
        },
    }
    let Some(id) = SessionId::parse(&agent.session_id) else {
        return Err(format!("its agent's session has no id to resume: {:?}", agent.session_id));
    };
    Ok(Again::Resumed { pid: agent.pid, id, cwd: agent.cwd.clone() })
}

/// The panes a move takes, in the order they open again, each by its id with
/// what it starts as, and the name of the tab they go to, if they go to one
/// of their own: their tab's, where someone named it and they are all of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub goes: Vec<(u32, Again)>,
    pub tab: Option<String>,
}

/// What moving the pane `pane` of `from`, or every pane of its tab, as
/// `whole` says, takes along, among `sessions` and their `agents`, as the
/// daemon's host has them; or why it moves nothing: the session or the pane
/// is gone, or one of them cannot start again, as `again` says.
pub fn planned(
    sessions: &[Session],
    agents: &[Agent],
    from: &Seat,
    pane: u32,
    whole: Whole,
) -> Result<Planned, String> {
    let started = |session: &Session| session.started_at.and_then(|at| at.try_into().ok());
    let session = sessions
        .iter()
        .find(|session| {
            session.name == from.session.as_str() && started(session) == Some(from.started)
        })
        .ok_or_else(|| format!("{} is gone", from.session))?;
    let panes: Vec<&Pane> = session.panes.iter().flat_map(|panes| &panes.panes).collect();
    let picked = panes.iter().find(|it| it.id == pane);
    let picked = picked.ok_or_else(|| format!("pane {pane} of {} is gone", from.session))?;
    let mut of_tab: Vec<&Pane> = panes.iter().copied().filter(|it| it.tab == picked.tab).collect();
    of_tab.sort_by_key(|it| it.id);
    let going = match whole {
        Whole::Pane => vec![*picked],
        Whole::Tab => of_tab.clone(),
    };
    let seated = seated(agents, sessions);
    let mut goes = Vec::with_capacity(going.len());
    for pane in going {
        let agent = seated.get(&(session.name.as_str(), pane.id)).copied();
        let again = again(pane, agent).map_err(|why| match whole {
            Whole::Pane => why,
            Whole::Tab => format!("pane {} of its tab: {why}", pane.id),
        })?;
        goes.push((pane.id, again));
    }
    let all = whole == Whole::Tab || of_tab.len() == 1;
    let tab = tab::named(&picked.tab_name).filter(|_| all).map(str::to_owned);
    Ok(Planned { goes, tab })
}

/// How many values an option of Claude Code's takes after it, unless joined
/// to it by `=`: one, one where the next argument is no option, or every one
/// up to the next option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Takes {
    One,
    Maybe,
    Many,
}

/// The options of Claude Code that take a value, as `claude --help` of 2.1.319
/// lists them, and the hidden ones its help names. One it does not list takes
/// none: it is a flag, or a newer Claude Code's.
const TAKES: [(&str, Takes); 48] = [
    ("--agent", Takes::One),
    ("--agents", Takes::One),
    ("--append-system-prompt", Takes::One),
    ("--append-system-prompt-file", Takes::One),
    ("--autocompact", Takes::One),
    ("--debug-file", Takes::One),
    ("--effort", Takes::One),
    ("--environment", Takes::One),
    ("--fallback-model", Takes::One),
    ("--input-format", Takes::One),
    ("--json-schema", Takes::One),
    ("--max-budget-usd", Takes::One),
    ("--model", Takes::One),
    ("-n", Takes::One),
    ("--name", Takes::One),
    ("--output-format", Takes::One),
    ("--permission-mode", Takes::One),
    ("--permission-prompts", Takes::One),
    ("--permission-prompt-tool", Takes::One),
    ("--plugin-dir", Takes::One),
    ("--plugin-url", Takes::One),
    ("--remote-control-session-name-prefix", Takes::One),
    ("--session-id", Takes::One),
    ("--setting-sources", Takes::One),
    ("--settings", Takes::One),
    ("--system-prompt", Takes::One),
    ("--system-prompt-file", Takes::One),
    ("--system-prompt-snapshot", Takes::One),
    ("--cloud", Takes::Maybe),
    ("-d", Takes::Maybe),
    ("--debug", Takes::Maybe),
    ("--from-pr", Takes::Maybe),
    ("--prompt-suggestions", Takes::Maybe),
    ("--remote-control", Takes::Maybe),
    ("-r", Takes::Maybe),
    ("--resume", Takes::Maybe),
    ("--teleport", Takes::Maybe),
    ("-w", Takes::Maybe),
    ("--worktree", Takes::Maybe),
    ("--add-dir", Takes::Many),
    ("--allowedTools", Takes::Many),
    ("--allowed-tools", Takes::Many),
    ("--betas", Takes::Many),
    ("--disallowedTools", Takes::Many),
    ("--disallowed-tools", Takes::Many),
    ("--file", Takes::Many),
    ("--mcp-config", Takes::Many),
    ("--tools", Takes::Many),
];

/// The options a session that resumes leaves out, with what they take: those
/// that pick a session, or start another, or name it anew, as a `/rename` may
/// have since, or run it elsewhere than at a prompt here, or do nothing but
/// print.
const LEFT_OUT: [&str; 24] = [
    "-c",
    "--continue",
    "-r",
    "--resume",
    "--session-id",
    "--fork-session",
    "--from-pr",
    "--teleport",
    "-n",
    "--name",
    "-w",
    "--worktree",
    "--tmux",
    "--bg",
    "--background",
    "--desktop",
    "--cloud",
    "--environment",
    "-p",
    "--print",
    "-h",
    "--help",
    "-v",
    "--version",
];

/// The runtimes Claude Code can run on as a script, whose next argument is
/// the script: its program is the two of them.
const RUNTIMES: [&str; 2] = ["node", "bun"];

/// The command that resumes Claude Code's session `id`, from `args`, the
/// command line its agent runs: its program, and its options but those left
/// out, each with what it takes, as `TAKES` says, and not what it was asked as
/// it started, which its session holds already, nor anything after `--`. Or
/// why there is none: an argument that is no text, or no program.
pub fn resuming(args: &[OsString], id: &SessionId) -> Result<Vec<String>, String> {
    let mut texts = Vec::with_capacity(args.len());
    for arg in args {
        texts.push(arg.to_str().ok_or_else(|| format!("{} is no text", arg.display()))?);
    }
    let args = texts;
    let binary = |arg: &str| arg.rsplit('/').next().unwrap_or(arg).to_owned();
    let program = match args.as_slice() {
        [] => return Err("its command line is empty".to_owned()),
        [runtime, _, ..] if RUNTIMES.contains(&binary(runtime).as_str()) => 2,
        _ => 1,
    };
    let mut kept: Vec<String> = args[..program].iter().map(|arg| (*arg).to_owned()).collect();
    let value = |arg: &&str| !arg.starts_with('-');
    let mut rest = args[program..].iter().copied().peekable();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            continue;
        }
        let short = !arg.starts_with("--") && arg.chars().count() > 2;
        let (name, joined) = if short {
            let end = arg.char_indices().nth(2).map_or(arg.len(), |(at, _)| at);
            (&arg[..end], true)
        } else {
            arg.split_once('=').map_or((arg, false), |(name, _)| (name, true))
        };
        let mut option = vec![arg];
        let takes = TAKES.iter().find(|(known, _)| *known == name).map(|(_, takes)| *takes);
        match takes.filter(|_| !joined) {
            None => {},
            Some(Takes::One) => option.extend(rest.next()),
            Some(Takes::Maybe) => option.extend(rest.next_if(value)),
            Some(Takes::Many) => {
                while let Some(more) = rest.next_if(value) {
                    option.push(more);
                }
            },
        }
        if !LEFT_OUT.contains(&name) {
            kept.extend(option.into_iter().map(str::to_owned));
        }
    }
    kept.extend(["--resume".to_owned(), id.as_str().to_owned()]);
    Ok(kept)
}

/// What closes of the session the panes leave: some of it, or all, which
/// ends it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Leaves {
    Part(Close),
    Session,
}

/// What `carry` closes, among `laid`, the panes of the session it leaves as
/// zellij lists them now, once `planned` goes; or why it closes nothing: the
/// pane is gone, or would take floating panes along, or its tab holds a pane
/// the plan has not, or it would end the session where `carry` says it may
/// not.
fn leaves(laid: &[Laid], carry: &Carry, planned: &Planned) -> Result<Leaves, String> {
    let Carry { from, pane, whole, ending, .. } = carry;
    let session = || {
        if *ending {
            Ok(Leaves::Session)
        } else {
            Err(format!("it is all {} has, which would end without it", from.session))
        }
    };
    match whole {
        Whole::Pane => match closes::closing(laid, *pane)? {
            Closes::Pane => Ok(Leaves::Part(Close::Pane(*pane))),
            Closes::Tab { id, panes } if panes == [*pane] => {
                Ok(Leaves::Part(Close::Tab { id, panes }))
            },
            Closes::Tab { .. } => {
                Err("its tab would close with it, floating panes and all: move the tab".to_owned())
            },
            Closes::Session => session(),
        },
        Whole::Tab => {
            let found = laid.iter().find(|laid| laid.id == *pane && !laid.is_plugin);
            let id = found.map(|laid| laid.tab_id).ok_or_else(|| format!("pane {pane} is gone"))?;
            let panes = closes::terminals(laid, id);
            let going: BTreeSet<u32> = planned.goes.iter().map(|(pane, _)| *pane).collect();
            if let Some(other) = panes.iter().find(|pane| !going.contains(pane)) {
                return Err(format!("its tab holds pane {other}, which lunetad has yet to list"));
            }
            if tabs(laid).iter().all(|tab| *tab == id) {
                return session();
            }
            Ok(Leaves::Part(Close::Tab { id, panes }))
        },
    }
}

/// Whether what `lands` on in `to` is there, or why not: the pane is gone, or
/// no client is in the session to make room for a tab.
fn landing(to: &SessionName, lands: Lands) -> Result<(), String> {
    match lands {
        Lands::Beside(beside) => {
            let laid = cli::laid(to.as_str())?;
            if laid.iter().any(|laid| laid.id == beside && !laid.is_plugin) {
                Ok(())
            } else {
                Err(format!("pane {beside} of {to} is gone"))
            }
        },
        Lands::Tab => {
            if cli::clients(to.as_str())?.is_empty() {
                return Err(opens::no_client(to));
            }
            Ok(())
        },
    }
}

/// A pane on its way: its id, where it starts again, and the command it
/// starts with there, or none for your shell.
type Going = (u32, String, Vec<String>);

/// What starts what `going` would have started again, by hand.
fn by_hand(going: &[Going]) -> String {
    let each = going.iter().map(|(_, cwd, command)| {
        if command.is_empty() {
            return format!("your shell in {cwd}");
        }
        let command: Vec<String> = command.iter().map(|arg| quoted(arg)).collect();
        format!("`{}` in {cwd}", command.join(" "))
    });
    each.collect::<Vec<_>>().join("; ")
}

/// `arg`, as a shell reads it back the same: as it is, of letters and the
/// like alone, or else in single quotes.
fn quoted(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c);
    if !arg.is_empty() && arg.chars().all(plain) {
        return arg.to_owned();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// The lock on `MOVING` in `folder`, lunetad's, which lets go as it drops, or
/// why there is none: another move holds it.
fn alone(folder: &Path) -> Result<File, String> {
    let path = folder.join(MOVING);
    let lock = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err("another move is under way".to_owned()),
        Err(TryLockError::Error(error)) => Err(format!("{}: {error}", path.display())),
    }
}

/// Whether the process `pid` ends within `ENDING`.
fn ended(pid: u32) -> bool {
    let (tell, heard) = mpsc::channel();
    thread::spawn(move || {
        let _ = tell.send(process::ends(pid));
    });
    matches!(heard.recv_timeout(ENDING), Ok(Ok(())))
}

/// What a move did once it closed what it takes: each pane opened again, the
/// terminal each opened as by the one it closed, in their order; or some did
/// not, those that did said, with what it closed and did not open again, and
/// what starts each of those by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Moved {
    All(Vec<(u32, u32)>),
    Stranded { opened: Vec<(u32, u32)>, why: String },
}

impl Moved {
    /// What the daemon answered of a move.
    #[must_use]
    pub fn of(answer: CarryResponse) -> Moved {
        let each = |carried: carry_response::Carried| (carried.from, carried.terminal);
        let opened = answer.carried.into_iter().map(each).collect();
        match answer.stranded {
            None => Moved::All(opened),
            Some(why) => Moved::Stranded { opened, why },
        }
    }
}

/// Moves what `planned` takes of `carry`, alone on its machine, whose lunetad
/// keeps `folder`: closes the pane, or its tab, or the session where it may,
/// and opens each pane again where it lands, once what ran in it has ended.
/// It answers with what it `Moved`, or why it closed nothing, as `alone`,
/// `leaves` and `landing` say, or a session that took a name since.
pub fn carried(folder: &Path, carry: &Carry, planned: &Planned) -> Result<Moved, String> {
    let _alone = alone(folder)?;
    let Carry { from, to, lands, .. } = carry;
    closes::same_session(&from.session, from.started)?;
    closes::same_session(&to.session, to.started)?;
    let mut going: Vec<Going> = Vec::with_capacity(planned.goes.len());
    for (pane, again) in &planned.goes {
        let command = match again {
            Again::Shell { .. } => Vec::new(),
            Again::Resumed { pid, id, .. } => {
                let args = process::args(*pid).ok_or_else(|| format!("its agent, {pid}, ended"))?;
                resuming(&args, id).map_err(|why| format!("its agent's command line: {why}"))?
            },
        };
        going.push((*pane, again.cwd().to_owned(), command));
    }
    let laid = cli::laid(from.session.as_str())?;
    let leaving = leaves(&laid, carry, planned)?;
    landing(&to.session, *lands)?;
    let closed = match &leaving {
        Leaves::Part(close) => {
            closes::closed_as_listed(&from.session, close)?;
            match carry.whole {
                Whole::Pane => format!("closed pane {} of {}", carry.pane, from.session),
                Whole::Tab => format!("closed the tab of pane {} of {}", carry.pane, from.session),
            }
        },
        Leaves::Session => {
            closes::killed(&from.session, from.started)?;
            format!("ended {}", from.session)
        },
    };
    for (pane, again) in &planned.goes {
        if let Again::Resumed { pid, .. } = again
            && !ended(*pid)
        {
            let secs = ENDING.as_secs();
            let why = format!(
                "{closed}, but its agent, {pid}, did not end in {secs}s, and opened nowhere: once \
                 it has, start pane {pane} again by hand: {}",
                by_hand(&going)
            );
            return Ok(Moved::Stranded { opened: Vec::new(), why });
        }
    }
    let mut carried = Vec::with_capacity(going.len());
    let mut beside = match lands {
        Lands::Beside(beside) => Some(*beside),
        Lands::Tab => None,
    };
    for (at, (pane, cwd, command)) in going.iter().enumerate() {
        let (session, cwd, command) = (to.session.clone(), cwd.clone(), command.clone());
        let opened = match beside {
            Some(beside) if command.is_empty() => opens::shell_beside(&session, beside, &cwd),
            Some(beside) => {
                let open = Open::Pane { session, beside, cwd, command };
                opens::opening(&open).map(|opened| opened.terminal)
            },
            None => {
                let tab = Tab { cwd, name: planned.tab.clone(), command };
                let open = Open::Tab { session, tab, alone_since: None };
                opens::opening(&open).map(|opened| opened.terminal)
            },
        };
        match opened {
            Ok(terminal) => {
                carried.push((*pane, terminal));
                beside = Some(terminal);
            },
            Err(why) => {
                let why = format!(
                    "{closed}, but zellij opened {at} of {} again in {}: {why}. Start the rest \
                     by hand: {}",
                    going.len(),
                    to.session,
                    by_hand(&going[at..])
                );
                return Ok(Moved::Stranded { opened: carried, why });
            },
        }
    }
    Ok(Moved::All(carried))
}

/// What the daemon answers of what it `Moved`.
#[must_use]
pub fn answer(moved: &Moved) -> CarryResponse {
    let each = |(from, terminal): &(u32, u32)| carry_response::Carried {
        from: *from,
        terminal: *terminal,
    };
    let (opened, stranded) = match moved {
        Moved::All(opened) => (opened, None),
        Moved::Stranded { opened, why } => (opened, Some(why.clone())),
    };
    CarryResponse { carried: opened.iter().map(each).collect(), stranded }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use prost_types::Timestamp;

    use super::*;
    use crate::api::shell::{Ended, Going, Stopped};
    use crate::api::{Front, Panes, Shell, Zellij};

    const ID: &str = "0b7e1c2a-5d1f-4c8e-9a43-1f2e3d4c5b6a";

    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn id() -> SessionId {
        SessionId::parse(ID).unwrap()
    }

    fn resumed_from(started: &[&str]) -> Vec<String> {
        resuming(&args(started), &id()).unwrap()
    }

    fn with_resume(kept: &[&str]) -> Vec<String> {
        kept.iter().copied().chain(["--resume", ID]).map(str::to_owned).collect()
    }

    /// An agent resumes its session with what its options took, each where it
    /// is joined to its option by `=` too, but not what it was asked as it
    /// started, which its session holds: before `--`, after it, or among the
    /// options.
    #[test]
    fn an_agent_resumes_with_its_options_and_not_its_prompt() {
        let tackle = "Tackle https://github.com/lorenzolfm/luneta/issues/299";
        assert_eq!(resumed_from(&["claude", tackle]), with_resume(&["claude"]));
        let started = ["claude", "--dangerously-skip-permissions", "--model", "opus", "--", "go"];
        let kept = ["claude", "--dangerously-skip-permissions", "--model", "opus"];
        assert_eq!(resumed_from(&started), with_resume(&kept));
        let started = ["claude", "fix it", "--effort=high", "--permission-mode", "plan"];
        let kept = ["claude", "--effort=high", "--permission-mode", "plan"];
        assert_eq!(resumed_from(&started), with_resume(&kept));
        let started = ["/bin/claude", "--allowedTools", "Read(//f)", "Edit", "--", "Fix this."];
        let kept = ["/bin/claude", "--allowedTools", "Read(//f)", "Edit"];
        assert_eq!(resumed_from(&started), with_resume(&kept));
    }

    /// An option that may take a value takes the next argument only where it
    /// is no option, as Claude Code's own parser reads it.
    #[test]
    fn an_option_that_may_take_a_value_takes_one_that_is_no_option() {
        let started = ["claude", "--debug", "api", "--verbose", "go"];
        assert_eq!(resumed_from(&started), with_resume(&["claude", "--debug", "api", "--verbose"]));
        let started = ["claude", "--debug", "--verbose"];
        assert_eq!(resumed_from(&started), with_resume(&["claude", "--debug", "--verbose"]));
    }

    /// What picks or names a session, or starts another, or runs it
    /// elsewhere, is left out, with what it takes, as one by a letter is, its
    /// value joined to it or not.
    #[test]
    fn what_picks_or_names_a_session_is_left_out() {
        let started = [
            "claude",
            "-r",
            "old",
            "--continue",
            "--session-id",
            "0a",
            "--fork-session",
            "--name",
            "auth",
            "-nwork",
            "--resume=old",
            "--worktree",
            "--model",
            "fable",
        ];
        assert_eq!(resumed_from(&started), with_resume(&["claude", "--model", "fable"]));
    }

    /// Claude Code run as a script keeps its runtime and its script as its
    /// program.
    #[test]
    fn a_script_keeps_its_runtime() {
        let started = ["node", "/lib/cli.js", "--model", "opus", "go"];
        assert_eq!(
            resumed_from(&started),
            with_resume(&["node", "/lib/cli.js", "--model", "opus"])
        );
    }

    /// A command line that is empty, or has an argument that is no text,
    /// resumes nothing.
    #[test]
    fn a_command_line_that_does_not_read_resumes_nothing() {
        use std::os::unix::ffi::OsStringExt;

        assert!(resuming(&[], &id()).is_err());
        let garbled = vec![OsString::from("claude"), OsString::from_vec(vec![0xff])];
        assert!(resuming(&garbled, &id()).unwrap_err().contains("is no text"));
    }

    /// What a hand types to start it again is quoted where a shell would read
    /// it otherwise.
    #[test]
    fn what_starts_it_by_hand_is_quoted_for_a_shell() {
        let command = ["claude", "--append-system-prompt", "it's done", "--resume", ID];
        let going = [(3, "/w".to_owned(), command.map(str::to_owned).to_vec())];
        let quoted = format!("`claude --append-system-prompt 'it'\\''s done' --resume {ID}` in /w");
        assert_eq!(by_hand(&going), quoted);
        let shell = [(4, "/w/b".to_owned(), Vec::new())];
        assert_eq!(by_hand(&shell), "your shell in /w/b");
    }

    fn pane(id: u32, tab: u32, command: &str) -> Pane {
        Pane {
            id,
            tab,
            tab_name: format!("Tab #{}", tab + 1),
            cwd: Some("/w/luneta".to_owned()),
            command: Some(command.to_owned()),
            titled: None,
            shell: None,
            tab_id: Some(tab + 10),
            held: false,
        }
    }

    fn in_shell(id: u32, tab: u32, doing: Doing) -> Pane {
        Pane { shell: Some(Shell { doing: Some(doing) }), ..pane(id, tab, "fish") }
    }

    fn going(command: &str) -> Doing {
        let since = Some(Timestamp { seconds: 100, nanos: 0 });
        Doing::Going(Going { command: command.to_owned(), since, front: Front::Working.into() })
    }

    fn agent(pane: u32, status: Status) -> Agent {
        Agent {
            pid: 4000 + pane,
            harness: Some(Harness::ClaudeCode.into()),
            session_id: ID.to_owned(),
            cwd: "/w/luneta/main".to_owned(),
            status: Some(Reported::KnownStatus(status.into())),
            status_since: None,
            started_at: Some(Timestamp { seconds: 50, nanos: 0 }),
            name: None,
            entrypoint: Some("cli".to_owned()),
            zellij: Some(Zellij {
                named: "work".to_owned(),
                session: Some("work".to_owned()),
                pane,
            }),
            waiting_for: None,
            topic: None,
        }
    }

    /// Claude Code's agent at its prompt resumes its session where it
    /// started; one at work, or waiting for an answer, or of another
    /// harness, or not started at a prompt, or that says nothing of what it
    /// does, stays.
    #[test]
    fn an_agent_at_its_prompt_alone_starts_again() {
        let pane = pane(3, 0, "claude");
        let again = |agent: &Agent| super::again(&pane, Some(agent));
        let idle = agent(3, Status::Idle);
        let resumed = Again::Resumed { pid: 4003, id: id(), cwd: "/w/luneta/main".to_owned() };
        assert_eq!(again(&idle), Ok(resumed));
        assert!(again(&agent(3, Status::Busy)).unwrap_err().contains("at work"));
        assert!(again(&agent(3, Status::Waiting)).unwrap_err().contains("waits for an answer"));
        let pi = Agent { harness: Some(Harness::Pi.into()), ..idle.clone() };
        assert!(again(&pi).unwrap_err().contains("pi"));
        let printing = Agent { entrypoint: Some("sdk-cli".to_owned()), ..idle.clone() };
        assert!(again(&printing).unwrap_err().contains("no prompt"));
        let silent = Agent { status: None, ..idle.clone() };
        assert!(again(&silent).unwrap_err().contains("says nothing"));
        let unnamed = Agent { session_id: "-r".to_owned(), ..idle };
        assert!(again(&unnamed).unwrap_err().contains("no id"));
    }

    /// A shell at its prompt starts anew where it works, as its hook says, or
    /// as what runs in front of it is a shell with nothing to run, where it
    /// has no hook. One that runs a command, or holds one stopped, stays, and
    /// so does one that runs a script, or what `-c` gave it, a program, what
    /// ended, and what zellij has yet to say.
    #[test]
    fn a_shell_at_its_prompt_alone_starts_again() {
        let shell = Ok(Again::Shell { cwd: "/w/luneta".to_owned() });
        assert_eq!(again(&in_shell(1, 0, Doing::Ready(())), None), shell);
        let at = Some(Timestamp { seconds: 120, nanos: 0 });
        let took = Some(prost_types::Duration { seconds: 1, nanos: 0 });
        let ended = Ended { command: "make".to_owned(), at, ending: None, took };
        assert_eq!(again(&in_shell(1, 0, Doing::Ended(ended)), None), shell);
        for command in ["fish", "-fish", "/run/current-system/sw/bin/zsh -l"] {
            assert_eq!(again(&pane(1, 0, command), None), shell, "{command}");
        }
        let runs = again(&in_shell(1, 0, going("cargo test")), None);
        assert_eq!(runs, Err("it runs `cargo test`".to_owned()));
        let at = Some(Timestamp { seconds: 120, nanos: 0 });
        let stopped = Doing::Stopped(Stopped { command: "vim".to_owned(), at });
        assert_eq!(
            again(&in_shell(1, 0, stopped), None),
            Err("it holds `vim`, stopped".to_owned())
        );
        assert_eq!(again(&pane(1, 0, "htop -d 5"), None), Err("it runs `htop -d 5`".to_owned()));
        for command in ["sh -c npm run dev", "bash deploy.sh", "/bin/bash -lc make watch"] {
            assert_eq!(again(&pane(1, 0, command), None), Err(format!("it runs `{command}`")));
        }
        let held = Pane { held: true, ..pane(1, 0, "fish") };
        assert!(again(&held, None).unwrap_err().contains("ended"));
        let unsaid = Pane { command: None, ..pane(1, 0, "fish") };
        assert!(again(&unsaid, None).unwrap_err().contains("yet to say"));
        let nowhere = Pane { cwd: None, ..pane(1, 0, "fish") };
        assert!(again(&nowhere, None).unwrap_err().contains("where it works"));
    }

    fn work(panes: Vec<Pane>) -> Session {
        Session {
            name: "work".to_owned(),
            started_at: Some(Timestamp { seconds: 100, nanos: 0 }),
            panes: Some(Panes { panes }),
            clients: None,
            socket: 7,
        }
    }

    fn seat(name: &str, started: u64) -> Seat {
        let session = SessionName::running(name).unwrap();
        Seat { session, started: SystemTime::UNIX_EPOCH + Duration::from_secs(started) }
    }

    /// A pane goes alone, its tab's name with it where it is all of a tab
    /// someone named; a tab goes with each of its panes, in the order of their
    /// ids, and its name. One that stays keeps all of a tab, and says which.
    #[test]
    fn a_pane_goes_alone_and_a_tab_with_all_its_panes() {
        let named = |pane: Pane| Pane { tab_name: "auth".to_owned(), ..pane };
        let panes = vec![
            named(pane(5, 1, "fish")),
            pane(1, 0, "fish"),
            named(pane(2, 1, "claude")),
            named(in_shell(4, 1, Doing::Ready(()))),
        ];
        let sessions = [work(panes)];
        let agents = [agent(2, Status::Idle)];
        let from = seat("work", 100);
        let planned = |pane, whole| planned(&sessions, &agents, &from, pane, whole);
        let shell = || Again::Shell { cwd: "/w/luneta".to_owned() };

        let alone = planned(1, Whole::Pane).unwrap();
        assert_eq!(alone, Planned { goes: vec![(1, shell())], tab: None });
        let sharing = planned(4, Whole::Pane).unwrap();
        assert_eq!(sharing, Planned { goes: vec![(4, shell())], tab: None });

        let tab = planned(4, Whole::Tab).unwrap();
        let ids: Vec<u32> = tab.goes.iter().map(|(pane, _)| *pane).collect();
        assert_eq!(ids, [2, 4, 5]);
        assert!(matches!(tab.goes[0].1, Again::Resumed { pid: 4002, .. }), "{tab:?}");
        assert_eq!(tab.tab.as_deref(), Some("auth"));

        let busy = [agent(2, Status::Busy)];
        let stays = super::planned(&sessions, &busy, &from, 4, Whole::Tab);
        assert_eq!(stays, Err("pane 2 of its tab: its agent is at work".to_owned()));
        assert!(super::planned(&sessions, &busy, &from, 4, Whole::Pane).is_ok());
    }

    /// Nothing goes of a session that took the name since, nor of a pane that
    /// is gone.
    #[test]
    fn nothing_goes_of_a_session_or_a_pane_gone() {
        let sessions = [work(vec![pane(1, 0, "fish")])];
        let since = planned(&sessions, &[], &seat("work", 200), 1, Whole::Pane);
        assert_eq!(since, Err("work is gone".to_owned()));
        let gone = planned(&sessions, &[], &seat("work", 100), 9, Whole::Pane);
        assert_eq!(gone, Err("pane 9 of work is gone".to_owned()));
    }

    /// A move reads back as it was asked, and a question that moves panes to
    /// their own session, or lands them nowhere, is invalid.
    #[test]
    fn a_move_reads_back_as_it_was_asked() {
        let carry = Carry {
            from: seat("work", 100),
            pane: 3,
            whole: Whole::Tab,
            to: seat("play", 200),
            lands: Lands::Beside(7),
            ending: true,
        };
        let on = Some(On { destination: "mini".to_owned() });
        assert_eq!(Carry::of(carry.asked(on.clone())), Ok((on, carry.clone())));
        let tab = Carry { lands: Lands::Tab, whole: Whole::Pane, ending: false, ..carry.clone() };
        assert_eq!(Carry::of(tab.asked(None)), Ok((None, tab)));
        let home = Carry { to: seat("work", 100), ..carry.clone() };
        assert!(Carry::of(home.asked(None)).unwrap_err().contains("the session it leaves"));
        let nowhere = CarryRequest { lands: None, ..carry.asked(None) };
        assert!(Carry::of(nowhere).unwrap_err().starts_with("lands:"));
        let undated = CarryRequest { to_started_at: None, ..carry.asked(None) };
        assert!(Carry::of(undated).unwrap_err().starts_with("to:"));
    }
}
