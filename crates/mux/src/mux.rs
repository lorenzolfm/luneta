use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lune_protocol::socket::Unreached;
use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Ends, Greeting, Opens, SHOWN_MAX, Size, Tag, TerminalKey};
use crate::id::{ClientId, TerminalId};
use crate::panics::Shared;
use crate::terminal::{self, Exit, Handle, Placing, Viewer, ViewerOut, Wanted};
use crate::pty::{self, Pty};

pub const TERMINALS_MAX: usize = 1024;

pub const CLIENTS_MAX: usize = 256;

const _: () = assert!(SHOWN_MAX <= TERMINALS_MAX);

pub struct Mux {
    model: Shared<Model>,
    listing: watch::Sender<Listed>,
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
    starting: BTreeSet<TerminalId>,
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
        Arc::new(Mux { model: Shared::new(Model::Holds(Holding::default())), listing })
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

    fn change<T>(&self, change: impl FnOnce(&mut Model) -> T) -> T {
        let mut model = self.model.lock();
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
        let key = self.change(Model::reserve)?;
        let spawned = spawned(self, key, tag, opens, size);
        self.change(|model| model.place(key, spawned))
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
        Ok(self.model.lock().terminal(key)?.handle.clone())
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
        let model = self.model.lock();
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
        match &*self.model.lock() {
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
                Err(Status::unavailable("it is ending, as nothing is left in it"))
            },
            Model::Ended(Ending::Unreached(unreached)) => {
                Err(Status::unavailable(format!("it is ending: {unreached}")))
            },
        }
    }

    fn reserve(&mut self) -> Result<TerminalKey, Status> {
        let holding = self.holding()?;
        let held = holding.terminals.len() + holding.starting.len();
        refuse_if_full(held, TERMINALS_MAX, "terminals")?;
        let id = TerminalId::next(&mut holding.last.terminal);
        assert!(holding.index_of(id).is_none(), "terminal {id} is new");
        let reserved = holding.starting.insert(id);
        assert!(reserved, "terminal {id} is new");
        Ok(TerminalKey { id, made: SystemTime::now() })
    }

    fn place(
        &mut self,
        key: TerminalKey,
        spawned: Result<(Terminal, Placing), Status>,
    ) -> Result<TerminalKey, Status> {
        let holding = self.holding()?;
        let reserved = holding.starting.remove(&key.id);
        assert!(reserved, "terminal {} was reserved", key.id);
        let (terminal, placing) = match spawned {
            Ok(spawned) => spawned,
            Err(refusal) => {
                self.end_if_empty();
                return Err(refusal);
            },
        };
        assert_eq!(terminal.key, key);
        holding.terminals.push(terminal);
        placing.placed();
        Ok(key)
    }

    fn end_if_empty(&mut self) {
        if let Model::Holds(holding) = self
            && holding.terminals.is_empty()
            && holding.starting.is_empty()
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
        assert!(self.terminals.len() + self.starting.len() <= TERMINALS_MAX);
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
            let id = terminal.key.id;
            assert!(!self.starting.contains(&id), "terminal {id} is placed, so it starts no more");
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
) -> Result<(Terminal, Placing), Status> {
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
    let terminal = Terminal {
        key,
        tag,
        size,
        title: String::new(),
        cwd: opens.cwd.clone(),
        program: Program::Runs { pid: started.pid, ends: opens.ends, foreground: None, ran: None },
        handle: started.handle,
    };
    Ok((terminal, started.placing))
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
    Err(Status::resource_exhausted(format!("it holds {max} {what} at most")))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::num::NonZeroU16;

    use lune_protocol::socket::{self, Found, Unreached};
    use lune_waits::eventually;
    use tonic::{Code, Status};

    use super::{Ending, Listed, Model, Mux, spawned};
    use crate::asked::{Ends, Opens, Size, Tag};

    pub(crate) fn panic_holding_the_model() {
        Mux::new().change(|_| panic!("the model's assertion fails"));
    }

    fn opens() -> Opens {
        script("read x", &[])
    }

    fn script(script: &str, args: &[&str]) -> Opens {
        let program = "/bin/sh".to_owned();
        let args = ["-c", script].iter().chain(args).map(|arg| (*arg).to_owned()).collect();
        Opens { cwd: "/".to_owned(), program, args, ends: Ends::Holds }
    }

    fn size() -> Size {
        Size { columns: NonZeroU16::new(20).unwrap(), rows: NonZeroU16::new(4).unwrap() }
    }

    fn tag() -> Tag {
        Tag::parse("tag", b"t".to_vec()).unwrap()
    }

    fn listed(mux: &Mux) -> Vec<u32> {
        let listing = mux.listing().terminals.into_iter();
        listing.map(|terminal| terminal.key.unwrap().id).collect()
    }

    fn unreached() -> Unreached {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("socket");
        let (_listener, bound) = socket::bind(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let Found::Lost(unreached) = bound.found().unwrap() else {
            panic!("a removed socket is lost");
        };
        unreached
    }

    #[test]
    fn a_terminal_that_starts_keeps_the_mux_from_ending_as_empty() {
        let mux = Mux::new();
        let key = mux.change(Model::reserve).unwrap();
        mux.end_if_unused();
        let started = spawned(&mux, key, tag(), &opens(), size());
        assert_eq!(mux.change(|model| model.place(key, started)).unwrap(), key);
        assert_eq!(listed(&mux), [key.id.said()]);
        mux.close_terminal(key).unwrap();
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Emptied));
    }

    #[test]
    fn a_terminal_that_fails_to_start_gives_its_place_back() {
        let mux = Mux::new();
        let key = mux.change(Model::reserve).unwrap();
        let failed = Err(Status::failed_precondition("opens did not start"));
        let refusal = mux.change(|model| model.place(key, failed)).unwrap_err();
        let refused = (Code::FailedPrecondition, "opens did not start");
        assert_eq!((refusal.code(), refusal.message()), refused);
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Emptied));
    }

    #[test]
    fn a_terminal_that_starts_as_the_mux_ends_is_not_placed() {
        let mux = Mux::new();
        let key = mux.change(Model::reserve).unwrap();
        let started = spawned(&mux, key, tag(), &opens(), size());
        let unreached = unreached();
        assert_eq!(mux.end_unreached(unreached.clone()), Some(0));
        let refusal = mux.change(|model| model.place(key, started)).unwrap_err();
        assert_eq!(refusal.code(), Code::Unavailable);
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Unreached(unreached)));
        assert_eq!(listed(&mux), [] as [u32; 0]);
    }

    #[test]
    fn what_a_terminal_says_before_it_is_placed_is_not_lost() {
        let folder = tempfile::tempdir().unwrap();
        let said = folder.path().join("said");
        let mux = Mux::new();
        let key = mux.change(Model::reserve).unwrap();
        let titles = r#"printf '\033]2;early\007'; : > "$0"; read x"#;
        let titles = script(titles, &[said.to_str().unwrap()]);
        let started = spawned(&mux, key, tag(), &titles, size());
        eventually("the program said its title", || said.exists().then_some(()));
        mux.change(|model| model.place(key, started)).unwrap();
        eventually("the title is listed", || {
            let listing = mux.listing();
            listing.terminals.iter().any(|terminal| terminal.title == "early").then_some(())
        });
    }
}
