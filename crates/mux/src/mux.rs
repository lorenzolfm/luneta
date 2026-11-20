use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Ends, Greeting, Opens, SHOWN_MAX, Size, Tag, TerminalKey};
use crate::folder::Unreached;
use crate::id::{ClientId, TerminalId};
use crate::panics::{self, Uncontained};
use crate::terminal::{self, Exit, Handle, Viewer, ViewerOut, Wanted};
use crate::pty::{self, Pty};

pub const TERMINALS_MAX: usize = 1024;

pub const CLIENTS_MAX: usize = 256;

const _: () = assert!(SHOWN_MAX <= TERMINALS_MAX);

pub struct Mux {
    model: Mutex<Model>,
    listing: watch::Sender<Listed>,
}

struct Held<'a> {
    model: MutexGuard<'a, Model>,
    _uncontained: Uncontained,
}

impl Deref for Held<'_> {
    type Target = Model;

    fn deref(&self) -> &Model {
        &self.model
    }
}

impl DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut Model {
        &mut self.model
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Listed {
    Holds(api::Listing),
    Ended(Ending),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    Emptied,
    Unreached(Unreached),
}

impl Ending {
    #[must_use]
    pub fn said(&self) -> api::watched::Ended {
        let why = match self {
            Ending::Emptied => api::watched::ended::Why::Emptied(api::watched::Emptied {}),
            Ending::Unreached(unreached) => {
                let why = unreached.to_string();
                api::watched::ended::Why::Unreached(api::watched::Unreached { why })
            },
        };
        api::watched::Ended { why: Some(why) }
    }
}

enum Model {
    Holds(Holding),
    Ended(Ending),
}

#[derive(Default)]
struct Holding {
    last: Ids,
    terminals: Vec<Terminal>,
    viewing: BTreeMap<ClientId, Viewing>,
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
        let (listing, _) = watch::channel(Listed::Holds(api::Listing::default()));
        Arc::new(Mux { model: Mutex::new(Model::Holds(Holding::default())), listing })
    }

    pub fn watched(&self) -> watch::Receiver<Listed> {
        self.listing.subscribe()
    }

    pub fn end_if_unused(&self) {
        self.change(Model::end_if_empty);
    }

    pub fn end_unreached(&self, unreached: Unreached) -> Option<usize> {
        self.change(|model| model.end_unreached(unreached))
    }

    fn lock(&self) -> Held<'_> {
        let uncontained = panics::uncontained();
        let model = self.model.lock().expect("a thread panicked while it held the model");
        Held { model, _uncontained: uncontained }
    }

    fn change<T>(&self, change: impl FnOnce(&mut Model) -> T) -> T {
        let mut model = self.lock();
        let changed = change(&mut model);
        let listed = match &mut *model {
            Model::Holds(holding) => {
                holding.settle();
                Listed::Holds(holding.listing())
            },
            Model::Ended(ending) => Listed::Ended(ending.clone()),
        };
        self.listing.send_if_modified(|said| {
            let modified = *said != listed;
            *said = listed;
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
            let holding = model.holding()?;
            refuse_if_full(holding.terminals.len(), TERMINALS_MAX, "terminals")?;
            let id = TerminalId::next(&mut holding.last.terminal);
            assert!(holding.index_of(id).is_none(), "terminal {id} is new");
            let key = TerminalKey { id, made: SystemTime::now() };
            let terminal = spawned(self, key, tag, opens, size)?;
            holding.terminals.push(terminal);
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
            let holding = model.holding()?;
            refuse_if_full(holding.viewing.len(), CLIENTS_MAX, "clients")?;
            let client = ClientId::next(&mut holding.last.client);
            let Greeting { colours, runs_in } = greeting;
            let viewing = Viewing { out, colours, runs_in, shows: BTreeMap::new(), _gone: gone };
            let was = holding.viewing.insert(client, viewing);
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
        let Model::Holds(holding) = &*model else {
            return Err(no_client(client));
        };
        let viewing = holding.viewing.get(&client).ok_or_else(|| no_client(client))?;
        if !viewing.shows.contains_key(&terminal) {
            let not_shown = format!("terminal {} is not shown", terminal.id);
            return Err(Status::failed_precondition(not_shown));
        }
        let terminal = holding.terminal(terminal).expect("a shown terminal is the model's");
        Ok(terminal.handle.clone())
    }

    pub fn unview(&self, client: ClientId) {
        self.change(|model| {
            if let Model::Holds(holding) = model {
                holding.viewing.remove(&client);
            }
        });
    }

    pub fn listing(&self) -> api::Listing {
        match &*self.lock() {
            Model::Holds(holding) => holding.listing(),
            Model::Ended(_) => api::Listing::default(),
        }
    }

    pub fn lost(&self, terminal: TerminalId) {
        self.change(|model| model.close(terminal));
    }

    pub fn titled(&self, terminal: TerminalId, title: String, cwd: Option<String>) {
        self.change(|model| {
            if let Some(found) = model.terminal_of_id_mut(terminal) {
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
            let found = model.terminal_of_id_mut(terminal).map(|found| &mut found.program);
            if let Some(Program::Runs { foreground: was, ran: said, .. }) = found {
                (*was, *said) = (foreground, ran);
            }
        });
    }

    pub fn ended(&self, terminal: TerminalId, exit: Exit) -> bool {
        self.change(|model| {
            let Some(found) = model.terminal_of_id_mut(terminal) else {
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
    fn holding(&mut self) -> Result<&mut Holding, Status> {
        match self {
            Model::Holds(holding) => Ok(holding),
            Model::Ended(Ending::Emptied) => {
                Err(Status::unavailable("luneta-mux is ending, as nothing is left in it"))
            },
            Model::Ended(Ending::Unreached(unreached)) => {
                Err(Status::unavailable(format!("luneta-mux is ending: {unreached}")))
            },
        }
    }

    fn end_if_empty(&mut self) {
        if let Model::Holds(holding) = self
            && holding.terminals.is_empty()
        {
            *self = Model::Ended(Ending::Emptied);
        }
    }

    fn end_unreached(&mut self, unreached: Unreached) -> Option<usize> {
        let Model::Holds(holding) = self else {
            return None;
        };
        let closed = holding.terminals.len();
        for terminal in holding.terminals.drain(..) {
            terminal.handle.close();
        }
        *self = Model::Ended(Ending::Unreached(unreached));
        Some(closed)
    }

    fn close(&mut self, terminal: TerminalId) {
        if let Model::Holds(holding) = self
            && holding.close(terminal)
        {
            self.end_if_empty();
        }
    }

    fn terminal_of_id_mut(&mut self, terminal: TerminalId) -> Option<&mut Terminal> {
        let Model::Holds(holding) = self else {
            return None;
        };
        holding.terminals.iter_mut().find(|found| found.key.id == terminal)
    }

    fn terminal(&self, key: TerminalKey) -> Result<&Terminal, Status> {
        let Model::Holds(holding) = self else {
            return Err(unmade(key));
        };
        holding.terminal(key)
    }

    fn terminal_mut(&mut self, key: TerminalKey) -> Result<&mut Terminal, Status> {
        let Model::Holds(holding) = self else {
            return Err(unmade(key));
        };
        holding.terminals.iter_mut().find(|found| found.key == key).ok_or_else(|| unmade(key))
    }

    fn show(
        &mut self,
        client: ClientId,
        keys: &BTreeSet<TerminalKey>,
    ) -> Result<Vec<Status>, Status> {
        let Model::Holds(holding) = self else {
            return Err(no_client(client));
        };
        holding.show(client, keys)
    }
}

impl Holding {
    fn index_of(&self, terminal: TerminalId) -> Option<usize> {
        self.terminals.iter().position(|found| found.key.id == terminal)
    }

    fn terminal(&self, key: TerminalKey) -> Result<&Terminal, Status> {
        self.terminals.iter().find(|found| found.key == key).ok_or_else(|| unmade(key))
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

    fn close(&mut self, terminal: TerminalId) -> bool {
        let Some(index) = self.index_of(terminal) else {
            return false;
        };
        let closed = self.terminals.remove(index);
        closed.handle.close();
        for viewing in self.viewing.values_mut() {
            viewing.shows.remove(&closed.key);
        }
        true
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
    let started = terminal::start(Arc::clone(mux), key, pty, child, size)
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

fn unmade(key: TerminalKey) -> Status {
    Status::not_found(format!("no terminal {} made then", key.id))
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

#[cfg(test)]
pub(crate) mod tests {
    use super::Mux;

    pub(crate) fn panic_holding_the_model() {
        Mux::new().change(|_| panic!("the model's assertion fails"));
    }
}
