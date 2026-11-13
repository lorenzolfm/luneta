//! The server's sessions, their tabs and the pane of each, and the clients on
//! them: one model behind one lock, which every change goes through, and which
//! tells the watchers of each.
//!
//! Ids are counted up from 1 and never given again, so a session, a tab or a
//! pane keeps its id until it ends, and a rename changes only its name.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Ends, Hello, Is, Name, Opens, Place, Size, Who};
use crate::id::{ClientId, PaneId, SessionId, TabId};
use crate::pane::{self, Busy, Exit, Handle, Out, ToPane, Viewer};
use crate::pty::{self, Pty};

/// The size a pane starts at, until a client comes to it.
const FIRST_SIZE: Size = Size {
    columns: std::num::NonZeroU16::new(80).unwrap(),
    rows: std::num::NonZeroU16::new(24).unwrap(),
};

/// The most sessions a server holds.
pub const SESSIONS_MAX: usize = 256;

/// The most tabs a server holds, across its sessions: each has a pane, with
/// its program, its pseudoterminal, two threads and its scrollback.
pub const TABS_MAX: usize = 1024;

/// The most clients a server holds.
pub const CLIENTS_MAX: usize = 256;

/// The model, and the watchers of it.
pub struct Mux {
    model: Mutex<Model>,
    /// The sessions as they stand, for the watchers: none once the last ended,
    /// and the server with it.
    listing: watch::Sender<Option<api::Listing>>,
}

#[derive(Default)]
struct Model {
    last: Ids,
    sessions: Vec<Session>,
    clients: BTreeMap<ClientId, Seat>,
    /// Whether the last session ended, which ends the server: it makes none
    /// after.
    ended: bool,
}

/// The last id given of each kind.
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
    /// When the server made it, which `id` tells it from the sessions of
    /// every other server by.
    made: SystemTime,
    name: Name,
    tabs: Tabs,
    last_typed: Option<ClientId>,
}

/// A session's tabs, in their order and never none, and the one a client
/// coming to the session goes to.
struct Tabs {
    tabs: Vec<Tab>,
    current: usize,
}

/// What became of a session's tabs as one of them closed.
enum Closed {
    /// It was none of them.
    Absent,
    /// It was the last, which stays for the session to end with.
    Last,
    /// It is out of them, and its clients go to the pane `next`.
    Out { tab: Box<Tab>, next: PaneId },
}

struct Tab {
    id: TabId,
    name: Name,
    ends: Ends,
    pane: Pane,
}

struct Pane {
    id: PaneId,
    title: String,
    /// Where the program last said it works, on this machine, or else where
    /// the pane opened.
    cwd: PathBuf,
    program: Program,
    handle: Handle,
}

/// The program a pane started: it runs, or it ended, in a tab that holds.
enum Program {
    /// It runs, with the leader of the group in front of its terminal, and
    /// what its shell said, as its thread last looked.
    Runs { pid: u32, foreground: Option<api::Foreground>, ran: Option<api::Ran> },
    Ended(Exit),
}

/// Where a client is, and what it is.
struct Seat {
    pane: PaneId,
    size: Size,
    colours: Colours,
    out: Out,
    /// When it came to its pane, by the count of the comings.
    came: u64,
    /// Dropped as the seat is, which ends the client's stream.
    _gone: oneshot::Sender<()>,
}

impl Mux {
    #[must_use]
    pub fn new() -> Arc<Mux> {
        let (listing, _) = watch::channel(Some(api::Listing::default()));
        Arc::new(Mux { model: Mutex::new(Model::default()), listing })
    }

    /// The sessions as they stand, then each time they change, then none as
    /// the server ends.
    pub fn watched(&self) -> watch::Receiver<Option<api::Listing>> {
        self.listing.subscribe()
    }

    /// Ends the server where no session was ever made in it.
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

    /// Runs `change` on the model, and tells the watchers what it changed.
    fn change<T>(&self, change: impl FnOnce(&mut Model) -> T) -> T {
        let mut model = self.lock();
        let changed = change(&mut model);
        if model.ended {
            assert!(model.sessions.is_empty(), "the server ended with a session");
        }
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
        name: Name,
        opens: &Opens,
    ) -> Result<(SessionId, SystemTime, TabId, PaneId), Status> {
        self.change(|model| {
            model.refuse_if_ended()?;
            model.refuse_if_taken(&name, None)?;
            refuse_if_full(model.sessions.len(), SESSIONS_MAX, "sessions")?;
            let session = SessionId::next(&mut model.last.session);
            let made = SystemTime::now();
            let tab = model.tab(self, (session, made), Name::numbered(1), opens)?;
            let ids = (session, made, tab.id, tab.pane.id);
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
            let (place, made) = {
                let session = model.session(session)?;
                (session.tabs.len() + 1, session.made)
            };
            let name = name.unwrap_or_else(|| Name::numbered(place));
            let tab = model.tab(self, (session, made), name, opens)?;
            let ids = (tab.id, tab.pane.id);
            model.session_mut(session)?.tabs.push(tab);
            Ok(ids)
        })
    }

    pub fn close_tab(&self, tab: TabId) -> Result<(), Status> {
        self.change(|model| {
            let (session, _) = model.tab_at(tab)?;
            model.close(session, tab);
            Ok(())
        })
    }

    pub fn kill(&self, session: SessionId) -> Result<(), Status> {
        self.change(|model| {
            model.session(session)?;
            model.end(session);
            Ok(())
        })
    }

    pub fn rename(&self, session: SessionId, name: Name) -> Result<(), Status> {
        self.change(|model| {
            model.session(session)?;
            model.refuse_if_taken(&name, Some(session))?;
            model.session_mut(session)?.name = name;
            Ok(())
        })
    }

    /// Takes the client `who` names to `to`, and says which it took.
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

    /// What reaches pane `pane`.
    pub fn pane(&self, pane: PaneId) -> Result<Handle, Status> {
        Ok(self.lock().pane_at(pane)?.1.pane.handle.clone())
    }

    pub fn listing(&self) -> api::Listing {
        self.lock().listing()
    }

    /// Takes a client in, as `hello` says, with `out` for what goes to it,
    /// and says its id. Its stream ends as `gone` drops, once it is let go of.
    pub fn attach(
        &self,
        hello: Hello,
        out: Out,
        gone: oneshot::Sender<()>,
    ) -> Result<ClientId, Status> {
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
            let seat = Seat { pane, size, colours, out, came: 0, _gone: gone };
            model.clients.insert(client, seat);
            if let Err(refusal) = model.seat_on(client, pane) {
                model.drop_client(client);
                return Err(refusal);
            }
            Ok(client)
        })
    }

    /// Lets go of a client that went.
    pub fn detach(&self, client: ClientId) {
        self.change(|model| model.drop_client(client));
    }

    /// What reaches the pane `client` is on, for its input, which `types`
    /// where it is typing. None where the client was let go of.
    pub fn input(&self, client: ClientId, types: bool) -> Option<Handle> {
        let mut model = self.lock();
        let pane = model.clients.get(&client)?.pane;
        let (session, tab) = model.pane_at(pane).expect("a client is on a pane of the model");
        let (session, handle) = (session.id, tab.pane.handle.clone());
        if types {
            model.session_mut(session).expect("a pane's session").last_typed = Some(client);
        }
        Some(handle)
    }

    /// Notes that `client` is now `size`, and gives back what reaches the
    /// pane it is on. None where the client was let go of.
    pub fn resize(&self, client: ClientId, size: Size) -> Option<Handle> {
        self.change(|model| {
            let seat = model.clients.get_mut(&client)?;
            seat.size = size;
            let pane = seat.pane;
            let (_, tab) = model.pane_at(pane).expect("a client is on a pane of the model");
            Some(tab.pane.handle.clone())
        })
    }

    /// Closes the tab of pane `pane`, whose thread failed, whatever it was
    /// opened to do as its program ends.
    pub fn lost(&self, pane: PaneId) {
        self.change(|model| {
            if let Ok((session, tab)) = model.pane_at(pane) {
                let (session, tab) = (session.id, tab.id);
                model.close(session, tab);
            }
        });
    }

    /// Notes what pane `pane`'s program last titled it, and where on this
    /// machine it last said it works, if it did.
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

    /// Notes what runs in front in pane `pane`, and what its shell said.
    pub fn looked(&self, pane: PaneId, foreground: Option<api::Foreground>, ran: Option<api::Ran>) {
        self.change(|model| {
            if let Some(Pane { program: Program::Runs { foreground: was, ran: said, .. }, .. }) =
                model.pane_mut(pane)
            {
                (*was, *said) = (foreground, ran);
            }
        });
    }

    /// Notes that pane `pane`'s program ended, as `exit` says, and says
    /// whether the pane went with it.
    pub fn ended(&self, pane: PaneId, exit: Exit) -> bool {
        self.change(|model| {
            let Ok((session, tab)) = model.pane_at(pane) else {
                return true;
            };
            let (session, tab, ends) = (session.id, tab.id, tab.ends);
            match ends {
                Ends::Closes => {
                    model.close(session, tab);
                    true
                },
                Ends::Holds => {
                    if let Some(pane) = model.pane_mut(pane) {
                        pane.program = Program::Ended(exit);
                    }
                    false
                },
            }
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

    fn refuse_if_taken(&self, name: &Name, renamed: Option<SessionId>) -> Result<(), Status> {
        let taken = self
            .sessions
            .iter()
            .any(|session| &session.name == name && Some(session.id) != renamed);
        if taken {
            return Err(Status::already_exists(format!("a session is named {}", name.as_str())));
        }
        Ok(())
    }

    /// A tab named `name` in session `session`, made at the time it says,
    /// whose pane runs what `opens` says. Its pseudoterminal is opened and its
    /// program started under the model's lock, so no other pane's program
    /// starts meanwhile and takes the pseudoterminal's end along before it is
    /// closed on exec.
    fn tab(
        &mut self,
        mux: &Arc<Mux>,
        (session, made): (SessionId, SystemTime),
        name: Name,
        opens: &Opens,
    ) -> Result<Tab, Status> {
        let tabs = self.sessions.iter().map(|session| session.tabs.len()).sum();
        refuse_if_full(tabs, TABS_MAX, "tabs")?;
        let made = made.duration_since(UNIX_EPOCH).ok();
        let made = made.and_then(|since| u64::try_from(since.as_nanos()).ok()).ok_or_else(|| {
            Status::failed_precondition("the clock says a time before 1970, or after 2554")
        })?;
        let (tab, pane) = (TabId::next(&mut self.last.tab), PaneId::next(&mut self.last.pane));
        let failed =
            |what: &str, error: std::io::Error| Status::internal(format!("{what}: {error}"));
        let pty =
            Pty::opened(FIRST_SIZE).map_err(|error| failed("no pseudoterminal opened", error))?;
        let vars = [
            (pty::SESSION_VAR, session.to_string()),
            (pty::MADE_VAR, made.to_string()),
            (pty::PANE_VAR, pane.to_string()),
        ];
        let child = pty.spawned(opens, &vars).map_err(|error| {
            Status::failed_precondition(format!("opens did not start: {error}"))
        })?;
        let started = pane::start(Arc::clone(mux), pane, pty, child, FIRST_SIZE)
            .map_err(|error| failed("the pane did not start", error))?;
        Ok(Tab {
            id: tab,
            name,
            ends: opens.ends,
            pane: Pane {
                id: pane,
                title: String::new(),
                cwd: opens.cwd.clone(),
                program: Program::Runs { pid: started.pid, foreground: None, ran: None },
                handle: started.handle,
            },
        })
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

    fn tab_at(&self, tab: TabId) -> Result<(SessionId, &Tab), Status> {
        self.sessions
            .iter()
            .find_map(|session| {
                session.tabs.iter().find(|found| found.id == tab).map(|found| (session.id, found))
            })
            .ok_or_else(|| Status::not_found(format!("no tab {tab}")))
    }

    fn pane_at(&self, pane: PaneId) -> Result<(&Session, &Tab), Status> {
        self.sessions
            .iter()
            .find_map(|session| {
                session.tabs.iter().find(|tab| tab.pane.id == pane).map(|tab| (session, tab))
            })
            .ok_or_else(|| Status::not_found(format!("no pane {pane}")))
    }

    fn pane_mut(&mut self, pane: PaneId) -> Option<&mut Pane> {
        self.sessions
            .iter_mut()
            .flat_map(|session| session.tabs.iter_mut())
            .map(|tab| &mut tab.pane)
            .find(|found| found.id == pane)
    }

    /// The pane a client going to `place` goes to, or why none: a place of a
    /// session made at another time is of a server before this one.
    fn pane_of_place(&self, place: Place) -> Result<PaneId, Status> {
        let (session, pane) = match place.is {
            Is::Tab(tab) => {
                let (session, tab) = self.tab_at(tab)?;
                (self.session(session).expect("a tab's session is the model's"), tab.pane.id)
            },
            Is::Session(session) => {
                let session = self.session(session)?;
                (session, session.tabs.current().pane.id)
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

    /// The client that last typed in `session`, or else the one that came to
    /// it last.
    fn last_typed_in(&self, session: SessionId) -> Result<ClientId, Status> {
        let found = self.session(session)?;
        let on = |client: &ClientId| {
            self.clients
                .get(client)
                .is_some_and(|seat| found.tabs.iter().any(|tab| tab.pane.id == seat.pane))
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

    /// Takes `client` to the pane of `to`.
    fn take(&mut self, client: ClientId, to: Place) -> Result<(), Status> {
        self.seat(client)?;
        let pane = self.pane_of_place(to)?;
        self.seat_on(client, pane)
    }

    /// Puts `client` on pane `pane`, and tells it so. The client stays where
    /// it was where the pane it leaves or the pane it goes to has no room in
    /// its queue, and a client that takes no more messages is let go of.
    fn seat_on(&mut self, client: ClientId, pane: PaneId) -> Result<(), Status> {
        let (session, tab) = self.pane_at(pane).expect("a client is seated on a pane of the model");
        let (session, tab, to) = (session.id, tab.id, tab.pane.handle.clone());
        let left = self.clients.get(&client).expect("a seated client is the model's").pane;
        let from = match self.pane_at(left) {
            Ok((_, tab)) if left != pane => Some(tab.pane.handle.clone()),
            _ => None,
        };
        let busy =
            |pane: PaneId| move |_: Busy| Status::unavailable(format!("pane {pane} is busy"));
        let unwatch = from.map(|from| from.reserve().map_err(busy(left))).transpose()?.flatten();
        let watch = to.reserve().map_err(busy(pane))?;
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
        let viewer =
            Viewer { client, out: seat.out.clone(), size: seat.size, colours: seat.colours };
        if let Some(unwatch) = unwatch {
            unwatch.send(ToPane::Unwatch(client));
        }
        if let Some(watch) = watch {
            watch.send(ToPane::Watch(viewer));
        }
        self.session_mut(session).expect("a pane's session").tabs.make_current(tab);
        Ok(())
    }

    /// Lets go of `client`, which ends its stream. Its pane is told, where it
    /// is still in the model and its queue has room, and else finds the
    /// stream ended at its next frame.
    fn drop_client(&mut self, client: ClientId) {
        let Some(seat) = self.clients.remove(&client) else {
            return;
        };
        if let Ok((_, tab)) = self.pane_at(seat.pane)
            && let Ok(Some(unwatch)) = tab.pane.handle.reserve()
        {
            unwatch.send(ToPane::Unwatch(client));
        }
    }

    /// Closes tab `tab` of session `session`, hangs up its pane, and takes the
    /// clients on it to the tab before it, or the one after. A session whose
    /// last tab it was ends.
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
        closed.pane.handle.close();
        let moved: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, seat)| seat.pane == closed.pane.id)
            .map(|(client, _)| *client)
            .collect();
        for client in moved {
            if self.seat_on(client, next_pane).is_err() {
                self.drop_client(client);
            }
        }
    }

    /// Ends session `session`: hangs up its panes, tells its clients and lets
    /// go of them. The server ends with its last session.
    fn end(&mut self, session: SessionId) {
        let Some(place) = self.sessions.iter().position(|found| found.id == session) else {
            return;
        };
        let ended = self.sessions.remove(place);
        let panes: Vec<PaneId> = ended.tabs.iter().map(|tab| tab.pane.id).collect();
        for tab in ended.tabs.iter() {
            tab.pane.handle.close();
        }
        let gone: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, seat)| panes.contains(&seat.pane))
            .map(|(client, _)| *client)
            .collect();
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
        let pane = &tab.pane;
        let mut clients: Vec<(ClientId, &Seat)> = self
            .clients
            .iter()
            .filter(|(_, seat)| seat.pane == pane.id)
            .map(|(client, seat)| (*client, seat))
            .collect();
        clients.sort_by_key(|(_, seat)| seat.came);
        let (program, foreground, ran) = match &pane.program {
            Program::Runs { pid, foreground, ran } => {
                (api::pane::Program::Pid(*pid), foreground.clone(), ran.clone())
            },
            Program::Ended(exit) => (api::pane::Program::Ended(exit.said()), None, None),
        };
        let cwd = pane.cwd.to_str().expect("a pane's cwd was said in UTF-8");
        let pane = api::Pane {
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
        };
        api::Tab { id: tab.id.said(), name: tab.name.as_str().to_owned(), panes: vec![pane] }
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

    /// Makes tab `tab`, which is one of them, the one a client coming goes to.
    fn make_current(&mut self, tab: TabId) {
        let place = self.tabs.iter().position(|found| found.id == tab);
        self.current = place.expect("a tab of the session is made current");
    }

    /// Takes tab `tab` out, where it is one of them and not the last. Its
    /// clients go to the tab before it, or else the one after, which is
    /// current where it was.
    fn close(&mut self, tab: TabId) -> Closed {
        let Some(place) = self.tabs.iter().position(|found| found.id == tab) else {
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
        Closed::Out { tab: Box::new(tab), next: self.tabs[next].pane.id }
    }
}

/// Refuses one more where `count` already reached `max` of `what`.
fn refuse_if_full(count: usize, max: usize, what: &str) -> Result<(), Status> {
    if count < max {
        return Ok(());
    }
    Err(Status::resource_exhausted(format!("luneta-mux holds {max} {what} at most")))
}
