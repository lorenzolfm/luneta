use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc as answers, oneshot};
use tonic::Status;

use crate::agent::Agent;
use crate::api::{self, show_request, show_response};
use crate::core::{self, Command as Core};
use crate::mux::own::server::{self, Is, Terminals, TerminalsListed, Word};
use crate::mux::own::TerminalKey;
use crate::mux::Runs as Opened;
use crate::projects::layout::{self, Direction, Order, Rect, Towards};
use crate::projects::{Change, Dir, Failed, Id, Line, Placed, Projects, Ran, Runs, Session};
use crate::projects::{Size, Store, Terminal, View, Workstream, LINE_BYTES_MAX};
use crate::watch::Troubles;
use crate::{opens, Directory};

pub const CLIENTS_MAX: usize = 64;

pub const ANSWERS_MAX: usize = 64;

const COMMANDS_MAX: usize = 64;

const ARGV_MAX: usize = LINE_BYTES_MAX / 2;

const TAG: &str = "luneta terminal ";

const SHELLS: [&str; 9] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh"];

const _: () = assert!(ANSWERS_MAX > 1);

pub type Out = answers::Sender<Result<api::ShowResponse, Status>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientId(u64);

pub enum Command {
    Change { change: Change, answer: oneshot::Sender<Result<Option<Id>, Failed>> },
    Came {
        workstream: Id,
        size: Size,
        out: Out,
        answer: oneshot::Sender<Result<ClientId, Status>>,
    },
    Asked { client: ClientId, asked: Result<Asked, String> },
    Left(ClientId),
    Listed(TerminalsListed),
    Emptied { since: Instant },
    Crashed,
    Unreached,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Size(Size),
    Go(Id),
    Open { beside: Beside, argv: Vec<String> },
    Close,
    Focus(Focus),
    Resize { direction: Direction, change: layout::Change },
    Move(Step),
    Fullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    Largest,
    Towards(Towards),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Step(Step),
    Terminal(Id),
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
            Event::Go(workstream) => Asked::Go(id("go", workstream)?),
            Event::Open(show_request::Open { beside, argv }) => {
                use show_request::open::Beside as Said;
                let beside = match beside.ok_or("open: it says no side to open beside")? {
                    Said::Largest(()) => Beside::Largest,
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
                    To::Terminal(terminal) => Focus::Terminal(id("focus", terminal)?),
                })
            },
            Event::Resize(show_request::Resize { direction, grow }) => {
                let change = if grow { layout::Change::Grow } else { layout::Change::Shrink };
                Asked::Resize { direction: direction_of(direction)?, change }
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

fn direction_of(said: i32) -> Result<Direction, String> {
    use show_request::Direction as Said;
    match Said::try_from(said) {
        Ok(Said::Left) => Ok(Direction::Left),
        Ok(Said::Right) => Ok(Direction::Right),
        Ok(Said::Up) => Ok(Direction::Up),
        Ok(Said::Down) => Ok(Direction::Down),
        Ok(Said::Unspecified) | Err(_) => Err("no direction it knows".to_owned()),
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
        mux: Terminals::default(),
        clients: BTreeMap::new(),
        arrivals: 0,
        known: BTreeMap::new(),
        listed: BTreeMap::new(),
        looked: false,
        last_focus: BTreeMap::new(),
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
    known: BTreeMap<Id, Known>,
    listed: BTreeMap<TerminalKey, server::Terminal>,
    looked: bool,
    last_focus: BTreeMap<Id, Id>,
    troubles: Troubles,
    shell: OsString,
}

struct Client {
    workstream: Id,
    size: Size,
    focus: Option<Id>,
    out: Out,
    said: Option<api::ShowResponse>,
}

struct Known {
    key: TerminalKey,
    heard: Heard,
    sized: Option<Size>,
    agent_front: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heard {
    Made(Instant),
    Seen,
}

fn run(mut views: Views, commands: &Receiver<Command>, sweep: Duration) {
    let mut swept = Instant::now();
    loop {
        match commands.recv_timeout(sweep.saturating_sub(swept.elapsed())) {
            Ok(command) => views.heard(command),
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                views.flushed();
                return;
            },
        }
        if swept.elapsed() >= sweep {
            views.read_runs();
            views.flushed();
            swept = Instant::now();
        }
        views.settle();
        views.troubles.done();
    }
}

impl Views {
    fn heard(&mut self, command: Command) {
        match command {
            Command::Change { change, answer } => {
                let _ = answer.send(self.store.change(change));
            },
            Command::Came { workstream, size, out, answer } => {
                let _ = answer.send(self.came(workstream, size, out));
            },
            Command::Asked { client, asked } => {
                if !self.clients.contains_key(&client) {
                    return;
                }
                if let Err(why) = asked.and_then(|asked| self.asked(client, asked)) {
                    let refused = Some(show_response::Event::Refused(why));
                    let word = api::ShowResponse { event: refused };
                    let held = self.clients.get(&client).expect("a client that asked is held");
                    if !told(&held.out, word) {
                        self.clients.remove(&client);
                    }
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
                self.looked = true;
                self.known.clear();
                self.listed.clear();
            },
            Command::Unreached => self.looked = true,
        }
    }

    fn came(&mut self, workstream: Id, size: Size, out: Out) -> Result<ClientId, Status> {
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
        let client = Client { workstream, size, focus, out, said: None };
        assert!(self.clients.insert(id, client).is_none(), "a client comes once");
        Ok(id)
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

    fn flushed(&mut self) {
        if let Err(failed) = self.store.flush() {
            self.troubles.say(format!("the views are not written: {}", said(failed)));
        }
    }

    fn asked(&mut self, client: ClientId, asked: Asked) -> Result<(), String> {
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
            },
            Asked::Open { beside, argv } => self.open(client, beside, &argv)?,
            Asked::Close => self.close(focused()?)?,
            Asked::Focus(Focus::Terminal(terminal)) => {
                if !self.view(workstream)?.holds(terminal) {
                    return Err(format!("the view holds no terminal {terminal}"));
                }
                self.focus(client, terminal)?;
            },
            Asked::Focus(Focus::Step(step)) => {
                let from = focused()?;
                let to = self.stepped(workstream, from, step, size)?;
                self.focus(client, to)?;
            },
            Asked::Resize { direction, change } => {
                let terminal = focused()?;
                self.changed(Change::Resize { terminal, direction, change, size })?;
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
        Ok(())
    }

    fn client(&mut self, client: ClientId) -> &mut Client {
        self.clients.get_mut(&client).expect("a client asked is held")
    }

    fn view(&self, workstream: Id) -> Result<&View, String> {
        let projects = self.kept()?;
        let found = projects.find(workstream).ok_or_else(|| format!("no workstream {workstream}"));
        let (_, found) = found?;
        Ok(found.view())
    }

    fn stepped(&self, workstream: Id, from: Id, step: Step, size: Size) -> Result<Id, String> {
        let tree = self.view(workstream)?.tree().ok_or("the view holds no terminal")?;
        let to = match step {
            Step::Direction(direction) => tree.neighbour(from, direction, size),
            Step::Order(order) => tree.in_order(from, order),
        };
        to.ok_or_else(|| "no other terminal is there".to_owned())
    }

    fn focus(&mut self, client: ClientId, terminal: Id) -> Result<(), String> {
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
        let shown = self.clients.values().filter(|it| it.workstream == workstream);
        shown.map(|it| it.size).fold(size, Size::least)
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
            (Some(tree), Beside::Towards(towards)) => {
                let at = focus.ok_or("the view holds no terminal to open beside")?;
                let room = tree.room(at, towards, terminal, size);
                let inner = room.ok_or_else(|| format!("terminal {at} has no room beside it"))?;
                (Placed::Beside { at, towards }, inner)
            },
            (Some(tree), Beside::Largest) => {
                let room = tree.room_beside_largest(terminal, size);
                let (at, towards, inner) = room.ok_or("no terminal has room for another")?;
                (Placed::Beside { at, towards }, inner)
            },
        };
        let listed_cwd = focus
            .and_then(|focus| self.known.get(&focus))
            .and_then(|known| self.listed.get(&known.key))
            .and_then(|listed| Dir::parse(&listed.cwd).ok());
        let folder = listed_cwd.unwrap_or_else(|| project.folder(shown));
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
                if let Err(why) = self.changed(Change::Close { terminal }) {
                    self.troubles.say(format!("terminal {terminal} stays lost: {why}"));
                }
                return Err(failure.to_string());
            },
        };
        let heard_none = !self.known.values().any(|known| known.heard == Heard::Seen);
        let heard = Heard::Made(Instant::now());
        let known = Known { key, heard, sized: Some(inner), agent_front: None };
        assert!(self.known.insert(terminal, known).is_none(), "terminal {terminal} is made once");
        if heard_none {
            match self.follower.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => {},
                Err(TrySendError::Disconnected(())) => {
                    self.troubles.say("the mux's terminals are followed no more".to_owned());
                },
            }
        }
        self.client(client).focus = Some(terminal);
        self.last_focus.insert(workstream, terminal);
        Ok(())
    }

    fn close(&mut self, terminal: Id) -> Result<(), String> {
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

    fn closed(&mut self, terminal: Id) -> Result<(), String> {
        let projects = self.kept()?;
        let (held_in, _) = projects.holding(terminal).ok_or("no view holds the terminal")?;
        let workstream = held_in.id();
        let tree = held_in.view().tree().expect("a view that holds a terminal is laid");
        let heir = tree.heir(terminal);
        self.changed(Change::Close { terminal })?;
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
        assert!(!projects.holds(terminal), "terminal {terminal} left its view");
        if let Some(heir) = heir {
            assert!(projects.holding(heir).is_some(), "terminal {heir} took the room it left");
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
        let mut found: BTreeMap<Id, (TerminalKey, Option<Size>)> = BTreeMap::new();
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
        let ended: Vec<Id> = if whole {
            let gone = |(terminal, known): &(&Id, &Known)| {
                !found.contains_key(*terminal) && known.heard == Heard::Seen
            };
            self.known.iter().filter(gone).map(|(terminal, _)| *terminal).collect()
        } else {
            let unread = "a listing is not read whole, and so no terminal is ended";
            self.troubles.say(unread.to_owned());
            Vec::new()
        };
        let owners: BTreeMap<TerminalKey, Id> =
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
                    let known = Known { key, heard: Heard::Seen, sized: size, agent_front: None };
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
            known.agent_front = front.filter(|_| matches!(runs, Runs::Agent { .. }));
            if folder != *held.folder() || runs != *held.runs() {
                ran.push(Ran { terminal: *terminal, folder, runs });
            }
        }
        if ran.is_empty() {
            return;
        }
        if let Err(why) = self.changed(Change::Ran(ran)) {
            self.troubles.say(format!("what the terminals run is not kept: {why}"));
        }
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
        let mut sizes: BTreeMap<Id, Size> = BTreeMap::new();
        for client in clients.values() {
            let size = sizes.entry(client.workstream).or_insert(client.size);
            *size = size.least(client.size);
        }
        for (&workstream, &size) in &sizes {
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
                if known.sized == Some(wanted) {
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
            let size = sizes[&client.workstream];
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
        Failed::NotKept(why) | Failed::Refused(why) | Failed::Unwritten(why) => why,
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

fn arrival(workstream: &Workstream, last: Option<Id>) -> Option<Id> {
    let view = workstream.view();
    if let Some(full) = view.full() {
        return Some(full);
    }
    if let Some(last) = last.filter(|last| view.holds(*last)) {
        return Some(last);
    }
    view.terminals().next().map(layout::Leaf::id)
}

fn tag(terminal: Id) -> Vec<u8> {
    format!("{TAG}{terminal}").into_bytes()
}

fn tagged(tag: &[u8]) -> Option<Id> {
    std::str::from_utf8(tag).ok()?.strip_prefix(TAG)?.parse().ok()
}

fn laid(
    workstream: &Workstream,
    size: Size,
    focus: Option<Id>,
    known: &BTreeMap<Id, Known>,
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
        focused: focus.map(Id::get),
        full: view.full().map(Id::get),
    };
    api::ShowResponse { event: Some(show_response::Event::Laid(laid)) }
}

fn runs_of(listed: &server::Terminal, agents: &[Agent], before: &Runs, shell: &OsString) -> Runs {
    let Is::Running { root, front, word } = &listed.is else {
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
    use std::time::SystemTime;

    use luneta_protocol::said::Said;

    use super::*;
    use crate::agent::{Harness, Status as Doing};
    use crate::mux::own::server::Foreground;
    use crate::mux::own::OwnMade;

    fn listed(is: Is) -> server::Terminal {
        let key = TerminalKey { id: 1.try_into().unwrap(), made: OwnMade::from_nanos(5) };
        server::Terminal { key, tag: Vec::new(), size: None, cwd: "/w".to_owned(), is }
    }

    fn running(front: Option<(u32, &[&str])>, word: Option<Word>) -> server::Terminal {
        let front = front.map(|(pid, argv)| Foreground {
            pid,
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
        });
        listed(Is::Running { root: 10, front, word })
    }

    fn agent(pid: u32) -> Agent {
        Agent {
            pid,
            harness: Harness::ClaudeCode,
            session_id: Said::new("7c22-ab".to_owned()).unwrap(),
            cwd: Said::new("/w".to_owned()).unwrap(),
            status: Doing::Idle,
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
    fn a_command_line_is_quoted_back_as_a_shell_would_read_it() {
        let argv = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
        assert_eq!(quoted(&argv(&["vim", "notes.md"])), "vim notes.md");
        assert_eq!(quoted(&argv(&["sh", "-c", "echo hi"])), "sh -c 'echo hi'");
        assert_eq!(quoted(&argv(&["printf", "it's", ""])), r"printf 'it'\''s' ''");
    }

    #[test]
    fn a_tag_names_the_terminal_of_a_view() {
        let id = Id::of(0x1f).unwrap();
        assert_eq!(tagged(format!("{TAG}{id}").as_bytes()), Some(id));
        for refused in [&b"luneta terminal 1f"[..], b"terminal 000000000000001f", b"\xff", b""] {
            assert_eq!(tagged(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn a_show_word_reads_into_what_it_asks() {
        use show_request::{focus, open, Event};
        let terminal = Event::Focus(show_request::Focus { to: Some(focus::To::Terminal(3)) });
        assert_eq!(Asked::of_api(terminal), Ok(Asked::Focus(Focus::Terminal(Id::of(3).unwrap()))));
        let right = show_request::Towards::Right.into();
        let open = show_request::Open { beside: Some(open::Beside::Towards(right)), argv: vec![] };
        let towards = Beside::Towards(Towards::Right);
        let asked = Asked::Open { beside: towards, argv: vec![] };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let largest = || Some(open::Beside::Largest(()));
        let open = show_request::Open { beside: largest(), argv: vec![] };
        let asked = Asked::Open { beside: Beside::Largest, argv: vec![] };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let refused = [
            Event::Go(0),
            Event::Focus(show_request::Focus { to: None }),
            Event::Resize(show_request::Resize { direction: 0, grow: true }),
            Event::Size(show_request::Size { columns: 0, rows: 24 }),
            Event::Open(show_request::Open { beside: None, argv: vec![] }),
            Event::Open(show_request::Open { beside: largest(), argv: vec![String::new()] }),
            Event::Open(show_request::Open {
                beside: largest(),
                argv: vec!["x".to_owned(); ARGV_MAX + 1],
            }),
            Event::Hello(show_request::Hello { workstream: 1, size: None }),
        ];
        for event in refused {
            assert!(Asked::of_api(event.clone()).is_err(), "{event:?}");
        }
    }
}
