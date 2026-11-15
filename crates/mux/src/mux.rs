use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{
    Act, At, Colours, Ends, FocusTo, Hello, Is, MoveTo, Name, Opening, Opens, Place, Runs,
    SessionName, Size, TurnTo, Who,
};
use crate::id::{ClientId, PaneId, SessionId, TabId};
use crate::layout::{self, Direction, Leaf, Order, Rect, Removed, Tree};
use crate::pane::{self, Exit, Handle, Out, Viewer, Wanted};
use crate::pty::{self, Pty};

const FIRST_SIZE: Size = Size {
    columns: std::num::NonZeroU16::new(80).unwrap(),
    rows: std::num::NonZeroU16::new(24).unwrap(),
};

pub const SESSIONS_MAX: usize = 256;

pub const TABS_MAX: usize = 1024;

pub const PANES_MAX: usize = 1024;

pub const CLIENTS_MAX: usize = 256;

const _: () = assert!(TABS_MAX <= PANES_MAX);

pub type Layouts = watch::Receiver<Option<LaidOut>>;

#[derive(Clone, Debug, PartialEq)]
pub struct LaidOut {
    pub layout: api::Layout,
    pub wents: u64,
}

pub struct Mux {
    model: Mutex<Model>,
    listing: watch::Sender<Option<api::Listing>>,
}

#[derive(Default)]
struct Model {
    last: Ids,
    sessions: Vec<Session>,
    clients: BTreeMap<ClientId, Seat>,
    ended: bool,
}

#[derive(Default)]
struct Ids {
    session: Option<SessionId>,
    tab: Option<TabId>,
    pane: Option<PaneId>,
    client: Option<ClientId>,
    came: u64,
}

struct Session {
    id: SessionId,
    made: SystemTime,
    name: SessionName,
    tabs: Tabs,
    last_typed: Option<ClientId>,
}

struct Tabs {
    tabs: Vec<Tab>,
    current: usize,
}

enum Closed {
    Absent,
    Last,
    Out { tab: Box<Tab>, next: PaneId },
}

struct Tab {
    id: TabId,
    name: Name,
    tree: Tree<Pane>,
    focused: PaneId,
    fullscreen: bool,
}

struct Pane {
    id: PaneId,
    name: Option<Name>,
    title: String,
    cwd: PathBuf,
    program: Program,
    handle: Handle,
}

enum Program {
    Runs { pid: u32, ends: Ends, foreground: Option<api::Foreground>, ran: Option<api::Ran> },
    Ended(Exit),
}

struct Seat {
    pane: PaneId,
    size: Size,
    colours: Colours,
    out: Out,
    layout: watch::Sender<Option<LaidOut>>,
    came: u64,
    wents: u64,
    _gone: oneshot::Sender<()>,
}

impl Mux {
    #[must_use]
    pub fn new() -> Arc<Mux> {
        let (listing, _) = watch::channel(Some(api::Listing::default()));
        Arc::new(Mux { model: Mutex::new(Model::default()), listing })
    }

    pub fn watched(&self) -> watch::Receiver<Option<api::Listing>> {
        self.listing.subscribe()
    }

    pub fn end_if_unused(&self) {
        self.change(|model| {
            if model.sessions.is_empty() {
                model.ended = true;
            }
        });
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.model.lock().expect("a thread panicked while it held the model")
    }

    fn change<T>(&self, change: impl FnOnce(&mut Model) -> T) -> T {
        let mut model = self.lock();
        let changed = change(&mut model);
        if model.ended {
            assert!(model.sessions.is_empty(), "the server ended with a session");
        }
        model.settle();
        let listing = (!model.ended).then(|| model.listing());
        self.listing.send_if_modified(|said| {
            let modified = *said != listing;
            *said = listing;
            modified
        });
        changed
    }

    pub fn new_session(
        self: &Arc<Mux>,
        name: SessionName,
        opens: &Opens,
    ) -> Result<(SessionId, SystemTime, TabId, PaneId), Status> {
        self.change(|model| {
            model.refuse_if_ended()?;
            model.refuse_if_taken(&name, None)?;
            refuse_if_full(model.sessions.len(), SESSIONS_MAX, "sessions")?;
            let session = SessionId::next(&mut model.last.session);
            let made = SystemTime::now();
            let tab = model.tab(self, (session, made), Name::numbered(1), opens, FIRST_SIZE)?;
            let ids = (session, made, tab.id, tab.focused);
            let tabs = Tabs::one(tab);
            model.sessions.push(Session { id: session, made, name, tabs, last_typed: None });
            Ok(ids)
        })
    }

    pub fn new_tab(
        self: &Arc<Mux>,
        session: SessionId,
        name: Option<Name>,
        opens: &Opens,
    ) -> Result<(TabId, PaneId), Status> {
        self.change(|model| {
            let (place, made, size) = {
                let session = model.session(session)?;
                (session.tabs.len() + 1, session.made, session.tabs.current().tree.size())
            };
            let name = name.unwrap_or_else(|| Name::numbered(place));
            let tab = model.tab(self, (session, made), name, opens, size)?;
            let ids = (tab.id, tab.focused);
            model.session_mut(session)?.tabs.push(tab);
            Ok(ids)
        })
    }

    pub fn new_pane(self: &Arc<Mux>, at: At, opens: &Opens) -> Result<PaneId, Status> {
        self.change(|model| model.new_pane(self, at, opens))
    }

    pub fn close_tab(&self, tab: TabId) -> Result<(), Status> {
        self.change(|model| {
            let (session, _) = model.tab_at(tab)?;
            model.close(session, tab);
            Ok(())
        })
    }

    pub fn close_pane(&self, pane: PaneId) -> Result<(), Status> {
        self.change(|model| {
            model.pane_at(pane)?;
            model.close_pane(pane);
            Ok(())
        })
    }

    pub fn turn(&self, pane: PaneId, to: TurnTo) -> Result<TabId, Status> {
        self.change(|model| model.turn(pane, to))
    }

    pub fn kill(&self, session: SessionId) -> Result<(), Status> {
        self.change(|model| {
            model.session(session)?;
            model.end(session);
            Ok(())
        })
    }

    pub fn rename(&self, session: SessionId, name: SessionName) -> Result<(), Status> {
        self.change(|model| {
            model.session(session)?;
            model.refuse_if_taken(&name, Some(session))?;
            model.session_mut(session)?.name = name;
            Ok(())
        })
    }

    pub fn go(&self, who: Who, to: Place) -> Result<ClientId, Status> {
        self.change(|model| {
            let client = match who {
                Who::Client(client) => client,
                Who::LastTypedIn(session) => model.last_typed_in(session)?,
            };
            model.take(client, to)?;
            Ok(client)
        })
    }

    pub fn act(self: &Arc<Mux>, client: ClientId, act: Act) -> Result<(), Status> {
        self.change(|model| model.act(self, client, act))
    }

    pub fn pane(&self, pane: PaneId) -> Result<Handle, Status> {
        Ok(self.lock().pane(pane)?.handle.clone())
    }

    pub fn listing(&self) -> api::Listing {
        self.lock().listing()
    }

    pub fn attach(
        &self,
        hello: Hello,
        out: Out,
        gone: oneshot::Sender<()>,
    ) -> Result<(ClientId, Layouts), Status> {
        self.change(|model| {
            refuse_if_full(model.clients.len(), CLIENTS_MAX, "clients")?;
            let pane = model.pane_of_place(hello.to)?;
            let client = ClientId::next(&mut model.last.client);
            let attached = api::ToClient {
                event: Some(api::to_client::Event::Attached(api::Attached {
                    client: client.said(),
                })),
            };
            out.try_send(Ok(attached)).expect("a new client's stream has room for its first word");
            let (size, colours) = (hello.size, hello.colours);
            let (layout, layouts) = watch::channel(None);
            let seat = Seat { pane, size, colours, out, layout, came: 0, wents: 0, _gone: gone };
            model.clients.insert(client, seat);
            model.seat_on(client, pane)?;
            Ok((client, layouts))
        })
    }

    pub fn detach(&self, client: ClientId) {
        self.change(|model| model.drop_client(client));
    }

    pub fn input(&self, client: ClientId, types: bool) -> Option<Handle> {
        let mut model = self.lock();
        let pane = model.clients.get(&client)?.pane;
        let (session, _) = model.pane_at(pane).expect("a client is on a pane of the model");
        let session = session.id;
        let handle = model.pane(pane).expect("a client is on a pane of the model").handle.clone();
        if types {
            model.session_mut(session).expect("a pane's session").last_typed = Some(client);
        }
        Some(handle)
    }

    pub fn resize(&self, client: ClientId, size: Size) {
        self.change(|model| {
            if let Some(seat) = model.clients.get_mut(&client) {
                seat.size = size;
            }
        });
    }

    pub fn lost(&self, pane: PaneId) {
        self.change(|model| model.close_pane(pane));
    }

    pub fn titled(&self, pane: PaneId, title: String, cwd: Option<PathBuf>) {
        self.change(|model| {
            if let Some(pane) = model.pane_mut(pane) {
                pane.title = title;
                if let Some(cwd) = cwd {
                    pane.cwd = cwd;
                }
            }
        });
    }

    pub fn looked(&self, pane: PaneId, foreground: Option<api::Foreground>, ran: Option<api::Ran>) {
        self.change(|model| {
            if let Some(Pane { program: Program::Runs { foreground: was, ran: said, .. }, .. }) =
                model.pane_mut(pane)
            {
                (*was, *said) = (foreground, ran);
            }
        });
    }

    pub fn ended(&self, pane: PaneId, exit: Exit) -> bool {
        self.change(|model| {
            let Some(found) = model.pane_mut(pane) else {
                return true;
            };
            if let Program::Runs { ends: Ends::Closes, .. } = found.program {
                model.close_pane(pane);
                return true;
            }
            found.program = Program::Ended(exit);
            false
        })
    }
}

impl Model {
    fn refuse_if_ended(&self) -> Result<(), Status> {
        if self.ended {
            return Err(Status::unavailable("luneta-mux is ending, as its last session did"));
        }
        Ok(())
    }

    fn refuse_if_taken(&self, name: &SessionName, renamed: Option<SessionId>) -> Result<(), Status> {
        let taken = self
            .sessions
            .iter()
            .any(|session| &session.name == name && Some(session.id) != renamed);
        if taken {
            return Err(Status::already_exists(format!("a session is named {}", name.as_str())));
        }
        Ok(())
    }

    fn refuse_if_no_room_for_a_tab(&self) -> Result<(), Status> {
        refuse_if_full(self.tabs().count(), TABS_MAX, "tabs")
    }

    fn refuse_if_no_room_for_a_pane(&self) -> Result<(), Status> {
        let panes = self.tabs().map(|(_, tab)| tab.tree.len()).sum();
        refuse_if_full(panes, PANES_MAX, "panes")
    }

    fn tabs(&self) -> impl Iterator<Item = (&Session, &Tab)> {
        self.sessions.iter().flat_map(|session| session.tabs.iter().map(move |tab| (session, tab)))
    }

    fn tab(
        &mut self,
        mux: &Arc<Mux>,
        session: (SessionId, SystemTime),
        name: Name,
        opens: &Opens,
        size: Size,
    ) -> Result<Tab, Status> {
        self.refuse_if_no_room_for_a_tab()?;
        self.refuse_if_no_room_for_a_pane()?;
        let tab = TabId::next(&mut self.last.tab);
        let id = PaneId::next(&mut self.last.pane);
        let pane = spawned(mux, session, id, opens, layout::lone(size))?;
        Ok(Tab::one(tab, name, pane, size))
    }

    fn new_pane(&mut self, mux: &Arc<Mux>, at: At, opens: &Opens) -> Result<PaneId, Status> {
        self.refuse_if_no_room_for_a_pane()?;
        let (session, tab) = match at {
            At::Beside { pane, .. } => {
                let (session, tab) = self.pane_at(pane)?;
                (session.id, tab.id)
            },
            At::Tab(tab) => self.tab_at(tab)?,
        };
        let new = PaneId::next(&mut self.last.pane);
        let tree = &self.tab_ref(tab).tree;
        let room = match at {
            At::Beside { pane, towards } => {
                tree.room(pane, towards, new).map(|size| (pane, towards, size))
            },
            At::Tab(_) => tree.room_beside_largest(new),
        };
        let (beside, towards, size) = room.ok_or_else(|| no_room(tab))?;
        let made = self.session(session).expect("a tab's session is the model's").made;
        let pane = spawned(mux, (session, made), new, opens, size)?;
        let found = self.tab_mut(tab);
        found.tree.split(beside, towards, pane);
        found.fullscreen = false;
        Ok(new)
    }

    fn act(&mut self, mux: &Arc<Mux>, client: ClientId, act: Act) -> Result<(), Status> {
        let pane = self.seat(client)?.pane;
        let (session, tab) = self.pane_at(pane).expect("a client is on a pane of the model");
        let (session, tab) = (session.id, tab.id);
        match act {
            Act::Open(opening) => {
                let at = match opening {
                    Opening::Largest => At::Tab(tab),
                    Opening::Beside(towards) => At::Beside { pane, towards },
                };
                let cwd = self.pane(pane).expect("a client's pane").cwd.clone();
                let opens = Opens { cwd, runs: Runs::Shell, ends: Ends::Closes };
                let new = self.new_pane(mux, at, &opens)?;
                self.seat_on(client, new)
            },
            Act::ClosePane => {
                self.close_pane(pane);
                Ok(())
            },
            Act::Focus(to) => self.focus(client, (session, tab, pane), to),
            Act::Resize { change, towards } => {
                let found = self.tab_mut(tab);
                let directions = towards.map_or(Direction::ALL.to_vec(), |towards| vec![towards]);
                for direction in directions {
                    found.tree.resize(pane, direction, change);
                }
                Ok(())
            },
            Act::Move(to) => {
                let found = self.tab_mut(tab);
                let other = match to {
                    MoveTo::Towards(direction) => found.tree.neighbour(pane, direction),
                    MoveTo::InOrder(order) => found.tree.in_order(pane, order),
                };
                if let Some(other) = other {
                    found.tree.swap(pane, other);
                }
                Ok(())
            },
            Act::Fullscreen => {
                let found = self.tab_mut(tab);
                found.fullscreen = !found.fullscreen && found.tree.len() > 1;
                if !found.fullscreen {
                    return Ok(());
                }
                found.focused = pane;
                let found = self.tab_ref(tab);
                let others = self.seated_on(|seat| seat != pane && found.holds(seat));
                self.seat_all_on(others, pane);
                Ok(())
            },
            Act::NamePane(name) => {
                self.pane_mut(pane).expect("a client's pane").name = name;
                Ok(())
            },
            Act::NameTab(name) => {
                self.tab_mut(tab).name = name;
                Ok(())
            },
            Act::MoveTab(order) => {
                self.session_mut(session).expect("a tab's session").tabs.shift(tab, order);
                Ok(())
            },
            Act::Turn(to) => self.turn(pane, to).map(drop),
        }
    }

    fn focus(
        &mut self,
        client: ClientId,
        (session, tab, pane): (SessionId, TabId, PaneId),
        to: FocusTo,
    ) -> Result<(), Status> {
        let found = self.tab_ref(tab);
        let next = match to {
            FocusTo::Towards(direction) => found.tree.neighbour(pane, direction),
            FocusTo::InOrder(order) => found.tree.in_order(pane, order),
            FocusTo::TowardsOrTab(order) => {
                let direction = match order {
                    Order::Next => Direction::Right,
                    Order::Previous => Direction::Left,
                };
                found.tree.neighbour(pane, direction).or_else(|| {
                    let tabs = &self.session(session).expect("a tab's session").tabs;
                    tabs.in_order(tab, order).map(|tab| tab.focused)
                })
            },
        };
        match next {
            Some(next) => self.seat_on(client, next),
            None => Ok(()),
        }
    }

    fn turn(&mut self, pane: PaneId, to: TurnTo) -> Result<TabId, Status> {
        let (session, from) = self.pane_at(pane)?;
        let (session, from, alone) = (session.id, from.id, from.tree.len() == 1);
        let placed = match to {
            TurnTo::NewTab(name) => {
                if alone {
                    return Err(Status::failed_precondition(format!(
                        "pane {pane} is alone in its tab"
                    )));
                }
                self.refuse_if_no_room_for_a_tab()?;
                let size = self.tab_ref(from).tree.size();
                let moved = self.unhooked(session, from, pane);
                let id = TabId::next(&mut self.last.tab);
                let tabs = &mut self.session_mut(session).expect("a tab's session").tabs;
                let name = name.unwrap_or_else(|| Name::numbered(tabs.len() + 1));
                tabs.push(Tab::one(id, name, moved, size));
                id
            },
            TurnTo::Tab(to) => {
                let (into, _) = self.tab_at(to)?;
                if into != session {
                    let refused = format!("tab {to} is in a session other than pane {pane}'s");
                    return Err(Status::failed_precondition(refused));
                }
                if to == from {
                    return Err(Status::failed_precondition(format!(
                        "pane {pane} is in tab {to} already"
                    )));
                }
                let room = self.tab_ref(to).tree.room_beside_largest(pane);
                let (beside, towards, _) = room.ok_or_else(|| no_room(to))?;
                let moved = self.unhooked(session, from, pane);
                let found = self.tab_mut(to);
                found.tree.split(beside, towards, moved);
                found.fullscreen = false;
                to
            },
        };
        let following = self.seated_on(|seat| seat == pane);
        self.seat_all_on(following, pane);
        Ok(placed)
    }

    fn unhooked(&mut self, session: SessionId, tab: TabId, pane: PaneId) -> Pane {
        let found = self.tab_mut(tab);
        match found.tree.remove(pane) {
            Removed::Last => {
                let tabs = &mut self.session_mut(session).expect("a tab's session").tabs;
                let Closed::Out { tab, .. } = tabs.close(tab) else {
                    unreachable!("a pane leaves a lone tab only for another tab of its session");
                };
                tab.tree.into_lone()
            },
            Removed::Out { gone, heir } => {
                found.left(pane, heir);
                gone
            },
        }
    }

    fn session(&self, session: SessionId) -> Result<&Session, Status> {
        self.sessions
            .iter()
            .find(|found| found.id == session)
            .ok_or_else(|| Status::not_found(format!("no session {session}")))
    }

    fn session_mut(&mut self, session: SessionId) -> Result<&mut Session, Status> {
        self.sessions
            .iter_mut()
            .find(|found| found.id == session)
            .ok_or_else(|| Status::not_found(format!("no session {session}")))
    }

    fn tab_at(&self, tab: TabId) -> Result<(SessionId, TabId), Status> {
        self.tabs()
            .find(|(_, found)| found.id == tab)
            .map(|(session, _)| (session.id, tab))
            .ok_or_else(|| Status::not_found(format!("no tab {tab}")))
    }

    fn tab_ref(&self, tab: TabId) -> &Tab {
        let found = self.tabs().find(|(_, found)| found.id == tab);
        found.expect("a tab found before is the model's").1
    }

    fn tab_mut(&mut self, tab: TabId) -> &mut Tab {
        let mut tabs = self.sessions.iter_mut().flat_map(|session| session.tabs.iter_mut());
        tabs.find(|found| found.id == tab).expect("a tab found before is the model's")
    }

    fn pane_at(&self, pane: PaneId) -> Result<(&Session, &Tab), Status> {
        self.tabs()
            .find(|(_, tab)| tab.holds(pane))
            .ok_or_else(|| Status::not_found(format!("no pane {pane}")))
    }

    fn pane(&self, pane: PaneId) -> Result<&Pane, Status> {
        self.tabs()
            .find_map(|(_, tab)| tab.tree.get(pane))
            .ok_or_else(|| Status::not_found(format!("no pane {pane}")))
    }

    fn pane_mut(&mut self, pane: PaneId) -> Option<&mut Pane> {
        self.sessions
            .iter_mut()
            .flat_map(|session| session.tabs.iter_mut())
            .find_map(|tab| tab.tree.get_mut(pane))
    }

    fn pane_of_place(&self, place: Place) -> Result<PaneId, Status> {
        let (session, pane) = match place.is {
            Is::Pane(pane) => {
                let (session, _) = self.pane_at(pane)?;
                (session, pane)
            },
            Is::Tab(tab) => {
                let (session, tab) = self.tab_at(tab)?;
                let session = self.session(session).expect("a tab's session is the model's");
                (session, self.tab_ref(tab).focused)
            },
            Is::Session(session) => {
                let session = self.session(session)?;
                (session, session.tabs.current().focused)
            },
        };
        if place.made != session.made {
            return Err(Status::not_found(format!("no session {} made then", session.id)));
        }
        Ok(pane)
    }

    fn seat(&self, client: ClientId) -> Result<&Seat, Status> {
        self.clients.get(&client).ok_or_else(|| Status::not_found(format!("no client {client}")))
    }

    fn seated_on(&self, on: impl Fn(PaneId) -> bool) -> Vec<ClientId> {
        self.clients.iter().filter(|(_, seat)| on(seat.pane)).map(|(client, _)| *client).collect()
    }

    fn last_typed_in(&self, session: SessionId) -> Result<ClientId, Status> {
        let found = self.session(session)?;
        let on = |client: &ClientId| {
            self.clients
                .get(client)
                .is_some_and(|seat| found.tabs.iter().any(|tab| tab.holds(seat.pane)))
        };
        if let Some(client) = found.last_typed.filter(on) {
            return Ok(client);
        }
        self.clients
            .iter()
            .filter(|(client, _)| on(client))
            .max_by_key(|(_, seat)| seat.came)
            .map(|(client, _)| *client)
            .ok_or_else(|| {
                Status::failed_precondition(format!("no client is on session {session}"))
            })
    }

    fn take(&mut self, client: ClientId, to: Place) -> Result<(), Status> {
        self.seat(client)?;
        let pane = self.pane_of_place(to)?;
        self.seat_on(client, pane)
    }

    fn seat_on(&mut self, client: ClientId, pane: PaneId) -> Result<(), Status> {
        let (session, tab) = self.pane_at(pane).expect("a client is seated on a pane of the model");
        let (session, tab) = (session.id, tab.id);
        self.last.came += 1;
        let came = self.last.came;
        let seat = self.clients.get_mut(&client).expect("a seated client is the model's");
        let went = api::ToClient {
            event: Some(api::to_client::Event::Went(api::Went {
                session: session.said(),
                tab: tab.said(),
                pane: pane.said(),
            })),
        };
        if seat.out.try_send(Ok(went)).is_err() {
            self.drop_client(client);
            return Err(Status::resource_exhausted(format!("client {client} takes no message")));
        }
        seat.pane = pane;
        seat.came = came;
        seat.wents += 1;
        self.session_mut(session).expect("a pane's session").tabs.make_current(tab);
        let found = self.tab_mut(tab);
        if found.focused != pane {
            found.fullscreen = false;
        }
        found.focused = pane;
        Ok(())
    }

    fn seat_all_on(&mut self, clients: Vec<ClientId>, pane: PaneId) {
        for client in clients {
            if self.seat_on(client, pane).is_err() {
                assert!(!self.clients.contains_key(&client), "a client refused a seat is dropped");
            }
        }
    }

    fn drop_client(&mut self, client: ClientId) {
        self.clients.remove(&client);
    }

    fn close_pane(&mut self, pane: PaneId) {
        let Ok((session, tab)) = self.pane_at(pane) else {
            return;
        };
        let (session, tab) = (session.id, tab.id);
        let found = self.tab_mut(tab);
        let (closed, heir) = match found.tree.remove(pane) {
            Removed::Last => {
                self.close(session, tab);
                return;
            },
            Removed::Out { gone, heir } => (gone, heir),
        };
        closed.handle.close();
        found.left(pane, heir);
        let seated = self.seated_on(|seat| seat == pane);
        self.seat_all_on(seated, heir);
    }

    fn close(&mut self, session: SessionId, tab: TabId) {
        let Ok(found) = self.session_mut(session) else {
            return;
        };
        let (closed, next_pane) = match found.tabs.close(tab) {
            Closed::Absent => return,
            Closed::Last => {
                self.end(session);
                return;
            },
            Closed::Out { tab, next } => (tab, next),
        };
        for pane in closed.tree.leaves() {
            pane.handle.close();
        }
        let seated = self.seated_on(|seat| closed.holds(seat));
        self.seat_all_on(seated, next_pane);
    }

    fn end(&mut self, session: SessionId) {
        let Some(place) = self.sessions.iter().position(|found| found.id == session) else {
            return;
        };
        let ended = self.sessions.remove(place);
        for pane in ended.tabs.iter().flat_map(|tab| tab.tree.leaves()) {
            pane.handle.close();
        }
        let gone = self.seated_on(|seat| ended.tabs.iter().any(|tab| tab.holds(seat)));
        for client in gone {
            if let Some(seat) = self.clients.remove(&client) {
                let word =
                    api::ToClient { event: Some(api::to_client::Event::Ended(api::Ended {})) };
                let _ = seat.out.try_send(Ok(word));
            }
        }
        if self.sessions.is_empty() {
            self.ended = true;
        }
    }

    fn settle(&mut self) {
        let clients = &self.clients;
        let mut seated_by_tab: Vec<Vec<(ClientId, &Seat)>> = Vec::new();
        for tab in self.sessions.iter_mut().flat_map(|session| session.tabs.iter_mut()) {
            tab.assert_whole();
            let seated: Vec<(ClientId, &Seat)> = clients
                .iter()
                .filter(|(_, seat)| tab.holds(seat.pane))
                .map(|(client, seat)| (*client, seat))
                .collect();
            if tab.fullscreen {
                let full = tab.focused;
                let off = seated.iter().find(|(_, seat)| seat.pane != full).map(|(id, _)| *id);
                assert!(off.is_none(), "client {off:?} is off pane {full}, which fills its tab");
            }
            if let Some(size) = smallest(seated.iter().map(|(_, seat)| seat.size)) {
                tab.tree.fit(size);
            }
            seated_by_tab.push(seated);
        }
        let mut seated_by_tab = seated_by_tab.into_iter();
        let mut laid_out = 0;
        for session in &self.sessions {
            for tab in session.tabs.iter() {
                let seated = seated_by_tab.next().expect("settle seats every tab once");
                if !seated.is_empty() {
                    let laid = tab.layout(session);
                    for (_, seat) in &seated {
                        let mut layout = laid.clone();
                        if let Some(api::layout::Shown::Tiled(tiled)) = &mut layout.shown {
                            tiled.focused = seat.pane.said();
                        }
                        let laid_out = Some(LaidOut { layout, wents: seat.wents });
                        seat.layout.send_if_modified(|said| {
                            let modified = *said != laid_out;
                            *said = laid_out;
                            modified
                        });
                    }
                }
                laid_out += seated.len();
                let viewers: Arc<[Viewer]> = seated
                    .iter()
                    .map(|(client, seat)| Viewer {
                        client: *client,
                        out: seat.out.clone(),
                        colours: seat.colours,
                        came: seat.came,
                    })
                    .collect();
                let whole = Rect::whole(tab.tree.size()).inner();
                for (pane, rect) in tab.tree.laid() {
                    let full = tab.fullscreen && pane.id == tab.focused;
                    let size = if full { whole } else { rect.inner() };
                    pane.handle.want(Wanted { size, viewers: Arc::clone(&viewers) });
                }
            }
        }
        assert!(seated_by_tab.next().is_none(), "settle seats no tab it did not see");
        assert_eq!(laid_out, self.clients.len(), "every client sits on a pane of the model");
    }

    fn listing(&self) -> api::Listing {
        let sessions = self.sessions.iter().map(|session| api::Session {
            id: session.id.said(),
            name: session.name.as_str().to_owned(),
            tabs: session.tabs.iter().map(|tab| self.tab_said(tab)).collect(),
            current_tab: session.tabs.current().id.said(),
            made_at: Some(session.made.into()),
        });
        api::Listing { sessions: sessions.collect() }
    }

    fn tab_said(&self, tab: &Tab) -> api::Tab {
        let panes = tab.tree.leaves().map(|pane| self.pane_said(pane));
        api::Tab { id: tab.id.said(), name: tab.name.as_str().to_owned(), panes: panes.collect() }
    }

    fn pane_said(&self, pane: &Pane) -> api::Pane {
        let mut clients: Vec<(ClientId, &Seat)> = self
            .clients
            .iter()
            .filter(|(_, seat)| seat.pane == pane.id)
            .map(|(client, seat)| (*client, seat))
            .collect();
        clients.sort_by_key(|(_, seat)| seat.came);
        let (program, foreground, ran) = match &pane.program {
            Program::Runs { pid, foreground, ran, .. } => {
                (api::pane::Program::Pid(*pid), foreground.clone(), ran.clone())
            },
            Program::Ended(exit) => (api::pane::Program::Ended(exit.said()), None, None),
        };
        let cwd = pane.cwd.to_str().expect("a pane's cwd was said in UTF-8");
        api::Pane {
            id: pane.id.said(),
            title: pane.title.clone(),
            cwd: cwd.to_owned(),
            program: Some(program),
            foreground,
            ran,
            clients: clients
                .into_iter()
                .map(|(id, seat)| api::Client { id: id.said(), size: Some(seat.size.said()) })
                .collect(),
            name: pane.name.as_ref().map(|name| name.as_str().to_owned()),
        }
    }
}

impl Leaf for Pane {
    fn id(&self) -> PaneId {
        self.id
    }
}

impl Tab {
    fn one(id: TabId, name: Name, pane: Pane, size: Size) -> Tab {
        let focused = pane.id;
        Tab { id, name, tree: Tree::one(pane, size), focused, fullscreen: false }
    }

    fn holds(&self, pane: PaneId) -> bool {
        self.tree.holds(pane)
    }

    fn left(&mut self, pane: PaneId, heir: PaneId) {
        if self.focused == pane {
            self.focused = heir;
            self.fullscreen = false;
        }
    }

    fn assert_whole(&self) {
        assert!(self.holds(self.focused), "tab {} focuses a pane of its own", self.id);
    }

    fn layout(&self, session: &Session) -> api::Layout {
        let placed = |(pane, rect): (&Pane, Rect)| {
            let title = pane.name.as_ref().map_or(pane.title.as_str(), Name::as_str);
            api::Placed {
                pane: pane.id.said(),
                rect: Some(rect_said(rect)),
                title: title.to_owned(),
            }
        };
        let size = self.tree.size();
        let shown = if self.fullscreen {
            let full = self.tree.get(self.focused).expect("a tab focuses a pane of its own");
            let laid = self.tree.laid().into_iter().map(|(pane, _)| pane.id);
            let hidden: Vec<u32> = laid.filter(|pane| *pane != full.id).map(PaneId::said).collect();
            assert_eq!(hidden.len() + 1, self.tree.laid().len(), "a full pane hides the others");
            let pane = Some(placed((full, Rect::whole(size))));
            api::layout::Shown::Full(api::Full { pane, hidden })
        } else {
            let panes = self.tree.laid().into_iter().map(placed).collect();
            api::layout::Shown::Tiled(api::Tiled { panes, focused: self.focused.said() })
        };
        let tabs = session.tabs.iter().map(|tab| api::Tabbed {
            id: tab.id.said(),
            name: tab.name.as_str().to_owned(),
        });
        api::Layout {
            session: session.id.said(),
            tab: self.id.said(),
            size: Some(size.said()),
            shown: Some(shown),
            tabs: tabs.collect(),
        }
    }
}

impl Tabs {
    fn one(tab: Tab) -> Tabs {
        Tabs { tabs: vec![tab], current: 0 }
    }

    fn current(&self) -> &Tab {
        &self.tabs[self.current]
    }

    fn len(&self) -> usize {
        self.tabs.len()
    }

    fn iter(&self) -> std::slice::Iter<'_, Tab> {
        self.tabs.iter()
    }

    fn iter_mut(&mut self) -> std::slice::IterMut<'_, Tab> {
        self.tabs.iter_mut()
    }

    fn push(&mut self, tab: Tab) {
        self.tabs.push(tab);
    }

    fn place(&self, tab: TabId) -> Option<usize> {
        self.tabs.iter().position(|found| found.id == tab)
    }

    fn make_current(&mut self, tab: TabId) {
        self.current = self.place(tab).expect("a tab of the session is made current");
    }

    fn in_order(&self, tab: TabId, order: Order) -> Option<&Tab> {
        let place = self.place(tab).expect("a tab of the session is stepped from");
        let count = self.tabs.len();
        (count > 1).then(|| &self.tabs[order.stepped(place, count)])
    }

    fn shift(&mut self, tab: TabId, order: Order) {
        let place = self.place(tab).expect("a tab of the session is shifted");
        let other = match order {
            Order::Next => place.checked_add(1),
            Order::Previous => place.checked_sub(1),
        };
        let Some(other) = other.filter(|other| *other < self.tabs.len()) else {
            return;
        };
        self.tabs.swap(place, other);
        if self.current == place {
            self.current = other;
        } else if self.current == other {
            self.current = place;
        }
    }

    fn close(&mut self, tab: TabId) -> Closed {
        let Some(place) = self.place(tab) else {
            return Closed::Absent;
        };
        if self.tabs.len() == 1 {
            return Closed::Last;
        }
        let tab = self.tabs.remove(place);
        let next = place.saturating_sub(1);
        if self.current >= place {
            self.current = self.current.saturating_sub(1);
        }
        Closed::Out { tab: Box::new(tab), next: self.tabs[next].focused }
    }
}

fn spawned(
    mux: &Arc<Mux>,
    (session, made): (SessionId, SystemTime),
    pane: PaneId,
    opens: &Opens,
    size: Size,
) -> Result<Pane, Status> {
    let made = made.duration_since(UNIX_EPOCH).ok();
    let made = made.and_then(|since| u64::try_from(since.as_nanos()).ok()).ok_or_else(|| {
        Status::failed_precondition("the clock says a time before 1970, or after 2554")
    })?;
    let failed =
        |what: &str, error: std::io::Error| Status::internal(format!("{what}: {error}"));
    let pty = Pty::opened(size).map_err(|error| failed("no pseudoterminal opened", error))?;
    let vars = [
        (pty::SESSION_VAR, session.to_string()),
        (pty::MADE_VAR, made.to_string()),
        (pty::PANE_VAR, pane.to_string()),
    ];
    let child = pty.spawned(opens, &vars).map_err(|error| {
        Status::failed_precondition(format!("opens did not start: {error}"))
    })?;
    let started = pane::start(Arc::clone(mux), pane, pty, child, size)
        .map_err(|error| failed("the pane did not start", error))?;
    Ok(Pane {
        id: pane,
        name: None,
        title: String::new(),
        cwd: opens.cwd.clone(),
        program: Program::Runs { pid: started.pid, ends: opens.ends, foreground: None, ran: None },
        handle: started.handle,
    })
}

fn no_room(tab: TabId) -> Status {
    Status::failed_precondition(format!("tab {tab} has no room for another pane"))
}

fn smallest(sizes: impl Iterator<Item = Size>) -> Option<Size> {
    sizes.reduce(|smallest, size| Size {
        columns: smallest.columns.min(size.columns),
        rows: smallest.rows.min(size.rows),
    })
}

fn rect_said(rect: Rect) -> api::Rect {
    api::Rect {
        column: rect.column.into(),
        row: rect.row.into(),
        columns: rect.columns.into(),
        rows: rect.rows.into(),
    }
}

fn refuse_if_full(count: usize, max: usize, what: &str) -> Result<(), Status> {
    if count < max {
        return Ok(());
    }
    Err(Status::resource_exhausted(format!("luneta-mux holds {max} {what} at most")))
}
