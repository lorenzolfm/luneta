use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::num::NonZeroU16;
use std::path::Path;
use std::process;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{mpsc as answers, oneshot};
use tonic::Status;

use crate::agent::Agent;
use crate::api::{self, show_request, show_response};
use crate::core::{self, Command as Core, Located, Shown};
use crate::mux::own::server::{self, Is, Terminals, TerminalsListed, Word};
use crate::mux::own::TerminalKey;
use crate::mux::{Failure, Runs as Opened};
use crate::program::{self, Ran as Looked};
use crate::projects::layout::{self, Direction, Order, Rect, Sides, Towards, SIDE_MAX};
use crate::projects::{Ask, Change, Changed, Closing, Dir, Failed, Home, Id, Line, Placed};
use crate::projects::{Made, Project, ProjectId, Projects, Ran, Runs, Spot};
use crate::projects::{Session, Size, Store, Terminal, TerminalId, Used, View, Workstream};
use crate::projects::{WorkstreamId, FRAME_CELLS, LINE_BYTES_MAX, PANES_MAX, TERMINALS_MAX};
use crate::projects::{Worktree, WORKSTREAMS_MAX};
use crate::shells::Doing;
use crate::shows::direction_said;
use crate::watch::Troubles;
use crate::{opens, Directory};

pub const CLIENTS_MAX: usize = 64;

pub const ANSWERS_MAX: usize = 64;

const COMMANDS_MAX: usize = 64;

const ARGV_MAX: usize = LINE_BYTES_MAX / 2;

const TAG: &str = "luneta terminal ";

const RESTORE_PAUSE: Duration = Duration::from_secs(10);

const RESTORE_ATTEMPTS_MAX: u32 = 3;

const RESTORES_PER_PASS: usize = 8;

const REHOMES_PER_PASS: usize = 4;

const RESTORE_SETTLE: Duration = Duration::from_secs(30);

const TYPING_WAIT: Duration = Duration::from_secs(3);

const TYPING_WAIT_MAX: Duration = Duration::from_secs(30);

const FOLDER_LOOK: Duration = Duration::from_secs(1);

const UNSHOWN_SIZE: Size =
    Size { columns: NonZeroU16::new(80).unwrap(), rows: NonZeroU16::new(24).unwrap() };

const SHELLS: [&str; 9] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh"];

const _: () = assert!(ANSWERS_MAX > 1);
const _: () = assert!(RESTORE_ATTEMPTS_MAX > 0);
const _: () = assert!(RESTORES_PER_PASS > 0);
const _: () = assert!(RESTORES_PER_PASS <= TERMINALS_MAX);
const _: () = assert!(REHOMES_PER_PASS > 0);
const _: () = assert!(REHOMES_PER_PASS <= TERMINALS_MAX);
const _: () = assert!(RESTORE_SETTLE.as_nanos() > RESTORE_PAUSE.as_nanos());
const _: () = assert!(TYPING_WAIT.as_nanos() < TYPING_WAIT_MAX.as_nanos());
const _: () = assert!(UNSHOWN_SIZE.columns.get() <= SIDE_MAX);
const _: () = assert!(UNSHOWN_SIZE.rows.get() <= SIDE_MAX);

pub type Out = answers::Sender<Result<api::ShowResponse, Status>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientId(u64);

pub enum Command {
    Change { change: Ask, answer: oneshot::Sender<Result<Changed, Failed>> },
    Came {
        workstream: WorkstreamId,
        size: Size,
        runs_in: Option<TerminalKey>,
        again: bool,
        out: Out,
        answer: oneshot::Sender<Result<ClientId, Status>>,
    },
    Asked { client: ClientId, asked: Result<Asked, String> },
    Left(ClientId),
    Listed(TerminalsListed),
    Emptied { since: Instant },
    Crashed,
    Unreached { since: Instant },
    Worktrees { root: Dir, worktrees: BTreeSet<Worktree>, seen: BTreeSet<WorkstreamId> },
    Rooted { root: Dir, worktrees: BTreeSet<Worktree> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Size(Size),
    Go(WorkstreamId),
    Open { beside: Beside, argv: Vec<String> },
    Close,
    Focus(Focus),
    Resize { sides: Sides, growth: layout::Growth },
    Move(Step),
    Fullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    Alone,
    Largest,
    Focused,
    Towards(Towards),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Done {
    Quiet,
    Edged(Direction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Step(Step),
    Terminal(TerminalId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Direction(Direction),
    Order(Order),
}

impl Asked {
    pub fn of_api(event: show_request::Event) -> Result<Asked, String> {
        use show_request::Event;
        Ok(match event {
            Event::Hello(_) => return Err("a hello comes only first".to_owned()),
            Event::Size(size) => Asked::Size(size_of(Some(size))?),
            Event::Go(workstream) => Asked::Go(id("go", workstream).map(WorkstreamId::new)?),
            Event::Open(show_request::Open { beside, argv }) => {
                use show_request::open::Beside as Said;
                let beside = match beside.ok_or("open: it says no side to open beside")? {
                    Said::Largest(()) => Beside::Largest,
                    Said::Focused(()) => Beside::Focused,
                    Said::Alone(()) => Beside::Alone,
                    Said::Towards(towards) => Beside::Towards(towards_of(towards)?),
                };
                if argv.first().is_some_and(String::is_empty) {
                    return Err("open: the program to run is named by no word".to_owned());
                }
                if argv.len() > ARGV_MAX {
                    return Err(format!("open: a command line is at most {ARGV_MAX} words"));
                }
                Asked::Open { beside, argv }
            },
            Event::Close(()) => Asked::Close,
            Event::Focus(show_request::Focus { to }) => {
                use show_request::focus::To;
                Asked::Focus(match to.ok_or("focus: it says no terminal to go to")? {
                    To::Direction(direction) => {
                        Focus::Step(Step::Direction(direction_of(direction)?))
                    },
                    To::Order(order) => Focus::Step(Step::Order(order_of(order)?)),
                    To::Terminal(terminal) => {
                        Focus::Terminal(id("focus", terminal).map(TerminalId::new)?)
                    },
                })
            },
            Event::Resize(show_request::Resize { sides, growth }) => {
                use show_request::resize;
                let growth = growth_of(growth)?;
                let sides = match sides.ok_or("resize: it says no side to move")? {
                    resize::Sides::Direction(direction) => Sides::One(direction_of(direction)?),
                    resize::Sides::Every(()) => Sides::Every,
                };
                Asked::Resize { sides, growth }
            },
            Event::Move(show_request::Move { to }) => {
                use show_request::r#move::To;
                Asked::Move(match to.ok_or("move: it says nowhere to move to")? {
                    To::Direction(direction) => Step::Direction(direction_of(direction)?),
                    To::Order(order) => Step::Order(order_of(order)?),
                })
            },
            Event::Fullscreen(()) => Asked::Fullscreen,
        })
    }
}

pub fn size_of(size: Option<show_request::Size>) -> Result<Size, String> {
    let size = size.ok_or("it says no size")?;
    Size::of(size.columns, size.rows).map_err(|why| format!("size: {why}"))
}

pub fn id(field: &str, id: u64) -> Result<Id, String> {
    Id::of(id).ok_or_else(|| format!("{field}: no id is 0"))
}

fn towards_of(said: i32) -> Result<Towards, String> {
    use show_request::Towards as Said;
    match Said::try_from(said) {
        Ok(Said::Right) => Ok(Towards::Right),
        Ok(Said::Down) => Ok(Towards::Down),
        Ok(Said::Unspecified) | Err(_) => Err("open: no side it knows to open towards".to_owned()),
    }
}

pub(crate) fn direction_of(said: i32) -> Result<Direction, String> {
    use show_request::Direction as Said;
    match Said::try_from(said) {
        Ok(Said::Left) => Ok(Direction::Left),
        Ok(Said::Right) => Ok(Direction::Right),
        Ok(Said::Up) => Ok(Direction::Up),
        Ok(Said::Down) => Ok(Direction::Down),
        Ok(Said::Unspecified) | Err(_) => Err("no direction it knows".to_owned()),
    }
}

fn growth_of(said: i32) -> Result<layout::Growth, String> {
    use show_request::Growth as Said;
    match Said::try_from(said) {
        Ok(Said::Grow) => Ok(layout::Growth::Grow),
        Ok(Said::Shrink) => Ok(layout::Growth::Shrink),
        Ok(Said::Unspecified) | Err(_) => Err("resize: it says neither grow nor shrink".to_owned()),
    }
}

fn order_of(said: i32) -> Result<Order, String> {
    use show_request::Order as Said;
    match Said::try_from(said) {
        Ok(Said::Next) => Ok(Order::Next),
        Ok(Said::Previous) => Ok(Order::Previous),
        Ok(Said::Unspecified) | Err(_) => Err("no order it knows".to_owned()),
    }
}

pub fn ask<T>(
    views: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (answer, answered) = oneshot::channel();
    views.send(question(answer)).ok()?;
    answered.blocking_recv().ok()
}

pub fn start(
    core: SyncSender<Core>,
    store: Store,
    follower: SyncSender<()>,
    sweep: Duration,
) -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    let views = Views {
        store,
        core,
        follower,
        mux: Terminals,
        clients: BTreeMap::new(),
        arrivals: 0,
        known: BTreeMap::new(),
        listed: BTreeMap::new(),
        looked: false,
        last_focus: BTreeMap::new(),
        unhomed: BTreeSet::new(),
        restore_round: BTreeSet::new(),
        restore_last_at: None,
        restore_attempts: Attempts::default(),
        told: None,
        troubles: Troubles::default(),
        shell: opens::your_shell(),
    };
    thread::Builder::new().name("views".to_owned()).spawn(move || run(views, &commands, sweep))?;
    Ok(send)
}

struct Views {
    store: Store,
    core: SyncSender<Core>,
    follower: SyncSender<()>,
    mux: Terminals,
    clients: BTreeMap<ClientId, Client>,
    arrivals: u64,
    known: BTreeMap<TerminalId, Known>,
    listed: BTreeMap<TerminalKey, server::Terminal>,
    looked: bool,
    last_focus: BTreeMap<WorkstreamId, TerminalId>,
    unhomed: BTreeSet<TerminalId>,
    restore_round: BTreeSet<TerminalId>,
    restore_last_at: Option<Instant>,
    restore_attempts: Attempts,
    told: Option<BTreeMap<TerminalId, Located>>,
    troubles: Troubles,
    shell: OsString,
}

struct Client {
    workstream: WorkstreamId,
    size: Size,
    runs_in: Option<TerminalKey>,
    focus: Option<TerminalId>,
    out: Out,
    said: Option<api::ShowResponse>,
}

struct Known {
    key: TerminalKey,
    heard: Heard,
    sized: Option<Size>,
    agent_front: Option<u32>,
    remade: Option<Remade>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heard {
    Made(Instant),
    Seen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Remade {
    at: Instant,
    restored: Option<Restored>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Restored {
    Typing { line: Line, due: Instant, by: Instant },
    Resumed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paste {
    Now,
    Later,
    Never,
}

#[derive(Default)]
struct Attempts {
    failures: BTreeMap<TerminalId, u32>,
}

impl Attempts {
    fn wanted(&self, terminal: TerminalId) -> bool {
        self.failures.get(&terminal).is_none_or(|count| *count < RESTORE_ATTEMPTS_MAX)
    }

    fn failed(&mut self, terminal: TerminalId, why: &str) -> String {
        let count = self.charged(terminal);
        if count < RESTORE_ATTEMPTS_MAX {
            let attempt = format!("attempt {count} of {RESTORE_ATTEMPTS_MAX}");
            format!("terminal {terminal} is not restored, on {attempt}: {why}")
        } else {
            let given_up = format!("given up after {RESTORE_ATTEMPTS_MAX} attempts");
            format!("terminal {terminal} is not restored, and is {given_up}: {why}")
        }
    }

    fn crashed(&mut self, terminal: TerminalId, after: Duration) -> String {
        let count = self.charged(terminal);
        let secs = after.as_secs();
        let ended = format!("luneta-mux ended {secs}s after terminal {terminal} was restored");
        if count < RESTORE_ATTEMPTS_MAX {
            format!("{ended}, which counts as attempt {count} of {RESTORE_ATTEMPTS_MAX}")
        } else {
            format!("{ended}, and it is given up after {RESTORE_ATTEMPTS_MAX} attempts")
        }
    }

    fn settled(&mut self, terminal: TerminalId) {
        self.failures.remove(&terminal);
    }

    fn charged(&mut self, terminal: TerminalId) -> u32 {
        assert!(self.wanted(terminal), "terminal {terminal}, given up, is not restored again");
        let count = self.failures.entry(terminal).or_insert(0);
        *count += 1;
        assert!(*count <= RESTORE_ATTEMPTS_MAX, "no terminal is tried past the attempts");
        *count
    }

    fn retain(&mut self, unsettled: impl Fn(TerminalId) -> bool) {
        self.failures.retain(|terminal, _| unsettled(*terminal));
        assert!(self.failures.len() <= TERMINALS_MAX, "no more failures than terminals");
    }
}

fn run(mut views: Views, commands: &Receiver<Command>, sweep: Duration) {
    let mut swept = Instant::now();
    loop {
        let restoring = !views.restore_round.is_empty();
        let wait = if restoring { Duration::ZERO } else { sweep.saturating_sub(swept.elapsed()) };
        match commands.recv_timeout(wait) {
            Ok(command) => views.heard(command),
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                views.flushed();
                return;
            },
        }
        if swept.elapsed() >= sweep {
            views.read_runs();
            views.rehome_due();
            views.flushed();
            swept = Instant::now();
        }
        views.restore();
        views.type_due();
        views.settle();
        views.tell_core();
        views.troubles.done();
    }
}

impl Views {
    fn heard(&mut self, command: Command) {
        match command {
            Command::Change { change, answer } => {
                let changed = match change {
                    Ask::Change(change) => self.store.change(change),
                    Ask::CloseWorkstream { workstream, seen } => {
                        self.close_workstream(workstream, &seen)
                    },
                    Ask::CloseProject { project, workstreams, seen } => {
                        self.close_project(project, &workstreams, &seen)
                    },
                    Ask::Bring { workstream, from } => self.bring(workstream, from),
                };
                let kept = self.store.revision();
                let _ = answer.send(changed.map(|made| Changed { made, kept }));
            },
            Command::Came { workstream, size, runs_in, again, out, answer } => {
                let _ = answer.send(self.came(workstream, size, runs_in, again, out));
            },
            Command::Asked { client, asked } => {
                if !self.clients.contains_key(&client) {
                    return;
                }
                let event = match asked.and_then(|asked| self.asked(client, asked)) {
                    Ok(Done::Quiet) => return,
                    Ok(Done::Edged(direction)) => {
                        let workstream = self.clients[&client].workstream.get();
                        let direction = direction_said(direction).into();
                        show_response::Event::Edged(show_response::Edged { workstream, direction })
                    },
                    Err(why) => show_response::Event::Refused(why),
                };
                let word = api::ShowResponse { event: Some(event) };
                let held = self.clients.get(&client).expect("a client that asked is held");
                if !told(&held.out, word) {
                    self.clients.remove(&client);
                }
            },
            Command::Left(client) => {
                self.clients.remove(&client);
            },
            Command::Listed(listed) => self.heard_listed(listed),
            Command::Emptied { since } => {
                let none = TerminalsListed { terminals: Vec::new(), whole: true, since };
                self.heard_listed(none);
                self.known.clear();
                self.listed.clear();
            },
            Command::Crashed => {
                self.crashed(|_| true);
                self.looked = true;
                self.known.clear();
                self.listed.clear();
            },
            Command::Worktrees { root, worktrees, seen } => {
                let listed = Change::Listed { root: root.clone(), worktrees, seen };
                if let Err(why) = self.followed(listed) {
                    let root = root.as_str();
                    self.troubles.say(format!("{root}: its worktrees are not kept: {why}"));
                }
            },
            Command::Rooted { root, worktrees } => {
                let rooted = Change::Rooted { root: root.clone(), worktrees };
                if let Err(why) = self.followed(rooted) {
                    let root = root.as_str();
                    self.troubles.say(format!("{root}: it is not kept as a clone: {why}"));
                    return;
                }
                let Some(projects) = self.store.projects() else {
                    return;
                };
                let held = projects.all().iter().find(|it| *it.dir() == root);
                let views = held.into_iter().flat_map(Project::workstreams).map(Workstream::view);
                let terminals = views.flat_map(View::terminals).map(layout::Leaf::id);
                self.unhomed.extend(terminals);
                assert!(self.unhomed.len() <= TERMINALS_MAX, "only held terminals wait for a home");
            },
            Command::Unreached { since } => {
                let gone = |known: &Known| match known.heard {
                    Heard::Made(made) => made < since,
                    Heard::Seen => true,
                };
                self.crashed(gone);
                self.looked = true;
                self.known.retain(|_, known| !gone(known));
                self.listed.clear();
            },
        }
    }

    fn came(
        &mut self,
        workstream: WorkstreamId,
        size: Size,
        runs_in: Option<TerminalKey>,
        again: bool,
        out: Out,
    ) -> Result<ClientId, Status> {
        if self.clients.len() >= CLIENTS_MAX {
            let many = format!("{CLIENTS_MAX} clients show views, as many as may");
            return Err(Status::resource_exhausted(many));
        }
        let projects = self.kept().map_err(Status::failed_precondition)?;
        let Some((_, shown)) = projects.find(workstream) else {
            return Err(Status::not_found(format!("no workstream has the id {workstream}")));
        };
        let focus = arrival(shown, self.last_focus.get(&workstream).copied());
        self.arrivals += 1;
        let id = ClientId(self.arrivals);
        let client = Client { workstream, size, runs_in, focus, out, said: None };
        assert!(self.clients.insert(id, client).is_none(), "a client comes once");
        if !again {
            self.went(workstream);
        }
        Ok(id)
    }

    fn went(&mut self, workstream: WorkstreamId) {
        let went = Change::Went { workstream, at: Used::at(SystemTime::now()) };
        if let Err(why) = self.changed(went) {
            self.troubles.say(format!("workstream {workstream}'s use is not kept: {why}"));
        }
    }

    fn close_project(
        &mut self,
        project: ProjectId,
        seen_workstreams: &BTreeSet<WorkstreamId>,
        seen: &BTreeSet<TerminalId>,
    ) -> Result<Option<Made>, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let unknown = || Failed::Refused(format!("no project has the id {project}"));
        let held = projects.project_of(project).ok_or_else(unknown)?;
        let workstreams: Vec<WorkstreamId> = held.workstreams().map(Workstream::id).collect();
        let views = held.workstreams().map(Workstream::view);
        let terminals: Vec<TerminalId> = views.flat_map(View::terminals).map(layout::Leaf::id).collect();
        assert!(workstreams.len() <= WORKSTREAMS_MAX, "a project holds at most {WORKSTREAMS_MAX}");
        assert!(terminals.len() <= TERMINALS_MAX, "a project holds at most {TERMINALS_MAX} terminals");
        let held_workstreams: BTreeSet<WorkstreamId> = workstreams.iter().copied().collect();
        let held_terminals: BTreeSet<TerminalId> = terminals.iter().copied().collect();
        if held_workstreams != *seen_workstreams || held_terminals != *seen {
            let changed = format!("project {project}'s workstreams or terminals are not those seen");
            return Err(Failed::Stale(format!("{changed}: they changed since")));
        }
        self.close_terminals(&terminals)?;
        if let Some(why) = self.store.closed(Closing::Project(project))? {
            self.unwritten(&why);
        }
        let projects = self.store.projects().expect("projects just changed are kept");
        assert!(projects.project_of(project).is_none(), "project {project} is closed");
        for workstream in &workstreams {
            assert!(projects.find(*workstream).is_none(), "workstream {workstream} goes with it");
        }
        self.forget_closed(&workstreams, &terminals);
        Ok(None)
    }

    fn close_workstream(
        &mut self,
        workstream: WorkstreamId,
        seen: &BTreeSet<TerminalId>,
    ) -> Result<Option<Made>, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let unknown = || Failed::Refused(format!("no workstream has the id {workstream}"));
        let (_, shown) = projects.find(workstream).ok_or_else(unknown)?;
        let terminals: Vec<TerminalId> = shown.view().terminals().map(layout::Leaf::id).collect();
        if terminals.iter().copied().collect::<BTreeSet<_>>() != *seen {
            let changed = format!("workstream {workstream}'s terminals are not the ones seen");
            return Err(Failed::Stale(format!("{changed}: they changed since")));
        }
        assert!(terminals.len() <= PANES_MAX, "a view holds at most {PANES_MAX} terminals");
        self.close_terminals(&terminals)?;
        if let Some(why) = self.store.closed(Closing::Workstream(workstream))? {
            self.unwritten(&why);
        }
        let projects = self.store.projects().expect("projects just changed are kept");
        let emptied = projects.find(workstream).is_some_and(|(_, it)| it.view().is_empty());
        assert!(emptied, "workstream {workstream} is closed, and stays empty");
        self.forget_closed(&[workstream], &terminals);
        Ok(None)
    }

    fn close_terminals(&mut self, terminals: &[TerminalId]) -> Result<(), Failed> {
        assert!(terminals.len() <= TERMINALS_MAX, "at most {TERMINALS_MAX} terminals are kept");
        let unclosed = |failure: Failure| Failed::Unclosed(failure.to_string());
        let unseen = terminals.iter().any(|terminal| !self.known.contains_key(terminal));
        let tags = if unseen { self.mux.tags().map_err(unclosed)? } else { Vec::new() };
        let mut done: Vec<TerminalId> = Vec::with_capacity(terminals.len());
        for terminal in terminals {
            let known = self.known.get(terminal).map(|known| known.key);
            let found = || {
                let found = tags.iter().find(|(tag, _)| tagged(tag) == Some(*terminal));
                found.map(|(_, key)| *key)
            };
            if let Some(key) = known.or_else(found)
                && let Err(failure) = self.mux.closed(key)
            {
                for closed in done {
                    if let Err(why) = self.closed(closed) {
                        self.troubles.say(format!("terminal {closed} closed, and stays: {why}"));
                    }
                }
                return Err(unclosed(failure));
            }
            done.push(*terminal);
        }
        assert_eq!(done, terminals, "each terminal is closed in the mux, in order");
        Ok(())
    }

    fn forget_closed(&mut self, workstreams: &[WorkstreamId], terminals: &[TerminalId]) {
        for terminal in terminals {
            self.known.remove(terminal);
        }
        for workstream in workstreams {
            self.last_focus.remove(workstream);
        }
        let projects = self.store.projects().expect("projects just changed are kept");
        for workstream in workstreams {
            let focused = self.last_focus.contains_key(workstream);
            assert!(!focused, "closed workstream {workstream} has no focus");
        }
        for terminal in terminals {
            assert!(projects.holding(*terminal).is_none(), "terminal {terminal} left its view");
            assert!(!self.known.contains_key(terminal), "terminal {terminal} is known no more");
        }
    }

    fn bring(
        &mut self,
        workstream: WorkstreamId,
        from: TerminalKey,
    ) -> Result<Option<Made>, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        if projects.find(workstream).is_none() {
            return Err(Failed::Refused(format!("no workstream has the id {workstream}")));
        }
        let held = self.known.iter().find(|(_, known)| known.key == from);
        let Some((&held, _)) = held else {
            return Ok(None);
        };
        let sent = api::ShowResponse { event: Some(show_response::Event::Sent(workstream.get())) };
        let on = self.clients.iter().filter(|(_, it)| it.focus == Some(held)).map(|(id, _)| *id);
        let on: Vec<ClientId> = on.collect();
        let mut taken = false;
        for client in on {
            let it = &self.clients[&client];
            if it.workstream == workstream || told(&it.out, sent.clone()) {
                taken = true;
            } else {
                self.clients.remove(&client);
            }
        }
        Ok(taken.then_some(Made::Taken(workstream)))
    }

    fn kept(&self) -> Result<&Projects, String> {
        match self.store.projects() {
            Some(projects) => Ok(projects),
            None => Err(self.store.unkept().unwrap_or("no projects are kept").to_owned()),
        }
    }

    fn changed(&mut self, change: Change) -> Result<(), String> {
        self.store.change(change).map(drop).map_err(said)
    }

    fn followed(&mut self, change: Change) -> Result<(), String> {
        if let Some(why) = self.store.followed(change).map_err(said)? {
            self.unwritten(&why);
        }
        Ok(())
    }

    fn unwritten(&mut self, why: &str) {
        self.troubles.say(format!("the views are kept, and written on the next sweep: {why}"));
    }

    fn flushed(&mut self) {
        if let Err(failed) = self.store.flush() {
            self.troubles.say(format!("the views are not written: {}", said(failed)));
        }
    }

    fn asked(&mut self, client: ClientId, asked: Asked) -> Result<Done, String> {
        let workstream = self.clients[&client].workstream;
        let size = self.size_of(client);
        let focus = self.clients[&client].focus;
        let focused = || focus.ok_or_else(|| "the view holds no terminal".to_owned());
        match asked {
            Asked::Size(size) => self.client(client).size = size,
            Asked::Go(to) => {
                let projects = self.kept()?;
                let (_, shown) = projects.find(to).ok_or_else(|| format!("no workstream {to}"))?;
                let focus = arrival(shown, self.last_focus.get(&to).copied());
                let moved = self.client(client);
                moved.workstream = to;
                moved.focus = focus;
                self.went(to);
            },
            Asked::Open { beside, argv } => self.open(client, beside, &argv)?,
            Asked::Close => self.close(focused()?)?,
            Asked::Focus(Focus::Terminal(terminal)) => {
                if !self.view(workstream)?.holds(terminal) {
                    return Err(format!("the view holds no terminal {terminal}"));
                }
                self.focus(client, terminal)?;
            },
            Asked::Focus(Focus::Step(Step::Direction(direction))) => {
                let tree = self.view(workstream)?.tree();
                let beside = tree.zip(focus).and_then(|(tree, from)| {
                    tree.neighbour(from, direction, size)
                });
                let Some(to) = beside else {
                    return Ok(Done::Edged(direction));
                };
                self.focus(client, to)?;
            },
            Asked::Focus(Focus::Step(step @ Step::Order(_))) => {
                let from = focused()?;
                let to = self.stepped(workstream, from, step, size)?;
                self.focus(client, to)?;
            },
            Asked::Resize { sides, growth } => {
                let Some(terminal) = focus else {
                    return Ok(Done::Quiet);
                };
                let view = self.view(workstream)?;
                let moves = view.full().is_none()
                    && view.tree().is_some_and(|tree| {
                        tree.clone().resize(terminal, sides, growth, size)
                    });
                if !moves {
                    return Ok(Done::Quiet);
                }
                self.changed(Change::Resize { terminal, sides, growth, size })?;
            },
            Asked::Move(step) => {
                let one = focused()?;
                let other = self.stepped(workstream, one, step, size)?;
                self.changed(Change::Swap { one, other })?;
            },
            Asked::Fullscreen => {
                let full = match self.view(workstream)?.full() {
                    Some(_) => None,
                    None => Some(focused()?),
                };
                self.changed(Change::Fill { workstream, full })?;
            },
        }
        Ok(Done::Quiet)
    }

    fn client(&mut self, client: ClientId) -> &mut Client {
        self.clients.get_mut(&client).expect("a client asked is held")
    }

    fn view(&self, workstream: WorkstreamId) -> Result<&View, String> {
        let projects = self.kept()?;
        let found = projects.find(workstream).ok_or_else(|| format!("no workstream {workstream}"));
        let (_, found) = found?;
        Ok(found.view())
    }

    fn stepped(
        &self,
        workstream: WorkstreamId,
        from: TerminalId,
        step: Step,
        size: Size,
    ) -> Result<TerminalId, String> {
        let tree = self.view(workstream)?.tree().ok_or("the view holds no terminal")?;
        let to = match step {
            Step::Direction(direction) => tree.neighbour(from, direction, size),
            Step::Order(order) => tree.in_order(from, order),
        };
        to.ok_or_else(|| "no other terminal is there".to_owned())
    }

    fn focus(&mut self, client: ClientId, terminal: TerminalId) -> Result<(), String> {
        let workstream = self.clients[&client].workstream;
        let full = self.view(workstream)?.full();
        if full.is_some_and(|full| full != terminal) {
            self.changed(Change::Fill { workstream, full: None })?;
        }
        self.client(client).focus = Some(terminal);
        self.last_focus.insert(workstream, terminal);
        Ok(())
    }

    fn size_of(&self, client: ClientId) -> Size {
        let Client { workstream, size, .. } = self.clients[&client];
        let Some((_, found)) = self.store.projects().and_then(|it| it.find(workstream)) else {
            return size;
        };
        let shown = self.clients.values().filter(|it| it.workstream == workstream);
        fit(shown, found.view(), &self.known).map_or(size, |fit| fit.size)
    }

    fn open(&mut self, client: ClientId, beside: Beside, argv: &[String]) -> Result<(), String> {
        let runs = match argv {
            [] => Runs::Shell,
            _ => Runs::Command(Line::parse(&quoted(argv)).map_err(|why| format!("open: {why}"))?),
        };
        let Client { workstream, focus, .. } = self.clients[&client];
        let size = self.size_of(client);
        let terminal = self.store.fresh().map_err(said)?;
        let projects = self.kept()?;
        let (project, shown) = projects.find(workstream).ok_or("the workstream is gone")?;
        let (placed, inner) = match (shown.view().tree(), beside) {
            (None, _) => (Placed::Alone, layout::lone(size)),
            (Some(_), Beside::Alone) => return Ok(()),
            (Some(tree), Beside::Towards(towards)) => {
                let at = focus.ok_or("the view holds no terminal to open beside")?;
                let room = tree.room(at, towards, terminal, size);
                let inner = room.ok_or_else(|| format!("terminal {at} has no room beside it"))?;
                (Placed::Beside { at, towards }, inner)
            },
            (Some(tree), Beside::Focused) => {
                let at = focus.ok_or("the view holds no terminal to open beside")?;
                let room = tree.room_beside(at, terminal, size);
                let roomless = || format!("terminal {at} has no room beside it");
                let (towards, inner) = room.ok_or_else(roomless)?;
                (Placed::Beside { at, towards }, inner)
            },
            (Some(tree), Beside::Largest) => {
                let room = tree.room_beside_largest(terminal, size);
                let (at, towards, inner) = room.ok_or("no terminal has room for another")?;
                (Placed::Beside { at, towards }, inner)
            },
        };
        let home = project.folder(shown.id()).ok_or("its project holds it not")?;
        let listed_cwd = focus
            .and_then(|focus| self.known.get(&focus))
            .and_then(|known| self.listed.get(&known.key))
            .and_then(|listed| Dir::parse(&listed.cwd).ok())
            .filter(|cwd| home.below(cwd).is_some());
        let folder = listed_cwd.unwrap_or(home);
        let cwd = Directory::parse(folder.as_str().to_owned()).expect("a dir is absolute");
        let made = Terminal::new(terminal, folder, runs);
        self.changed(Change::Open { workstream, terminal: made, placed, size })?;
        let opened = match argv.split_first() {
            None => Opened::Shell(&self.shell),
            Some((program, rest)) => Opened::Command { program, args: rest },
        };
        let key = match self.mux.made(&cwd, opened, tag(terminal), inner) {
            Ok(key) => key,
            Err(failure) => {
                if let Err(why) = self.followed(Change::Unopen { terminal }) {
                    self.troubles.say(format!("terminal {terminal} stays lost: {why}"));
                }
                return Err(failure.to_string());
            },
        };
        self.made(terminal, key, Some(inner), None);
        self.client(client).focus = Some(terminal);
        self.last_focus.insert(workstream, terminal);
        Ok(())
    }

    fn made(
        &mut self,
        terminal: TerminalId,
        key: TerminalKey,
        sized: Option<Size>,
        remade: Option<Remade>,
    ) {
        let heard_none = !self.known.values().any(|known| known.heard == Heard::Seen);
        let heard = Heard::Made(Instant::now());
        let known = Known { key, heard, sized, agent_front: None, remade };
        assert!(self.known.insert(terminal, known).is_none(), "terminal {terminal} is made once");
        if heard_none {
            match self.follower.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => {},
                Err(TrySendError::Disconnected(())) => {
                    self.troubles.say("the mux's terminals are followed no more".to_owned());
                },
            }
        }
    }

    fn crashed(&mut self, gone: impl Fn(&Known) -> bool) {
        let now = Instant::now();
        let Views { known, restore_attempts, troubles, .. } = self;
        for (terminal, known) in known.iter().filter(|(_, known)| gone(known)) {
            let after = known.remade.as_ref().map(|it| now.saturating_duration_since(it.at));
            match after {
                Some(after) if after < RESTORE_SETTLE => {
                    troubles.say(restore_attempts.crashed(*terminal, after));
                },
                Some(_) | None => restore_attempts.settled(*terminal),
            }
        }
    }

    fn restore(&mut self) {
        if !self.looked {
            return;
        }
        if self.restore_round.is_empty() {
            self.restore_begin();
        }
        self.restore_some();
    }

    fn restore_begin(&mut self) {
        assert!(self.restore_round.is_empty(), "a round begins once the last one is done");
        if self.restore_last_at.is_some_and(|at| at.elapsed() < RESTORE_PAUSE) {
            return;
        }
        let Some(projects) = self.store.projects() else {
            return;
        };
        let known = &self.known;
        let held = |terminal| projects.holding(terminal).is_some();
        let settled = |terminal: TerminalId| {
            known.get(&terminal).is_some_and(|known| {
                known.remade.as_ref().is_none_or(|it| it.at.elapsed() >= RESTORE_SETTLE)
            })
        };
        self.restore_attempts.retain(|terminal| held(terminal) && !settled(terminal));
        let attempts = &self.restore_attempts;
        let wanted = |terminal: &TerminalId| {
            !known.contains_key(terminal) && attempts.wanted(*terminal)
        };
        let ids = || projects.terminals().map(|(_, terminal)| layout::Leaf::id(terminal));
        if !ids().any(|terminal| wanted(&terminal)) {
            return;
        }
        let lost: BTreeSet<TerminalId> = ids().filter(wanted).collect();
        assert!(!lost.is_empty(), "a round begins with a terminal lost");
        self.restore_last_at = Some(Instant::now());
        let tags = match self.mux.tags() {
            Ok(tags) => tags,
            Err(failure) => {
                for terminal in &lost {
                    let said = self.restore_attempts.failed(*terminal, &failure.to_string());
                    self.troubles.say(said);
                }
                return;
            },
        };
        let mut adopted = BTreeSet::new();
        for (tag, key) in tags {
            let Some(terminal) = tagged(&tag).filter(|terminal| lost.contains(terminal)) else {
                continue;
            };
            if self.known.values().any(|known| known.key == key) || !adopted.insert(terminal) {
                continue;
            }
            self.made(terminal, key, None, None);
        }
        assert!(adopted.is_subset(&lost), "only a lost terminal is adopted");
        let keys: BTreeSet<TerminalKey> = self.known.values().map(|known| known.key).collect();
        assert_eq!(keys.len(), self.known.len(), "no terminal adopted shares a key");
        self.restore_round = lost.difference(&adopted).copied().collect();
    }

    fn restore_some(&mut self) {
        let mut remade = 0;
        while remade < RESTORES_PER_PASS {
            let Some(terminal) = self.restore_round.pop_first() else {
                break;
            };
            if self.known.contains_key(&terminal) {
                continue;
            }
            let held = self.store.projects().is_some_and(|it| it.holding(terminal).is_some());
            if !held {
                continue;
            }
            remade += 1;
            if let Err(why) = self.remade(terminal) {
                let said = self.restore_attempts.failed(terminal, &why);
                self.troubles.say(said);
            }
        }
        assert!(remade <= RESTORES_PER_PASS, "a pass restores no more than its share");
    }

    fn remade(&mut self, terminal: TerminalId) -> Result<(), String> {
        let projects = self.kept()?;
        let (held_in, held) = projects.holding(terminal).ok_or("no view holds it")?;
        let (project, workstream) = projects.find(held_in.id()).expect("a view's workstream is");
        let workstream_folder = project.folder(workstream.id()).ok_or("its project holds it not")?;
        let kept_folder = held.folder().clone();
        let runs = held.runs().clone();
        let view = workstream.view();
        let shown = self.clients.values().filter(|it| it.workstream == held_in.id());
        let size = fit(shown, view, &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let inner = placed(view, terminal, size).expect("a view lays out the terminals it holds");
        let folder = if is_folder(&kept_folder)? {
            kept_folder
        } else if is_folder(&workstream_folder)? {
            workstream_folder
        } else {
            let (kept, theirs) = (kept_folder.as_str(), workstream_folder.as_str());
            return Err(format!("neither {kept} nor its workstream's {theirs} is a folder"));
        };
        let cwd = Directory::parse(folder.as_str().to_owned()).expect("a dir is absolute");
        let args: Vec<String>;
        let (opened, restore) = match &runs {
            Runs::Shell => (Opened::Shell(&self.shell), None),
            Runs::Command(line) | Runs::Unrun(line) => {
                let restore = if line.as_str().contains('\n') {
                    let lines = format!("terminal {terminal}'s last command spans lines");
                    self.troubles.say(format!("{lines}, and is kept, not typed"));
                    None
                } else {
                    let now = Instant::now();
                    let (due, by) = (now + TYPING_WAIT, now + TYPING_WAIT_MAX);
                    Some(Restored::Typing { line: line.clone(), due, by })
                };
                (Opened::Shell(&self.shell), restore)
            },
            Runs::Agent { harness, session } => {
                args = vec![harness.resumes().to_owned(), session.as_str().to_owned()];
                let program = harness.program();
                (Opened::Command { program, args: &args }, Some(Restored::Resumed))
            },
        };
        let key = self.mux.made(&cwd, opened, tag(terminal), inner).map_err(|it| it.to_string())?;
        let remade = Remade { at: Instant::now(), restored: restore };
        self.made(terminal, key, Some(inner), Some(remade));
        assert_eq!(self.known.get(&terminal).map(|it| it.key), Some(key), "a remade one is known");
        if let Runs::Command(line) = runs {
            let unrun = Ran { terminal, folder, runs: Runs::Unrun(line) };
            if let Err(why) = self.changed(Change::Ran(vec![unrun])) {
                self.troubles.say(format!("terminal {terminal}'s line is not kept as unrun: {why}"));
            }
        }
        Ok(())
    }

    fn type_due(&mut self) {
        let Views { known, listed, mux, troubles, .. } = self;
        let now = Instant::now();
        let mut pastes_due: Vec<(TerminalId, TerminalKey, Line)> = Vec::new();
        for (terminal, known) in known.iter_mut() {
            let Some(remade) = &mut known.remade else {
                continue;
            };
            let Some(Restored::Typing { line, due, by }) = &remade.restored else {
                continue;
            };
            let is = listed.get(&known.key).map(|listed| &listed.is);
            match pasted(is, *due, *by, now) {
                Paste::Later => continue,
                Paste::Now => pastes_due.push((*terminal, known.key, line.clone())),
                Paste::Never => {
                    let untyped = format!("terminal {terminal}'s last command is not typed");
                    troubles.say(format!("{untyped}, as it ran or runs another program first"));
                },
            }
            remade.restored = None;
        }
        assert!(pastes_due.len() <= known.len(), "a paste is due only where one is restored");
        if pastes_due.is_empty() {
            return;
        }
        let pastes: Vec<(TerminalKey, &Line)> =
            pastes_due.iter().map(|(_, key, line)| (*key, line)).collect();
        if let Err(failure) = mux.typed(&pastes) {
            let terminals: Vec<String> =
                pastes_due.iter().map(|(terminal, ..)| terminal.to_string()).collect();
            let kept = format!("the last commands of terminals {} are kept", terminals.join(", "));
            troubles.say(format!("{kept}, maybe not typed: {failure}"));
        }
    }

    fn close(&mut self, terminal: TerminalId) -> Result<(), String> {
        let key = match self.known.get(&terminal) {
            Some(known) => Some(known.key),
            None => self.mux.tagged(&tag(terminal)).map_err(|failure| {
                format!("terminal {terminal} is not looked for in luneta-mux: {failure}")
            })?,
        };
        if let Some(key) = key {
            self.mux.closed(key).map_err(|failure| failure.to_string())?;
        }
        self.closed(terminal)
    }

    fn closed(&mut self, terminal: TerminalId) -> Result<(), String> {
        let projects = self.kept()?;
        let (held_in, _) = projects.holding(terminal).ok_or("no view holds the terminal")?;
        let workstream = held_in.id();
        let tree = held_in.view().tree().expect("a view that holds a terminal is laid");
        let heir = tree.heir(terminal);
        self.followed(Change::Close { terminal })?;
        self.known.remove(&terminal);
        for client in self.clients.values_mut().filter(|client| client.focus == Some(terminal)) {
            client.focus = heir;
        }
        if self.last_focus.get(&workstream) == Some(&terminal) {
            match heir {
                Some(heir) => self.last_focus.insert(workstream, heir),
                None => self.last_focus.remove(&workstream),
            };
        }
        let projects = self.kept()?;
        assert!(projects.holding(terminal).is_none(), "terminal {terminal} left its view");
        if let Some(heir) = heir {
            assert!(projects.holding(heir).is_some(), "terminal {heir} took the room it left");
        } else {
            let emptied = projects.find(workstream).is_some_and(|(_, it)| it.view().is_empty());
            assert!(emptied, "workstream {workstream} stays, empty, after its last terminal");
        }
        let focused = self.clients.values().any(|client| client.focus == Some(terminal));
        assert!(!focused, "no client focuses terminal {terminal}, which closed");
        Ok(())
    }

    fn heard_listed(&mut self, listed: TerminalsListed) {
        let TerminalsListed { terminals, whole, since } = listed;
        self.looked |= whole;
        for known in self.known.values_mut() {
            if let Heard::Made(made) = known.heard
                && made < since
            {
                known.heard = Heard::Seen;
            }
        }
        let Some(projects) = self.store.projects() else {
            self.listed = by_key(terminals);
            return;
        };
        let mut found: BTreeMap<TerminalId, (TerminalKey, Option<Size>)> = BTreeMap::new();
        for listed in &terminals {
            let Some(terminal) = tagged(&listed.tag) else {
                continue;
            };
            if projects.holding(terminal).is_none() {
                let id = listed.key.id;
                let unheld = format!("terminal {id} is tagged {terminal}, which no view holds");
                self.troubles.say(unheld);
                continue;
            }
            if found.contains_key(&terminal) {
                let why = format!("two terminals are tagged {terminal}; the first is kept");
                self.troubles.say(why);
                continue;
            }
            found.insert(terminal, (listed.key, listed.size));
        }
        let ended: Vec<TerminalId> = if whole {
            let gone = |(terminal, known): &(&TerminalId, &Known)| {
                !found.contains_key(*terminal) && known.heard == Heard::Seen
            };
            self.known.iter().filter(gone).map(|(terminal, _)| *terminal).collect()
        } else {
            let unread = "a listing is not read whole, and so no terminal is ended";
            self.troubles.say(unread.to_owned());
            Vec::new()
        };
        let owners: BTreeMap<TerminalKey, TerminalId> =
            found.iter().map(|(terminal, (key, _))| (*key, *terminal)).collect();
        self.known.retain(|terminal, known| {
            owners.get(&known.key).is_none_or(|owner| owner == terminal)
        });
        for (terminal, (key, size)) in found {
            match self.known.get_mut(&terminal) {
                Some(known) if known.key == key => {
                    known.heard = Heard::Seen;
                    if size.is_some() {
                        known.sized = size;
                    }
                },
                Some(_) | None => {
                    let heard = Heard::Seen;
                    let known = Known { key, heard, sized: size, agent_front: None, remade: None };
                    self.known.insert(terminal, known);
                },
            }
        }
        let keys: BTreeSet<TerminalKey> = self.known.values().map(|known| known.key).collect();
        assert_eq!(keys.len(), self.known.len(), "no two terminals share a key of luneta-mux");
        self.listed = by_key(terminals);
        for terminal in ended {
            if let Err(why) = self.closed(terminal) {
                let stays = format!("terminal {terminal} ended, and stays in its view");
                self.troubles.say(format!("{stays}: {why}"));
            }
        }
        self.read_runs();
    }

    fn read_runs(&mut self) {
        if self.listed.is_empty() {
            return;
        }
        let agents = core::ask(&self.core, Core::AllAgents).unwrap_or_default();
        let Views { store, known, listed, shell, .. } = self;
        let Some(projects) = store.projects() else {
            return;
        };
        let mut ran = Vec::new();
        let mut elsewhere = Vec::new();
        for (terminal, known) in known.iter_mut() {
            let Some(listed) = listed.get(&known.key) else {
                continue;
            };
            let Some((_, held)) = projects.holding(*terminal) else {
                continue;
            };
            let folder = Dir::parse(&listed.cwd).unwrap_or_else(|_| held.folder().clone());
            let mut runs = runs_of(listed, &agents, held.runs(), shell);
            let front = match &listed.is {
                Is::Running { front: Some(front), .. } => Some(front.pid),
                Is::Running { front: None, .. } | Is::Ended => None,
            };
            let was_agent = matches!(held.runs(), Runs::Agent { .. });
            let same_front = front.is_some() && front == known.agent_front;
            if was_agent && !matches!(runs, Runs::Agent { .. }) && same_front {
                runs = held.runs().clone();
            }
            let restored = known.remade.as_ref().and_then(|remade| remade.restored.as_ref());
            if keeps(restored, held.runs(), &listed.is, &runs) {
                runs = held.runs().clone();
            } else if let Some(remade) = &mut known.remade {
                remade.restored = None;
            }
            known.agent_front = front.filter(|_| matches!(runs, Runs::Agent { .. }));
            if folder != *held.folder() {
                elsewhere.push(*terminal);
            }
            if folder != *held.folder() || runs != *held.runs() {
                ran.push(Ran { terminal: *terminal, folder, runs });
            }
        }
        assert!(ran.len() <= TERMINALS_MAX, "a read runs no more terminals than are held");
        if ran.is_empty() {
            return;
        }
        if let Err(why) = self.changed(Change::Ran(ran)) {
            self.troubles.say(format!("what the terminals run is not kept: {why}"));
            return;
        }
        self.unhomed.extend(elsewhere);
    }

    fn rehome_due(&mut self) {
        let Some(projects) = self.store.projects() else {
            return;
        };
        self.unhomed.retain(|terminal| projects.holding(*terminal).is_some());
        assert!(self.unhomed.len() <= TERMINALS_MAX, "only held terminals wait for a home");
        for _ in 0..REHOMES_PER_PASS {
            let Some(terminal) = self.unhomed.pop_first() else {
                return;
            };
            self.rehome(terminal);
        }
    }

    fn rehome(&mut self, terminal: TerminalId) {
        let Some(projects) = self.store.projects() else {
            return;
        };
        let (_, held) = projects.holding(terminal).expect("a terminal waiting for a home is held");
        let folder = held.folder().clone();
        let stays = format!("terminal {terminal}, gone to {}, stays", folder.as_str());
        let spot = match Spot::of(&folder, FOLDER_LOOK) {
            Ok(spot) => spot,
            Err(why) => return self.troubles.say(format!("{stays}: its worktree is unknown: {why}")),
        };
        let projects = self.store.projects().expect("projects just looked at are kept");
        let to = match projects.home(terminal, &spot) {
            Home::Stays => return,
            Home::In(to) => to,
            Home::Made { project, spot } => {
                let unmade = format!("no workstream is made at {}", spot.place().as_str());
                let add = Change::AddWorkstream { project, spot, name: None };
                match self.store.change(add) {
                    Ok(Some(Made::Workstream(made))) => made,
                    Ok(_) => unreachable!("a workstream added is made"),
                    Err(failed) => {
                        return self.troubles.say(format!("{stays}: {unmade}: {}", said(failed)));
                    },
                }
            },
        };
        if let Err(why) = self.moved(terminal, to) {
            self.troubles.say(format!("{stays}: {why}"));
        }
    }

    fn moved(&mut self, terminal: TerminalId, to: WorkstreamId) -> Result<(), String> {
        let projects = self.kept()?;
        let (held_in, _) = projects.holding(terminal).ok_or("no view holds it")?;
        let from = held_in.id();
        let heir = held_in.view().tree().and_then(|tree| tree.heir(terminal));
        let (_, target) = projects.find(to).ok_or_else(|| format!("no workstream {to}"))?;
        let follows = |client: &Client| client.workstream == from && client.focus == Some(terminal);
        let shown = self.clients.values().filter(|it| it.workstream == to || follows(it));
        let size = fit(shown, target.view(), &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        self.changed(Change::Move { terminal, to, size })?;
        let mut followed = false;
        for client in self.clients.values_mut() {
            if follows(client) {
                client.workstream = to;
                followed = true;
            }
        }
        if self.last_focus.get(&from) == Some(&terminal) {
            match heir {
                Some(heir) => self.last_focus.insert(from, heir),
                None => self.last_focus.remove(&from),
            };
        }
        if followed {
            self.last_focus.insert(to, terminal);
            self.went(to);
        }
        let projects = self.store.projects().expect("projects just changed are kept");
        let held_in = projects.holding(terminal).map(|(it, _)| it.id());
        assert_eq!(held_in, Some(to), "terminal {terminal} moved to workstream {to}");
        assert!(!self.clients.values().any(follows), "no client of {from} focuses {terminal}");
        Ok(())
    }

    fn tell_core(&mut self) {
        let Some(projects) = self.store.projects() else {
            return;
        };
        let located = |(_, terminal): (&Workstream, &Terminal)| {
            let id = layout::Leaf::id(terminal);
            let located = match self.known.get(&id) {
                Some(known) => {
                    let listed = self.listed.get(&known.key).cloned().map(Box::new);
                    Located::Shown(Shown { key: known.key, listed })
                },
                None if self.looked => Located::Lost,
                None => Located::Looking,
            };
            (id, located)
        };
        let now: BTreeMap<TerminalId, Located> = projects.terminals().map(located).collect();
        if self.told.as_ref() == Some(&now) {
            return;
        }
        if self.core.send(Core::Terminals(now.clone())).is_err() {
            self.troubles.say("the core hears the views' terminals no more".to_owned());
            return;
        }
        self.told = Some(now);
    }

    fn settle(&mut self) {
        let Views { store, clients, known, looked, last_focus, mux, troubles, .. } = self;
        let Some(projects) = store.projects() else {
            let why = store.unkept().unwrap_or("no projects are kept").to_owned();
            for (_, client) in std::mem::take(clients) {
                let _ = client.out.try_send(Err(Status::failed_precondition(why.clone())));
            }
            return;
        };
        let mut gone = Vec::new();
        for (id, client) in clients.iter_mut() {
            let Some((_, shown)) = projects.find(client.workstream) else {
                let forgotten = format!("workstream {} is forgotten", client.workstream);
                let _ = client.out.try_send(Err(Status::not_found(forgotten)));
                gone.push(*id);
                continue;
            };
            let view = shown.view();
            if let Some(full) = view.full() {
                client.focus = Some(full);
            } else if !client.focus.is_some_and(|focus| view.holds(focus)) {
                client.focus = arrival(shown, last_focus.get(&client.workstream).copied());
            }
        }
        for id in gone {
            clients.remove(&id);
        }
        last_focus.retain(|workstream, terminal| {
            projects.find(*workstream).is_some_and(|(_, it)| it.view().holds(*terminal))
        });
        let workstreams: BTreeSet<_> = clients.values().map(|it| it.workstream).collect();
        let mut fits: BTreeMap<WorkstreamId, Fit> = BTreeMap::new();
        for workstream in workstreams {
            let (_, found) = projects.find(workstream).expect("a client's workstream is held");
            let shown = clients.values().filter(|it| it.workstream == workstream);
            let fitted = fit(shown, found.view(), known).expect("a workstream shown has a client");
            fits.insert(workstream, fitted);
        }
        for (&workstream, &Fit { size, outside }) in &fits {
            let (_, found) = projects.find(workstream).expect("a client's workstream is held");
            let Some(tree) = found.view().tree() else {
                continue;
            };
            let whole = Rect::whole(tree.fitted(size)).inner();
            for (terminal, rect) in tree.laid(size) {
                let terminal = layout::Leaf::id(terminal);
                let full = found.view().full() == Some(terminal);
                let wanted = if full { whole } else { rect.inner() };
                let Some(known) = known.get_mut(&terminal) else {
                    continue;
                };
                let runs_in = |it: &Client| {
                    it.workstream == workstream && it.runs_in == Some(known.key)
                };
                let pinned = !outside && clients.values().any(runs_in);
                if known.sized == Some(wanted) || pinned {
                    continue;
                }
                match mux.sized(known.key, wanted) {
                    Ok(()) => known.sized = Some(wanted),
                    Err(failure) => {
                        troubles.say(format!("terminal {} is not sized: {failure}", known.key.id));
                    },
                }
            }
        }
        let mut slow = Vec::new();
        for (id, client) in clients.iter_mut() {
            let found = projects.find(client.workstream);
            let (_, found) = found.expect("a client's workstream is held");
            let size = fits[&client.workstream].size;
            let laid = laid(found, size, client.focus, known, *looked);
            if client.said.as_ref() == Some(&laid) {
                continue;
            }
            if !told(&client.out, laid.clone()) {
                slow.push(*id);
                continue;
            }
            client.said = Some(laid);
        }
        for id in slow {
            clients.remove(&id);
        }
        assert!(clients.len() <= CLIENTS_MAX, "no more than {CLIENTS_MAX} clients show views");
    }
}

#[derive(Clone, Copy)]
struct Fit {
    size: Size,
    outside: bool,
}

fn fit<'a>(
    shown: impl Iterator<Item = &'a Client>,
    view: &View,
    known: &BTreeMap<TerminalId, Known>,
) -> Option<Fit> {
    shown.fold(None, |fitted, client| {
        let outside = !inside(client, view, known);
        let size = match fitted {
            Some(Fit { outside: true, .. }) if !outside => return fitted,
            Some(fitted) if fitted.outside == outside => fitted.size.least(client.size),
            Some(_) | None => client.size,
        };
        Some(Fit { size, outside })
    })
}

fn inside(client: &Client, view: &View, known: &BTreeMap<TerminalId, Known>) -> bool {
    let holds = |runs_in| known.iter().any(|(id, it)| it.key == runs_in && view.holds(*id));
    client.runs_in.is_some_and(holds)
}

fn placed(view: &View, terminal: TerminalId, size: Size) -> Option<Size> {
    let tree = view.tree()?;
    let fitted = tree.fitted(size);
    let inner = if view.full() == Some(terminal) {
        Rect::whole(fitted).inner()
    } else {
        let mut laid = tree.laid(size).into_iter();
        laid.find(|(it, _)| layout::Leaf::id(*it) == terminal).map(|(_, rect)| rect.inner())?
    };
    let ((columns, rows), (columns_fitted, rows_fitted)) = (inner.get(), fitted.get());
    assert!(columns + 2 * FRAME_CELLS <= columns_fitted, "a terminal fits its view's width");
    assert!(rows + 2 * FRAME_CELLS <= rows_fitted, "a terminal fits its view's height");
    Some(inner)
}

fn keeps(restored: Option<&Restored>, held: &Runs, is: &Is, read: &Runs) -> bool {
    match restored {
        Some(Restored::Typing { .. }) => true,
        Some(Restored::Resumed) => !matches!(read, Runs::Agent { .. }),
        None => matches!(held, Runs::Unrun(_)) && !ran_since(is),
    }
}

fn pasted(is: Option<&Is>, due: Instant, by: Instant, now: Instant) -> Paste {
    assert!(due <= by, "a paste is due before it is given up");
    let Some(is) = is else {
        return if now >= due { Paste::Now } else { Paste::Later };
    };
    if ran(is) {
        return Paste::Never;
    }
    if runs_in_front(is) {
        return if now >= by { Paste::Never } else { Paste::Later };
    }
    let prompts = matches!(is, Is::Running { word: Some(Word::Prompts), .. });
    if prompts || now >= due { Paste::Now } else { Paste::Later }
}

fn ran_since(is: &Is) -> bool {
    ran(is) || runs_in_front(is)
}

fn ran(is: &Is) -> bool {
    match is {
        Is::Running { doing, .. } => {
            matches!(doing, Some(Doing::Ended { .. } | Doing::Stopped { .. }))
        },
        Is::Ended => true,
    }
}

fn runs_in_front(is: &Is) -> bool {
    match is {
        Is::Running { root, front, word, doing } => {
            matches!(word, Some(Word::Going(_)))
                || matches!(doing, Some(Doing::Going { .. }))
                || front.as_ref().is_some_and(|it| it.pid != *root)
        },
        Is::Ended => false,
    }
}

fn is_folder(folder: &Dir) -> Result<bool, String> {
    let mut test = process::Command::new("/bin/sh");
    test.args(["-c", r#"test -d "$1""#, "sh", folder.as_str()]);
    let secs = FOLDER_LOOK.as_secs();
    match program::run(&mut test, FOLDER_LOOK) {
        Ok(Looked::Ended { status, .. }) => Ok(status.success()),
        Ok(Looked::Stuck) => Err(format!("{} did not answer in {secs}s", folder.as_str())),
        Err(error) => Err(format!("{} is not looked at: {error}", folder.as_str())),
    }
}

fn told(out: &Out, word: api::ShowResponse) -> bool {
    if out.capacity() <= 1 {
        let slow = format!("the client read too slowly, with {ANSWERS_MAX} answers unread");
        let _ = out.try_send(Err(Status::resource_exhausted(slow)));
        return false;
    }
    out.try_send(Ok(word)).is_ok()
}

fn said(failed: Failed) -> String {
    match failed {
        Failed::NotKept(why)
        | Failed::Nests(why)
        | Failed::Stale(why)
        | Failed::Refused(why)
        | Failed::Unwritten(why)
        | Failed::Unclosed(why) => why,
        Failed::KeptUntold => "it is kept, but the daemon's core has stopped".to_owned(),
    }
}

fn by_key(terminals: Vec<server::Terminal>) -> BTreeMap<TerminalKey, server::Terminal> {
    let count = terminals.len();
    let listed: BTreeMap<_, _> =
        terminals.into_iter().map(|terminal| (terminal.key, terminal)).collect();
    assert_eq!(listed.len(), count, "a listing holds each terminal once");
    listed
}

fn arrival(workstream: &Workstream, last: Option<TerminalId>) -> Option<TerminalId> {
    let view = workstream.view();
    if let Some(full) = view.full() {
        return Some(full);
    }
    if let Some(last) = last.filter(|last| view.holds(*last)) {
        return Some(last);
    }
    view.terminals().next().map(layout::Leaf::id)
}

fn tag(terminal: TerminalId) -> Vec<u8> {
    format!("{TAG}{terminal}").into_bytes()
}

fn tagged(tag: &[u8]) -> Option<TerminalId> {
    std::str::from_utf8(tag).ok()?.strip_prefix(TAG)?.parse().ok()
}

fn laid(
    workstream: &Workstream,
    size: Size,
    focus: Option<TerminalId>,
    known: &BTreeMap<TerminalId, Known>,
    looked: bool,
) -> api::ShowResponse {
    let view = workstream.view();
    if let Some(focus) = focus {
        assert!(view.holds(focus), "a client focuses terminal {focus} of the view it shows");
    }
    let (fitted, panes) = match view.tree() {
        None => (size, Vec::new()),
        Some(tree) => {
            let panes = tree.laid(size).into_iter().map(|(terminal, rect)| {
                use show_response::pane::Found;
                let terminal = layout::Leaf::id(terminal);
                let found = match known.get(&terminal) {
                    Some(known) => Found::Key(api::TerminalKey {
                        id: known.key.id.get(),
                        made_at: Some(known.key.made.into()),
                    }),
                    None if looked => Found::Lost(()),
                    None => Found::Looking(()),
                };
                show_response::Pane {
                    terminal: terminal.get(),
                    rect: Some(show_response::Rect {
                        column: rect.column.into(),
                        row: rect.row.into(),
                        columns: rect.columns.into(),
                        rows: rect.rows.into(),
                    }),
                    found: Some(found),
                }
            });
            (tree.fitted(size), panes.collect::<Vec<_>>())
        },
    };
    assert_eq!(panes.len(), view.len(), "a view lays out each of its terminals");
    let (columns, rows) = fitted.get();
    let laid = show_response::Laid {
        workstream: workstream.id().get(),
        size: Some(show_request::Size { columns: columns.into(), rows: rows.into() }),
        panes,
        shown: match (view.full(), focus) {
            (Some(full), _) => {
                assert_eq!(focus, Some(full), "a client of a full view focuses its terminal");
                Some(show_response::laid::Shown::Full(full.get()))
            },
            (None, Some(focus)) => Some(show_response::laid::Shown::Tiled(focus.get())),
            (None, None) => None,
        },
    };
    api::ShowResponse { event: Some(show_response::Event::Laid(laid)) }
}

fn runs_of(listed: &server::Terminal, agents: &[Agent], before: &Runs, shell: &OsString) -> Runs {
    let Is::Running { root, front, word, .. } = &listed.is else {
        return before.clone();
    };
    if let Some(front) = front
        && let Some(agent) = agents.iter().find(|agent| agent.pid == front.pid)
        && let Ok(session) = Session::parse(agent.session_id.as_str())
    {
        return Runs::Agent { harness: agent.harness, session };
    }
    match (word, front) {
        (Some(Word::Prompts), _) => Runs::Shell,
        (Some(Word::Going(Some(command))), _) => runs_line(command),
        (_, Some(front)) if front.pid != *root => runs_line(&quoted(&front.argv)),
        (_, Some(front)) if is_shell(&front.argv, shell) => Runs::Shell,
        (_, Some(front)) => runs_line(&quoted(&front.argv)),
        (Some(Word::Going(None)) | None, None) => before.clone(),
    }
}

fn runs_line(line: &str) -> Runs {
    let printable = |char: char| {
        let control = char.is_control() && !matches!(char, '\n' | '\t');
        if control { char::REPLACEMENT_CHARACTER } else { char }
    };
    let mut kept: String = line.chars().map(printable).collect();
    let end = (0..=kept.len().min(LINE_BYTES_MAX)).rev().find(|end| kept.is_char_boundary(*end));
    kept.truncate(end.expect("a string's start is a char's boundary"));
    assert!(kept.len() <= LINE_BYTES_MAX, "a line kept is cut to {LINE_BYTES_MAX} bytes");
    Line::parse(&kept).map_or(Runs::Shell, Runs::Command)
}

fn quoted(argv: &[String]) -> String {
    let plain = |word: &str| {
        let safe = |byte: u8| byte.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&byte);
        !word.is_empty() && word.bytes().all(safe)
    };
    let words = argv.iter().map(|word| {
        if plain(word) { word.clone() } else { format!("'{}'", word.replace('\'', r"'\''")) }
    });
    words.collect::<Vec<String>>().join(" ")
}

fn is_shell(argv: &[String], shell: &OsString) -> bool {
    let Some((first, rest)) = argv.split_first() else {
        return false;
    };
    let name = |path: &Path| path.file_name().map(|name| name.to_string_lossy().into_owned());
    let Some(named) = name(Path::new(first.trim_start_matches('-'))) else {
        return false;
    };
    let yours = name(Path::new(shell)).is_some_and(|yours| yours == named);
    if !yours && !SHELLS.contains(&named.as_str()) {
        return false;
    }
    rest.iter().all(|word| is_shell_option(word))
}

fn is_shell_option(word: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        return !long.is_empty() && long != "command";
    }
    word.strip_prefix('-').is_some_and(|short| !short.is_empty() && !short.contains('c'))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::SystemTime;

    use luneta_protocol::said::Said;

    use super::*;
    use crate::agent::{Harness, Status};
    use crate::mux::own::server::Foreground;
    use crate::mux::own::OwnMade;

    fn listed(is: Is) -> server::Terminal {
        let key = TerminalKey { id: 1.try_into().unwrap(), made: OwnMade::from_nanos(5) };
        let (tag, cwd, title) = (Vec::new(), "/w".to_owned(), String::new());
        server::Terminal { key, tag, size: None, cwd, title, is }
    }

    fn running(front: Option<(u32, &[&str])>, word: Option<Word>) -> server::Terminal {
        let front = front.map(|(pid, argv)| Foreground {
            pid,
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
        });
        listed(Is::Running { root: 10, front, word, doing: None })
    }

    fn agent(pid: u32) -> Agent {
        Agent {
            pid,
            harness: Harness::ClaudeCode,
            session_id: Said::new("7c22-ab".to_owned()).unwrap(),
            cwd: Said::new("/w".to_owned()).unwrap(),
            status: Status::Idle,
            status_since: None,
            started_at: Some(SystemTime::UNIX_EPOCH),
            name: None,
            entrypoint: None,
            pane: None,
        }
    }

    fn command(line: &str) -> Runs {
        Runs::Command(Line::parse(line).unwrap())
    }

    fn doing(word: Word, doing: Doing) -> server::Terminal {
        let front = Some(Foreground { pid: 10, argv: vec!["fish".to_owned()] });
        listed(Is::Running { root: 10, front, word: Some(word), doing: Some(doing) })
    }

    fn ended_ls() -> Doing {
        let command = Said::new("ls".to_owned());
        let at = SystemTime::UNIX_EPOCH;
        Doing::Ended { command, at, ending: None, took: Duration::from_millis(5) }
    }

    fn own(argv: &[&str]) -> Vec<String> {
        argv.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_terminal_runs_its_agent_its_command_or_its_shell() {
        let shell = OsString::from("/bin/fish");
        let before = command("htop");
        let of = |listed: &server::Terminal, agents: &[Agent]| {
            runs_of(listed, agents, &before, &shell)
        };
        let claude = running(Some((20, &["claude"])), Some(Word::Going(Some("claude".into()))));
        let session = Session::parse("7c22-ab").unwrap();
        let agent_runs = Runs::Agent { harness: Harness::ClaudeCode, session };
        assert_eq!(of(&claude, &[agent(20)]), agent_runs);
        assert_eq!(of(&claude, &[agent(21)]), command("claude"));
        assert_eq!(of(&running(Some((10, &["fish"])), Some(Word::Prompts)), &[]), Runs::Shell);
        let vim = running(Some((20, &["vim", "notes"])), None);
        assert_eq!(of(&vim, &[]), command("vim notes"));
        assert_eq!(of(&running(Some((10, &["-bash"])), None), &[]), Runs::Shell);
        assert_eq!(of(&running(Some((10, &["/nix/store/x/fish"])), None), &[]), Runs::Shell);
        let kanban = running(Some((10, &["luneta", "kanban"])), None);
        assert_eq!(of(&kanban, &[]), command("luneta kanban"));
        assert_eq!(of(&running(None, None), &[]), before);
        assert_eq!(of(&running(None, Some(Word::Going(None))), &[]), before);
        assert_eq!(of(&listed(Is::Ended), &[agent(20)]), before);
        let long = "é".repeat(LINE_BYTES_MAX);
        let too_long = running(Some((20, &[long.as_str()])), None);
        let cut = "é".repeat(LINE_BYTES_MAX / 2 - 1);
        assert_eq!(of(&too_long, &[]), command(&format!("'{cut}")), "a line is cut on a char");
        let bell = running(Some((20, &["printf", "\u{7}"])), None);
        assert_eq!(of(&bell, &[]), command("printf '\u{fffd}'"));
        for argv in [&["ssh", "box"][..], &["bash", "-c", "make"], &["bash", "-lc", "make"]] {
            assert_eq!(of(&running(Some((10, argv)), None), &[]), command(&quoted(&own(argv))));
        }
        for argv in [&["-zsh"][..], &["fish", "--login"], &["bash", "-i"]] {
            assert_eq!(of(&running(Some((10, argv)), None), &[]), Runs::Shell, "{argv:?}");
        }
    }

    #[test]
    fn a_restored_terminal_keeps_what_it_ran_until_it_runs_something_else() {
        let shell = Runs::Shell;
        let line = Line::parse("make test").unwrap();
        let unrun = Runs::Unrun(line.clone());
        let now = Instant::now();
        let typing = Restored::Typing { line, due: now, by: now };
        let prompts = running(Some((10, &["fish"])), Some(Word::Prompts));
        let started = running(Some((10, &["fish"])), None);
        let going = running(Some((10, &["fish"])), Some(Word::Going(Some("ls".into()))));
        let vim = running(Some((20, &["vim"])), None);
        let ended = listed(Is::Ended);
        let ran_ls = doing(Word::Prompts, ended_ls());
        for is in [&prompts, &started, &going, &vim, &ended, &ran_ls] {
            assert!(!keeps(None, &shell, &is.is, &shell), "{is:?}");
            assert!(!keeps(None, &command("make test"), &is.is, &shell), "{is:?}");
            assert!(keeps(Some(&typing), &unrun, &is.is, &shell), "{is:?}");
        }
        for is in [&prompts, &started] {
            assert!(keeps(None, &unrun, &is.is, &shell), "{is:?}");
        }
        for is in [&going, &vim, &ended, &ran_ls] {
            assert!(!keeps(None, &unrun, &is.is, &shell), "{is:?}");
        }
        let session = Session::parse("7c22-ab").unwrap();
        let agent = Runs::Agent { harness: Harness::ClaudeCode, session };
        let resumed = Some(&Restored::Resumed);
        assert!(keeps(resumed, &agent, &started.is, &command("claude --resume 7c22-ab")));
        assert!(!keeps(resumed, &agent, &started.is, &agent));
    }

    #[test]
    fn a_terminal_that_fails_to_restore_is_given_up_after_its_attempts() {
        let (failing, other) = (TerminalId::of(7).unwrap(), TerminalId::of(8).unwrap());
        let mut attempts = Attempts::default();
        for attempt in 1..RESTORE_ATTEMPTS_MAX {
            assert!(attempts.wanted(failing));
            let said = attempts.failed(failing, "no folder");
            assert!(said.contains(&format!("attempt {attempt} of")), "{said}");
        }
        let said = attempts.failed(failing, "no folder");
        assert!(said.contains("given up"), "{said}");
        assert!(!attempts.wanted(failing));
        assert!(attempts.wanted(other));
        attempts.retain(|terminal| terminal != failing);
        assert!(attempts.wanted(failing), "a terminal found again is tried afresh");
    }

    #[test]
    fn a_crash_soon_after_a_restore_counts_as_a_failed_attempt() {
        let (crashing, other) = (TerminalId::of(7).unwrap(), TerminalId::of(8).unwrap());
        let mut attempts = Attempts::default();
        let said = attempts.failed(crashing, "no folder");
        assert!(said.contains("attempt 1 of"), "{said}");
        let said = attempts.crashed(crashing, Duration::from_secs(4));
        assert!(said.contains(&format!("ended 4s after terminal {crashing} was")), "{said}");
        assert!(said.contains("attempt 2 of"), "{said}");
        for _ in 2..RESTORE_ATTEMPTS_MAX {
            assert!(attempts.wanted(crashing));
            attempts.crashed(crashing, Duration::from_secs(4));
        }
        assert!(!attempts.wanted(crashing), "a terminal luneta-mux keeps ending after is given up");
        attempts.failed(other, "no folder");
        attempts.settled(other);
        attempts.settled(crashing);
        assert!(attempts.wanted(other) && attempts.wanted(crashing), "a settled one starts afresh");
        for terminal in [other, crashing] {
            let said = attempts.failed(terminal, "no folder");
            assert!(said.contains("attempt 1 of"), "{said}");
        }
    }

    #[test]
    fn a_restored_line_is_pasted_only_where_its_shell_waits_for_one() {
        let start = Instant::now();
        let (due, by) = (start + TYPING_WAIT, start + TYPING_WAIT_MAX);
        let (early, late, too_late) = (start, due, by);
        let prompts = running(Some((10, &["fish"])), Some(Word::Prompts));
        let quiet = running(Some((10, &["bash"])), None);
        let vim = running(Some((20, &["vim"])), None);
        let going = running(Some((10, &["fish"])), Some(Word::Going(Some("vim".into()))));
        let ran_ls = doing(Word::Prompts, ended_ls());
        let ended = listed(Is::Ended);
        let at = |is: Option<&server::Terminal>, now| pasted(is.map(|it| &it.is), due, by, now);
        assert_eq!(at(Some(&prompts), early), Paste::Now, "a prompt is waited for");
        assert_eq!(at(Some(&quiet), early), Paste::Later);
        assert_eq!(at(Some(&quiet), late), Paste::Now, "a shell with no marks is pasted in time");
        assert_eq!(at(None, early), Paste::Later);
        assert_eq!(at(None, late), Paste::Now);
        for busy in [&vim, &going] {
            assert_eq!(at(Some(busy), late), Paste::Later, "{busy:?}");
            assert_eq!(at(Some(busy), too_late), Paste::Never, "{busy:?}");
        }
        for done in [&ran_ls, &ended] {
            assert_eq!(at(Some(done), early), Paste::Never, "{done:?}");
        }
    }

    #[test]
    fn a_folder_is_looked_at_apart_from_the_views_thread() {
        let folder = tempfile::tempdir().unwrap();
        let dir = Dir::parse(folder.path().to_str().unwrap()).unwrap();
        assert_eq!(is_folder(&dir), Ok(true));
        let file = folder.path().join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(is_folder(&Dir::parse(file.to_str().unwrap()).unwrap()), Ok(false));
        let gone = folder.path().join("gone");
        assert_eq!(is_folder(&Dir::parse(gone.to_str().unwrap()).unwrap()), Ok(false));
        let spaced = folder.path().join("a b'c");
        fs::create_dir(&spaced).unwrap();
        assert_eq!(is_folder(&Dir::parse(spaced.to_str().unwrap()).unwrap()), Ok(true));
    }

    #[test]
    fn a_command_line_is_quoted_back_as_a_shell_would_read_it() {
        let argv = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
        assert_eq!(quoted(&argv(&["vim", "notes.md"])), "vim notes.md");
        assert_eq!(quoted(&argv(&["sh", "-c", "echo hi"])), "sh -c 'echo hi'");
        assert_eq!(quoted(&argv(&["printf", "it's", ""])), r"printf 'it'\''s' ''");
    }

    #[test]
    fn a_tag_names_the_terminal_of_a_view() {
        let id = TerminalId::of(0x1f).unwrap();
        assert_eq!(tagged(format!("{TAG}{id}").as_bytes()), Some(id));
        for refused in [&b"luneta terminal 1f"[..], b"terminal 000000000000001f", b"\xff", b""] {
            assert_eq!(tagged(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn a_show_word_reads_into_what_it_asks() {
        use show_request::{focus, open, resize, Event};
        let terminal = Event::Focus(show_request::Focus { to: Some(focus::To::Terminal(3)) });
        let three = TerminalId::of(3).unwrap();
        assert_eq!(Asked::of_api(terminal), Ok(Asked::Focus(Focus::Terminal(three))));
        let right = show_request::Towards::Right.into();
        let open = show_request::Open { beside: Some(open::Beside::Towards(right)), argv: vec![] };
        let towards = Beside::Towards(Towards::Right);
        let asked = Asked::Open { beside: towards, argv: vec![] };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let largest = || Some(open::Beside::Largest(()));
        let open = show_request::Open { beside: largest(), argv: vec![] };
        let asked = Asked::Open { beside: Beside::Largest, argv: vec![] };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let open = show_request::Open { beside: Some(open::Beside::Focused(())), argv: vec![] };
        let asked = Asked::Open { beside: Beside::Focused, argv: vec![] };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let open = show_request::Open { beside: Some(open::Beside::Alone(())), argv: vec![] };
        let asked = Asked::Open { beside: Beside::Alone, argv: vec![] };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let (one_side, every) = (resize::Sides::Direction, Some(resize::Sides::Every(())));
        let (grow, shrink) = (show_request::Growth::Grow, show_request::Growth::Shrink);
        let (grow, shrink) = (grow.into(), shrink.into());
        let around = Event::Resize(show_request::Resize { sides: every, growth: shrink });
        let asked = Asked::Resize { sides: Sides::Every, growth: layout::Growth::Shrink };
        assert_eq!(Asked::of_api(around), Ok(asked));
        let left = Some(one_side(show_request::Direction::Left.into()));
        let towards = Event::Resize(show_request::Resize { sides: left, growth: grow });
        let (sides, growth) = (Sides::One(Direction::Left), layout::Growth::Grow);
        assert_eq!(Asked::of_api(towards), Ok(Asked::Resize { sides, growth }));
        let refused = [
            Event::Go(0),
            Event::Focus(show_request::Focus { to: None }),
            Event::Resize(show_request::Resize { sides: None, growth: grow }),
            Event::Resize(show_request::Resize { sides: Some(one_side(0)), growth: grow }),
            Event::Resize(show_request::Resize { sides: every, growth: 0 }),
            Event::Resize(show_request::Resize { sides: every, growth: 3 }),
            Event::Size(show_request::Size { columns: 0, rows: 24 }),
            Event::Open(show_request::Open { beside: None, argv: vec![] }),
            Event::Open(show_request::Open { beside: largest(), argv: vec![String::new()] }),
            Event::Open(show_request::Open {
                beside: largest(),
                argv: vec!["x".to_owned(); ARGV_MAX + 1],
            }),
            Event::Hello(show_request::Hello { workstream: 1, ..Default::default() }),
        ];
        for event in refused {
            assert!(Asked::of_api(event.clone()).is_err(), "{event:?}");
        }
    }
}
