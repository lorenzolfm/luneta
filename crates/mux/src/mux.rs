use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Ends, Greeting, Opens, SHOWN_MAX, Size, Tag, TerminalKey};
use crate::id::{ClientId, TerminalId};
use crate::pane::{self, Exit, Handle, Viewer, ViewerOut, Wanted};
use crate::pty::{self, Pty};

pub const TERMINALS_MAX: usize = 1024;

pub const CLIENTS_MAX: usize = 256;

const _: () = assert!(SHOWN_MAX <= TERMINALS_MAX);

pub struct Mux {
    model: Mutex<Model>,
    listing: watch::Sender<Option<api::Listing>>,
}

#[derive(Default)]
struct Model {
    last: Ids,
    terminals: Vec<Terminal>,
    viewing: BTreeMap<ClientId, Viewing>,
    ended: bool,
}

#[derive(Default)]
struct Ids {
    terminal: Option<TerminalId>,
    client: Option<ClientId>,
    came: u64,
}

struct Terminal {
    key: TerminalKey,
    tag: Tag,
    size: Size,
    title: String,
    cwd: String,
    program: Program,
    handle: Handle,
}

enum Program {
    Runs { pid: u32, ends: Ends, foreground: Option<api::Foreground>, ran: Option<api::Ran> },
    Ended(Exit),
}

struct Viewing {
    out: ViewerOut,
    colours: Colours,
    runs_in: Option<TerminalKey>,
    shows: BTreeMap<TerminalKey, u64>,
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
        self.change(Model::end_if_empty);
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.model.lock().expect("a thread panicked while it held the model")
    }

    fn change<T>(&self, change: impl FnOnce(&mut Model) -> T) -> T {
        let mut model = self.lock();
        let changed = change(&mut model);
        if model.ended {
            assert!(model.terminals.is_empty(), "the server ended with a terminal");
            assert!(model.viewing.is_empty(), "the server ended with a viewer");
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

    pub fn new_terminal(
        self: &Arc<Mux>,
        opens: &Opens,
        tag: Tag,
        size: Size,
    ) -> Result<TerminalKey, Status> {
        self.change(|model| {
            model.refuse_if_ended()?;
            refuse_if_full(model.terminals.len(), TERMINALS_MAX, "terminals")?;
            let id = TerminalId::next(&mut model.last.terminal);
            assert!(model.place(id).is_none(), "terminal {id} is new");
            let key = TerminalKey { id, made: SystemTime::now() };
            let terminal = spawned(self, key, tag, opens, size)?;
            model.terminals.push(terminal);
            Ok(key)
        })
    }

    pub fn close_terminal(&self, key: TerminalKey) -> Result<(), Status> {
        self.change(|model| {
            model.terminal(key)?;
            model.close(key.id);
            Ok(())
        })
    }

    pub fn size_terminal(&self, key: TerminalKey, size: Size) -> Result<(), Status> {
        self.change(|model| {
            model.terminal_mut(key)?.size = size;
            Ok(())
        })
    }

    pub fn tag_terminal(&self, key: TerminalKey, tag: Tag) -> Result<(), Status> {
        self.change(|model| {
            model.terminal_mut(key)?.tag = tag;
            Ok(())
        })
    }

    pub fn terminal(&self, key: TerminalKey) -> Result<Handle, Status> {
        Ok(self.lock().terminal(key)?.handle.clone())
    }

    pub fn view(
        &self,
        greeting: Greeting,
        out: ViewerOut,
        gone: oneshot::Sender<()>,
    ) -> Result<ClientId, Status> {
        self.change(|model| {
            model.refuse_if_ended()?;
            refuse_if_full(model.viewing.len(), CLIENTS_MAX, "clients")?;
            let client = ClientId::next(&mut model.last.client);
            let Greeting { colours, runs_in } = greeting;
            let viewing = Viewing { out, colours, runs_in, shows: BTreeMap::new(), _gone: gone };
            let was = model.viewing.insert(client, viewing);
            assert!(was.is_none(), "viewer {client} is new");
            Ok(client)
        })
    }

    pub fn show(
        &self,
        client: ClientId,
        keys: &BTreeSet<TerminalKey>,
    ) -> Result<Vec<Status>, Status> {
        self.change(|model| model.show(client, keys))
    }

    pub fn typed(&self, client: ClientId, terminal: TerminalKey) -> Result<Handle, Status> {
        let model = self.lock();
        let viewing = model.viewing.get(&client).ok_or_else(|| no_client(client))?;
        if !viewing.shows.contains_key(&terminal) {
            let not_shown = format!("terminal {} is not shown", terminal.id);
            return Err(Status::failed_precondition(not_shown));
        }
        let terminal = model.terminal(terminal).expect("a shown terminal is the model's");
        Ok(terminal.handle.clone())
    }

    pub fn unview(&self, client: ClientId) {
        self.change(|model| {
            model.viewing.remove(&client);
        });
    }

    pub fn listing(&self) -> api::Listing {
        self.lock().listing()
    }

    pub fn lost(&self, terminal: TerminalId) {
        self.change(|model| model.close(terminal));
    }

    pub fn titled(&self, terminal: TerminalId, title: String, cwd: Option<String>) {
        self.change(|model| {
            if let Some(found) = model.of_id_mut(terminal) {
                found.title = title;
                if let Some(cwd) = cwd {
                    found.cwd = cwd;
                }
            }
        });
    }

    pub fn looked(
        &self,
        terminal: TerminalId,
        foreground: Option<api::Foreground>,
        ran: Option<api::Ran>,
    ) {
        self.change(|model| {
            let found = model.of_id_mut(terminal).map(|found| &mut found.program);
            if let Some(Program::Runs { foreground: was, ran: said, .. }) = found {
                (*was, *said) = (foreground, ran);
            }
        });
    }

    pub fn ended(&self, terminal: TerminalId, exit: Exit) -> bool {
        self.change(|model| {
            let Some(found) = model.of_id_mut(terminal) else {
                return true;
            };
            if let Program::Runs { ends: Ends::Closes, .. } = found.program {
                model.close(terminal);
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
            return Err(Status::unavailable("luneta-mux is ending, as nothing is left in it"));
        }
        Ok(())
    }

    fn end_if_empty(&mut self) {
        if self.terminals.is_empty() {
            self.ended = true;
            self.viewing.clear();
        }
    }

    fn place(&self, terminal: TerminalId) -> Option<usize> {
        self.terminals.iter().position(|found| found.key.id == terminal)
    }

    fn of_id_mut(&mut self, terminal: TerminalId) -> Option<&mut Terminal> {
        self.terminals.iter_mut().find(|found| found.key.id == terminal)
    }

    fn terminal(&self, key: TerminalKey) -> Result<&Terminal, Status> {
        self.terminals
            .iter()
            .find(|found| found.key == key)
            .ok_or_else(|| Status::not_found(format!("no terminal {} made then", key.id)))
    }

    fn terminal_mut(&mut self, key: TerminalKey) -> Result<&mut Terminal, Status> {
        self.terminals
            .iter_mut()
            .find(|found| found.key == key)
            .ok_or_else(|| Status::not_found(format!("no terminal {} made then", key.id)))
    }

    fn show(
        &mut self,
        client: ClientId,
        keys: &BTreeSet<TerminalKey>,
    ) -> Result<Vec<Status>, Status> {
        let viewing = self.viewing.get(&client).ok_or_else(|| no_client(client))?;
        if let Some(runs_in) = viewing.runs_in
            && keys.contains(&runs_in)
        {
            return Err(Status::failed_precondition(format!(
                "the viewer runs in terminal {}, and never shows itself",
                runs_in.id
            )));
        }
        assert!(keys.len() <= SHOWN_MAX);
        let mut now = BTreeMap::new();
        let mut gone = Vec::new();
        for key in keys {
            if let Err(refusal) = self.terminal(*key) {
                gone.push(refusal);
                continue;
            }
            let came = viewing.shows.get(key).copied().unwrap_or_else(|| {
                self.last.came += 1;
                self.last.came
            });
            now.insert(*key, came);
        }
        assert_eq!(now.len() + gone.len(), keys.len());
        self.viewing.get_mut(&client).expect("a viewer found above").shows = now;
        Ok(gone)
    }

    fn close(&mut self, terminal: TerminalId) {
        let Some(place) = self.place(terminal) else {
            return;
        };
        let closed = self.terminals.remove(place);
        closed.handle.close();
        for viewing in self.viewing.values_mut() {
            viewing.shows.remove(&closed.key);
        }
        self.end_if_empty();
    }

    fn settle(&mut self) {
        assert!(self.viewing.len() <= CLIENTS_MAX);
        assert!(self.terminals.len() <= TERMINALS_MAX);
        let mut by_terminal: BTreeMap<TerminalKey, Vec<Viewer>> = BTreeMap::new();
        for (client, viewing) in &self.viewing {
            assert!(viewing.shows.len() <= SHOWN_MAX);
            if let Some(runs_in) = viewing.runs_in {
                let shown = viewing.shows.contains_key(&runs_in);
                assert!(!shown, "viewer {client} shows terminal {}, which it runs in", runs_in.id);
            }
            for (key, came) in &viewing.shows {
                let frames = viewing.out.clone();
                let colours = viewing.colours;
                let viewer = Viewer { client: *client, frames, colours, came: *came };
                by_terminal.entry(*key).or_default().push(viewer);
            }
        }
        for terminal in &self.terminals {
            let viewers = by_terminal.remove(&terminal.key).unwrap_or_default();
            terminal.handle.want(Wanted { size: terminal.size, viewers: viewers.into() });
        }
        assert!(by_terminal.is_empty(), "every shown terminal is the model's");
    }

    fn listing(&self) -> api::Listing {
        api::Listing { terminals: self.terminals.iter().map(Terminal::said).collect() }
    }
}

impl Terminal {
    fn said(&self) -> api::Terminal {
        let program = match &self.program {
            Program::Runs { pid, foreground, ran, .. } => {
                let (foreground, ran) = (foreground.clone(), ran.clone());
                api::terminal::Program::Runs(api::Running { pid: *pid, foreground, ran })
            },
            Program::Ended(exit) => api::terminal::Program::Ended(exit.said()),
        };
        api::Terminal {
            key: Some(self.key.said()),
            tag: self.tag.said(),
            size: Some(self.size.said()),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            program: Some(program),
        }
    }
}

fn nanos(made: SystemTime) -> Result<u64, Status> {
    let made = made.duration_since(UNIX_EPOCH).ok();
    made.and_then(|since| u64::try_from(since.as_nanos()).ok()).ok_or_else(|| {
        Status::failed_precondition("the clock says a time before 1970, or after 2554")
    })
}

fn spawned(
    mux: &Arc<Mux>,
    key: TerminalKey,
    tag: Tag,
    opens: &Opens,
    size: Size,
) -> Result<Terminal, Status> {
    let vars = [
        (pty::TERMINAL_VAR, key.id.to_string()),
        (pty::TERMINAL_MADE_VAR, nanos(key.made)?.to_string()),
    ];
    let failed =
        |what: &str, error: std::io::Error| Status::internal(format!("{what}: {error}"));
    let pty = Pty::opened(size).map_err(|error| failed("no pseudoterminal opened", error))?;
    let child = pty.spawned(opens, &vars).map_err(|error| {
        Status::failed_precondition(format!("opens did not start: {error}"))
    })?;
    let started = pane::start(Arc::clone(mux), key.id, pty, child, size)
        .map_err(|error| failed("the terminal did not start", error))?;
    Ok(Terminal {
        key,
        tag,
        size,
        title: String::new(),
        cwd: opens.cwd.clone(),
        program: Program::Runs { pid: started.pid, ends: opens.ends, foreground: None, ran: None },
        handle: started.handle,
    })
}

fn no_client(client: ClientId) -> Status {
    Status::not_found(format!("no client {client}"))
}

fn refuse_if_full(count: usize, max: usize, what: &str) -> Result<(), Status> {
    if count < max {
        return Ok(());
    }
    Err(Status::resource_exhausted(format!("luneta-mux holds {max} {what} at most")))
}
