use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc as answers, oneshot};
use tonic::Status;

use crate::agent::Agent;
use crate::api::{self, show_request, show_response};
use crate::core::{self, Command as Core};
use crate::mux::own::server::{self, Is, Terminals, Word};
use crate::mux::own::TerminalKey;
use crate::mux::Runs as Opened;
use crate::projects::layout::{self, Direction, Order, Rect, Towards};
use crate::projects::{Change, Dir, Failed, Id, Line, Placed, Projects, Ran, Runs, Session};
use crate::projects::{Size, Store, Terminal, View, Workstream};
use crate::watch::Troubles;
use crate::{opens, Directory};

pub const CLIENTS_MAX: usize = 64;

pub const ANSWERS_MAX: usize = 64;

const COMMANDS_MAX: usize = 64;

const TAG: &str = "luneta terminal ";

const UNSHOWN: (u32, u32) = (80, 24);

pub type Out = answers::Sender<Result<api::ShowResponse, Status>>;

pub enum Command {
    Change { change: Change, answer: oneshot::Sender<Result<Option<Id>, Failed>> },
    Came { workstream: Id, size: Size, out: Out, answer: oneshot::Sender<Result<u64, Status>> },
    Asked { client: u64, asked: Result<Asked, String> },
    Left(u64),
    Listed { terminals: Vec<server::Terminal>, since: Instant },
    Unlisted { emptied: Option<Instant> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Size(Size),
    Go(Id),
    Open { beside: Beside, argv: Vec<String> },
    Close,
    Focus(Focus),
    Resize { towards: Direction, change: layout::Change },
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
    Towards(Direction),
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
                let beside = match beside {
                    None | Some(Said::Largest(())) => Beside::Largest,
                    Some(Said::Towards(towards)) => Beside::Towards(towards_of(towards)?),
                };
                if argv.first().is_some_and(String::is_empty) {
                    return Err("open: the program to run is named by no word".to_owned());
                }
                Asked::Open { beside, argv }
            },
            Event::Close(()) => Asked::Close,
            Event::Focus(show_request::Focus { to }) => {
                use show_request::focus::To;
                Asked::Focus(match to.ok_or("focus: it says no terminal to go to")? {
                    To::Towards(towards) => Focus::Step(Step::Towards(direction_of(towards)?)),
                    To::Order(order) => Focus::Step(Step::Order(order_of(order)?)),
                    To::Terminal(terminal) => Focus::Terminal(id("focus", terminal)?),
                })
            },
            Event::Resize(show_request::Resize { towards, grow }) => {
                let change = if grow { layout::Change::Grow } else { layout::Change::Shrink };
                Asked::Resize { towards: direction_of(towards)?, change }
            },
            Event::Move(show_request::Move { to }) => {
                use show_request::r#move::To;
                Asked::Move(match to.ok_or("move: it says nowhere to move to")? {
                    To::Towards(towards) => Step::Towards(direction_of(towards)?),
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
        keys: BTreeMap::new(),
        seen: BTreeSet::new(),
        made: BTreeMap::new(),
        agent_fronts: BTreeMap::new(),
        listed: Vec::new(),
        sized: BTreeMap::new(),
        sizes: BTreeMap::new(),
        last: BTreeMap::new(),
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
    clients: BTreeMap<u64, Client>,
    arrivals: u64,
    keys: BTreeMap<Id, TerminalKey>,
    seen: BTreeSet<TerminalKey>,
    made: BTreeMap<TerminalKey, Instant>,
    agent_fronts: BTreeMap<Id, u32>,
    listed: Vec<server::Terminal>,
    sized: BTreeMap<TerminalKey, Size>,
    sizes: BTreeMap<Id, Size>,
    last: BTreeMap<Id, Id>,
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

fn run(mut views: Views, commands: &Receiver<Command>, sweep: Duration) {
    loop {
        match commands.recv_timeout(sweep) {
            Ok(command) => views.heard(command),
            Err(RecvTimeoutError::Timeout) => views.read_runs(),
            Err(RecvTimeoutError::Disconnected) => return,
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
                    if held.out.try_send(Ok(word)).is_err() {
                        self.clients.remove(&client);
                    }
                }
            },
            Command::Left(client) => {
                self.clients.remove(&client);
            },
            Command::Listed { terminals, since } => self.heard_listed(terminals, since),
            Command::Unlisted { emptied } => {
                if let Some(since) = emptied {
                    self.heard_listed(Vec::new(), since);
                }
                self.keys.clear();
                self.seen.clear();
                self.made.clear();
                self.agent_fronts.clear();
                self.listed.clear();
                self.sized.clear();
            },
        }
    }

    fn came(&mut self, workstream: Id, size: Size, out: Out) -> Result<u64, Status> {
        if self.clients.len() >= CLIENTS_MAX {
            let many = format!("{CLIENTS_MAX} clients show views, as many as may");
            return Err(Status::resource_exhausted(many));
        }
        let projects = self.kept().map_err(Status::failed_precondition)?;
        let Some((_, shown)) = projects.find(workstream) else {
            return Err(Status::not_found(format!("no workstream has the id {workstream}")));
        };
        let focus = arrival(shown, self.last.get(&workstream).copied());
        self.arrivals += 1;
        let client = Client { workstream, size, focus, out, said: None };
        assert!(self.clients.insert(self.arrivals, client).is_none(), "a client comes once");
        Ok(self.arrivals)
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

    fn asked(&mut self, client: u64, asked: Asked) -> Result<(), String> {
        let workstream = self.clients[&client].workstream;
        let size = self.size_of(workstream);
        let focus = self.clients[&client].focus;
        let focused = || focus.ok_or_else(|| "the view holds no terminal".to_owned());
        match asked {
            Asked::Size(size) => self.client(client).size = size,
            Asked::Go(to) => {
                let projects = self.kept()?;
                let (_, shown) = projects.find(to).ok_or_else(|| format!("no workstream {to}"))?;
                let focus = arrival(shown, self.last.get(&to).copied());
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
            Asked::Resize { towards, change } => {
                if self.view(workstream)?.full().is_some() {
                    return Err("a view one terminal fills is resized by none".to_owned());
                }
                let terminal = focused()?;
                self.changed(Change::Resize { terminal, direction: towards, change, size })?;
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

    fn client(&mut self, client: u64) -> &mut Client {
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
            Step::Towards(direction) => tree.neighbour(from, direction, size),
            Step::Order(order) => tree.in_order(from, order),
        };
        to.ok_or_else(|| "no other terminal is there".to_owned())
    }

    fn focus(&mut self, client: u64, terminal: Id) -> Result<(), String> {
        let workstream = self.clients[&client].workstream;
        let full = self.view(workstream)?.full();
        if full.is_some_and(|full| full != terminal) {
            self.changed(Change::Fill { workstream, full: None })?;
        }
        self.client(client).focus = Some(terminal);
        self.last.insert(workstream, terminal);
        Ok(())
    }

    fn size_of(&self, workstream: Id) -> Size {
        let shown = self.clients.values().filter(|client| client.workstream == workstream);
        Size::smallest(shown.map(|client| client.size))
            .or_else(|| self.sizes.get(&workstream).copied())
            .unwrap_or_else(|| Size::of(UNSHOWN.0, UNSHOWN.1).expect("80 by 24 is a size"))
    }

    fn open(&mut self, client: u64, beside: Beside, argv: &[String]) -> Result<(), String> {
        let Client { workstream, focus, .. } = self.clients[&client];
        let size = self.size_of(workstream);
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
            .and_then(|focus| self.keys.get(&focus))
            .and_then(|key| self.listed.iter().find(|listed| listed.key == *key))
            .and_then(|listed| Dir::parse(&listed.cwd).ok());
        let folder = listed_cwd.unwrap_or_else(|| project.folder(shown));
        let cwd = Directory::parse(folder.as_str().to_owned()).expect("a dir is absolute");
        let (opened, runs) = match argv.split_first() {
            None => (Opened::Shell(&self.shell), Runs::Shell),
            Some((program, rest)) => {
                (Opened::Command { program, args: rest }, runs_line(&quoted(argv)))
            },
        };
        let opening = self.mux.made(&cwd, opened, tag(terminal), inner);
        let key = opening.map_err(|failure| failure.to_string())?;
        let made = Terminal::new(terminal, folder, runs);
        let change = Change::Open { workstream, terminal: made, placed, size };
        if let Err(why) = self.changed(change) {
            if let Err(failure) = self.mux.closed(key) {
                self.troubles.say(format!("terminal {} was left open: {failure}", key.id));
            }
            return Err(why);
        }
        self.keys.insert(terminal, key);
        self.made.insert(key, Instant::now());
        self.sized.insert(key, inner);
        if self.seen.is_empty() {
            let _ = self.follower.try_send(());
        }
        self.client(client).focus = Some(terminal);
        self.last.insert(workstream, terminal);
        Ok(())
    }

    fn close(&mut self, terminal: Id) -> Result<(), String> {
        let key = match self.keys.get(&terminal).copied() {
            Some(key) => Some(key),
            None => self.mux.tagged(&tag(terminal)).unwrap_or_default(),
        };
        if let Some(key) = key {
            self.mux.closed(key).map_err(|failure| failure.to_string())?;
        }
        self.closed(terminal)
    }

    fn closed(&mut self, terminal: Id) -> Result<(), String> {
        let projects = self.kept()?;
        let (workstream, _) = projects.holding(terminal).ok_or("no view holds the terminal")?;
        let workstream = workstream.id();
        let mut tree = workstream_tree(projects, workstream);
        let heir = match tree.remove(terminal) {
            layout::Removed::Last => None,
            layout::Removed::Out { heir, .. } => Some(heir),
        };
        self.changed(Change::Close { terminal })?;
        if let Some(key) = self.keys.remove(&terminal) {
            self.seen.remove(&key);
            self.made.remove(&key);
            self.sized.remove(&key);
            self.agent_fronts.remove(&terminal);
        }
        for client in self.clients.values_mut().filter(|client| client.focus == Some(terminal)) {
            client.focus = heir;
        }
        if self.last.get(&workstream) == Some(&terminal) {
            match heir {
                Some(heir) => self.last.insert(workstream, heir),
                None => self.last.remove(&workstream),
            };
        }
        Ok(())
    }

    fn heard_listed(&mut self, terminals: Vec<server::Terminal>, since: Instant) {
        self.made.retain(|key, made| {
            let before = *made < since;
            if before {
                self.seen.insert(*key);
            }
            !before
        });
        let Some(projects) = self.store.projects() else {
            self.listed = terminals;
            return;
        };
        let mut found: BTreeMap<Id, TerminalKey> = BTreeMap::new();
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
            if found.insert(terminal, listed.key).is_some() {
                let why = format!("two terminals are tagged {terminal}; the first is kept");
                self.troubles.say(why);
                continue;
            }
            if let Some(size) = listed.size {
                self.sized.insert(listed.key, size);
            }
        }
        let ended: Vec<Id> = self
            .keys
            .iter()
            .filter(|(terminal, key)| !found.contains_key(terminal) && self.seen.contains(key))
            .map(|(terminal, _)| *terminal)
            .collect();
        for (terminal, key) in found {
            self.keys.insert(terminal, key);
            self.seen.insert(key);
        }
        self.listed = terminals;
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
        let Some(projects) = self.store.projects() else {
            return;
        };
        let mut ran = Vec::new();
        let mut fronts = Vec::new();
        for (terminal, key) in &self.keys {
            let Some(listed) = self.listed.iter().find(|listed| listed.key == *key) else {
                continue;
            };
            let Some((_, held)) = projects.holding(*terminal) else {
                continue;
            };
            let folder = Dir::parse(&listed.cwd).unwrap_or_else(|_| held.folder().clone());
            let mut runs = runs_of(listed, &agents, held.runs(), &self.shell);
            let front = match &listed.is {
                Is::Running { front: Some(front), .. } => Some(front.pid),
                Is::Running { front: None, .. } | Is::Ended => None,
            };
            let was_agent = matches!(held.runs(), Runs::Agent { .. });
            let same_front = front.is_some() && front == self.agent_fronts.get(terminal).copied();
            if was_agent && !matches!(runs, Runs::Agent { .. }) && same_front {
                runs = held.runs().clone();
            }
            let agent_front = front.filter(|_| matches!(runs, Runs::Agent { .. }));
            fronts.push((*terminal, agent_front));
            if folder != *held.folder() || runs != *held.runs() {
                ran.push(Ran { terminal: *terminal, folder, runs });
            }
        }
        for (terminal, front) in fronts {
            match front {
                Some(pid) => self.agent_fronts.insert(terminal, pid),
                None => self.agent_fronts.remove(&terminal),
            };
        }
        if ran.is_empty() {
            return;
        }
        if let Err(why) = self.changed(Change::Ran(ran)) {
            self.troubles.say(format!("what the terminals run is not kept: {why}"));
        }
    }

    fn settle(&mut self) {
        let Views { store, clients, keys, sized, sizes, last, mux, troubles, .. } = self;
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
                client.focus = arrival(shown, last.get(&client.workstream).copied());
            }
        }
        for id in gone {
            clients.remove(&id);
        }
        sizes.retain(|workstream, _| projects.find(*workstream).is_some());
        last.retain(|workstream, terminal| {
            projects.find(*workstream).is_some_and(|(_, it)| it.view().holds(*terminal))
        });
        let shown: BTreeSet<Id> = clients.values().map(|client| client.workstream).collect();
        for workstream in shown {
            let size = Size::smallest(
                clients.values().filter(|it| it.workstream == workstream).map(|it| it.size),
            );
            let size = size.expect("a workstream shown has a client");
            sizes.insert(workstream, size);
            let (_, found) = projects.find(workstream).expect("a client's workstream is held");
            let Some(tree) = found.view().tree() else {
                continue;
            };
            let whole = Rect::whole(tree.fitted(size)).inner();
            for (terminal, rect) in tree.laid(size) {
                let terminal = layout::Leaf::id(terminal);
                let full = found.view().full() == Some(terminal);
                let wanted = if full { whole } else { rect.inner() };
                let Some(key) = keys.get(&terminal).copied() else {
                    continue;
                };
                if sized.get(&key) == Some(&wanted) {
                    continue;
                }
                match mux.sized(key, wanted) {
                    Ok(()) => {
                        sized.insert(key, wanted);
                    },
                    Err(failure) => {
                        troubles.say(format!("terminal {} is not sized: {failure}", key.id));
                    },
                }
            }
        }
        let mut slow = Vec::new();
        for (id, client) in clients.iter_mut() {
            let found = projects.find(client.workstream);
            let (_, found) = found.expect("a client's workstream is held");
            let size = sizes[&client.workstream];
            let laid = laid(found, size, client.focus, keys);
            if client.said.as_ref() == Some(&laid) {
                continue;
            }
            if client.out.try_send(Ok(laid.clone())).is_err() {
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

fn said(failed: Failed) -> String {
    match failed {
        Failed::NotKept(why) | Failed::Refused(why) | Failed::Unwritten(why) => why,
        Failed::KeptUntold => "it is kept, but the daemon's core has stopped".to_owned(),
    }
}

fn workstream_tree(projects: &Projects, workstream: Id) -> layout::Tree<Terminal> {
    let (_, found) = projects.find(workstream).expect("a workstream that holds it is held");
    found.view().tree().expect("a view that holds a terminal is laid").clone()
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
    keys: &BTreeMap<Id, TerminalKey>,
) -> api::ShowResponse {
    let view = workstream.view();
    let (fitted, panes) = match view.tree() {
        None => (size, Vec::new()),
        Some(tree) => {
            let panes = tree.laid(size).into_iter().map(|(terminal, rect)| {
                let terminal = layout::Leaf::id(terminal);
                let key = keys.get(&terminal).map(|key| api::TerminalKey {
                    id: key.id.get(),
                    made_at: Some(key.made.into()),
                });
                show_response::Pane {
                    terminal: terminal.get(),
                    rect: Some(show_response::Rect {
                        column: rect.column.into(),
                        row: rect.row.into(),
                        columns: rect.columns.into(),
                        rows: rect.rows.into(),
                    }),
                    key,
                }
            });
            (tree.fitted(size), panes.collect())
        },
    };
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
    Line::parse(line).map_or(Runs::Shell, Runs::Command)
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
    let Some(first) = argv.first() else {
        return false;
    };
    let name = |path: &Path| path.file_name().map(|name| name.to_string_lossy().into_owned());
    let named = name(Path::new(first.trim_start_matches('-')));
    named.is_some_and(|named| named.ends_with("sh") || Some(named) == name(Path::new(shell)))
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
        let long = "x".repeat(crate::projects::LINE_BYTES_MAX + 1);
        let too_long = running(Some((20, &[long.as_str()])), None);
        assert_eq!(of(&too_long, &[]), Runs::Shell);
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
        let refused = [
            Event::Go(0),
            Event::Focus(show_request::Focus { to: None }),
            Event::Resize(show_request::Resize { towards: 0, grow: true }),
            Event::Size(show_request::Size { columns: 0, rows: 24 }),
            Event::Open(show_request::Open { beside: None, argv: vec![String::new()] }),
            Event::Hello(show_request::Hello { workstream: 1, size: None }),
        ];
        for event in refused {
            assert!(Asked::of_api(event.clone()).is_err(), "{event:?}");
        }
    }
}
