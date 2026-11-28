use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use lune_protocol::program_resume::{TOKEN_VAR, TerminalResumes};
use lune_protocol::shell::Doing;
use lune_protocol::socket::Unreached;
use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{
    AbsoluteCwd, Ends, Greeting, Launch, Opens, SHOWN_MAX, Size, SpawnSpec, Tag, TerminalKey,
};
use crate::id::{ClientId, TerminalId};
use crate::leader::Leaders;
use crate::looker::Foreground;
use crate::panics::Shared;
use crate::program_resume::ResumeToken;
use crate::program_status::{EndedProgramStatuses, Snapshot as ProgramStatuses};
use crate::pty::{self, Pty};
use crate::resume_exec;
use crate::terminal::{self, Exit, Handle, Placing, ViewerOut};

use model::{MetaMut, Model, Terminal, Terminals, Viewers};

pub const TERMINALS_MAX: usize = lune_protocol::MUX_TERMINALS_MAX;

pub const CLIENTS_MAX: usize = 256;

const _: () = assert!(SHOWN_MAX <= TERMINALS_MAX);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaUpdate {
    Title(String),
    Cwd(AbsoluteCwd),
    Both { title: String, cwd: AbsoluteCwd },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinalMeta {
    pub title: Option<String>,
    pub cwd: Option<AbsoluteCwd>,
}

pub struct Mux {
    model: Shared<Model>,
    listing: watch::Sender<Listed>,
    leaders: Leaders,
    runtime_dir: Option<PathBuf>,
    browser_socket: Option<PathBuf>,
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
            }
        };
        api::watched::Ended { why: Some(why) }
    }
}

#[must_use]
pub struct Reserved {
    mux: Arc<Mux>,
    key: TerminalKey,
}

impl Reserved {
    pub fn start(self, opens: &Opens, tag: Tag, size: Size) -> Result<TerminalKey, Status> {
        let started = spawned(&self.mux, self.key, tag, opens, size);
        self.place(started)
    }

    fn place(self, started: Result<(Terminal, Placing), Status>) -> Result<TerminalKey, Status> {
        self.mux.change(|model| model.place(self.key, started))
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        self.mux.change(|model| model.unreserve(self.key.id));
    }
}

impl Mux {
    #[cfg(test)]
    #[must_use]
    pub fn new() -> Arc<Mux> {
        Mux::serving(None, None)
    }

    #[must_use]
    pub fn serving(runtime_dir: Option<PathBuf>, browser_socket: Option<PathBuf>) -> Arc<Mux> {
        let (listing, _) = watch::channel(Listed::Holds(api::Listing::default()));
        let model = Shared::new(Model::new());
        Arc::new(Mux {
            model,
            listing,
            leaders: Leaders::new(),
            runtime_dir,
            browser_socket,
        })
    }

    pub fn watched(&self) -> watch::Receiver<Listed> {
        self.listing.subscribe()
    }

    pub fn leaders(&self) -> &Leaders {
        &self.leaders
    }

    pub fn end_if_unused(&self) {
        self.change(Model::end_if_unused);
    }

    pub fn end_unreached(&self, unreached: Unreached) -> Option<usize> {
        self.change(|model| model.end_unreached(unreached))
    }

    fn change<T>(&self, change: impl FnOnce(&mut Model) -> T) -> T {
        let mut model = self.model.lock();
        let changed = change(&mut model);
        model.settle();
        self.publish(&model);
        changed
    }

    fn change_viewers<T>(
        &self,
        when_ended: impl FnOnce(&Ending) -> T,
        change: impl FnOnce(&Terminals, &mut Viewers) -> T,
    ) -> T {
        let mut model = self.model.lock();
        model.change_viewers(when_ended, change)
    }

    fn change_terminal_meta<E>(
        &self,
        lookup: impl FnOnce(&mut Model) -> Result<&mut Terminal, E>,
        change: impl FnOnce(&mut MetaMut<'_>),
    ) -> Result<(), E> {
        let mut model = self.model.lock();
        let terminal = lookup(&mut model)?;
        let modified = model::mutate_meta(terminal, change);
        if modified {
            self.publish(&model);
        }
        Ok(())
    }

    fn publish(&self, model: &Model) {
        let listed = model.listed();
        self.listing.send_if_modified(|said| {
            let modified = *said != listed;
            *said = listed;
            modified
        });
    }

    pub fn reserve_terminal(self: &Arc<Mux>) -> Result<Reserved, Status> {
        let key = self.change(Model::reserve)?;
        Ok(Reserved {
            mux: Arc::clone(self),
            key,
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
        let mut model = self.model.lock();
        if model.terminal_mut(key)?.set_size(size) {
            model.settle();
            self.publish(&model);
        }
        Ok(())
    }

    pub fn tag_terminal(&self, key: TerminalKey, tag: Tag) -> Result<(), Status> {
        self.change_terminal_meta(|model| model.terminal_mut(key), |meta| meta.set_tag(tag))
    }

    pub fn terminal(&self, key: TerminalKey) -> Result<Handle, Status> {
        Ok(self.model.lock().terminal(key)?.handle().clone())
    }

    pub fn view(
        &self,
        greeting: Greeting,
        out: ViewerOut,
        gone: oneshot::Sender<()>,
    ) -> Result<ClientId, Status> {
        self.change_viewers(
            |ending| Err(model::ending_status(ending)),
            |_, viewers| viewers.view(greeting, out, gone),
        )
    }

    pub fn show(
        &self,
        client: ClientId,
        keys: &BTreeSet<TerminalKey>,
    ) -> Result<Vec<Status>, Status> {
        self.change_viewers(
            |_| Err(model::no_client(client)),
            |terminals, viewers| viewers.show(terminals, client, keys),
        )
    }

    pub fn typed(&self, client: ClientId, terminal: TerminalKey) -> Result<Handle, Status> {
        self.model.lock().typed(client, terminal)
    }

    pub fn unview(&self, client: ClientId) {
        self.change_viewers(|_| {}, |_, viewers| viewers.unview(client));
    }

    pub fn listing(&self) -> api::Listing {
        self.model.lock().listing()
    }

    pub fn lost(&self, terminal: TerminalId) {
        self.change(|model| model.close(terminal));
    }

    pub fn titled(&self, terminal: TerminalId, update: MetaUpdate) {
        let _ = self.change_terminal_meta(
            |model| model.terminal_of_id_mut(terminal).ok_or(()),
            |meta| match update {
                MetaUpdate::Title(title) => meta.set_title(title),
                MetaUpdate::Cwd(cwd) => meta.set_cwd(cwd),
                MetaUpdate::Both { title, cwd } => {
                    meta.set_title(title);
                    meta.set_cwd(cwd);
                }
            },
        );
    }

    pub fn looked(&self, terminal: TerminalId, foreground: Option<Foreground>, ran: Option<Doing>) {
        let _ = self.change_terminal_meta(
            |model| model.terminal_of_id_mut(terminal).ok_or(()),
            |meta| meta.set_looked(foreground, ran),
        );
    }

    pub fn program_statuses(&self, terminal: TerminalId, statuses: ProgramStatuses) {
        assert!(statuses.children.len() <= crate::program_status::RECORDS_MAX);
        let _ = self.change_terminal_meta(
            |model| model.terminal_of_id_mut(terminal).ok_or(()),
            |meta| meta.set_program_statuses(statuses),
        );
    }

    pub fn program_resumes(&self, terminal: TerminalId, resumes: Option<TerminalResumes>) {
        let _ = self.change_terminal_meta(
            |model| model.terminal_of_id_mut(terminal).ok_or(()),
            |meta| meta.set_program_resumes(resumes),
        );
    }

    pub fn ended(
        &self,
        terminal: TerminalId,
        exit: Exit,
        statuses: EndedProgramStatuses,
        final_meta: FinalMeta,
    ) -> bool {
        assert!(statuses.children.len() <= crate::program_status::RECORDS_MAX);
        let mut model = self.model.lock();
        let (closed, changed) = model.ended(terminal, exit, statuses, final_meta);
        if changed {
            model.settle();
            self.publish(&model);
        }
        closed
    }
}

mod model {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::SystemTime;

    use lune_protocol::made::Made;
    use lune_protocol::program_resume::TerminalResumes;
    use lune_protocol::program_status::bound_fair;
    use lune_protocol::shell::Doing;
    use lune_protocol::socket::Unreached;
    use lune_protocol::{LISTING_CHILDREN_BYTES_MAX, LISTING_RESUMES_BYTES_MAX};
    use prost::Message as _;
    use tokio::sync::mpsc::error::TrySendError;
    use tokio::sync::oneshot;
    use tonic::Status;

    use super::{CLIENTS_MAX, Ending, FinalMeta, Listed, TERMINALS_MAX};
    use crate::api;
    use crate::asked::{AbsoluteCwd, Colours, Ends, Greeting, SHOWN_MAX, Size, Tag, TerminalKey};
    use crate::id::{ClientId, TerminalId};
    use crate::looker::Foreground;
    use crate::process::ProcessId;
    use crate::program_status::{EndedProgramStatuses, Snapshot as ProgramStatuses};
    use crate::terminal::{Exit, Handle, Placing, Viewer, ViewerOut, Wanted};

    pub(super) enum Model {
        Holds(Holding),
        Ended(Ending),
    }

    #[derive(Default)]
    pub(super) struct Holding {
        pub(super) terminals: Terminals,
        pub(super) viewers: Viewers,
    }

    #[derive(Default)]
    pub(super) struct Terminals {
        last: Option<TerminalId>,
        slots: BTreeMap<TerminalId, Slot>,
        empty: Empty,
    }

    #[derive(Default)]
    pub(super) struct Viewers {
        last_client: Option<ClientId>,
        came: u64,
        viewing: BTreeMap<ClientId, Viewing>,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    enum Empty {
        #[default]
        Waits,
        Ends,
    }

    enum Slot {
        Starting,
        Placed(Box<Terminal>),
    }

    pub(super) struct Terminal {
        key: TerminalKey,
        tag: Tag,
        size: Size,
        title: String,
        cwd: AbsoluteCwd,
        tty: String,
        program: Program,
        handle: Handle,
    }

    enum Program {
        Runs {
            pid: ProcessId,
            ends: Ends,
            foreground: Option<Foreground>,
            ran: Option<Doing>,
            statuses: ProgramStatuses,
            resumes: Option<Box<TerminalResumes>>,
        },
        Ended {
            exit: Exit,
            statuses: EndedProgramStatuses,
        },
    }

    struct Viewing {
        out: ViewerOut,
        colours: Colours,
        runs_in: Option<TerminalKey>,
        shows: BTreeMap<TerminalKey, u64>,
        _gone: oneshot::Sender<()>,
    }

    pub(super) struct MetaMut<'a> {
        terminal: &'a mut Terminal,
        modified: bool,
    }

    pub(super) fn mutate_meta(
        terminal: &mut Terminal,
        change: impl FnOnce(&mut MetaMut<'_>),
    ) -> bool {
        let mut meta = MetaMut {
            terminal,
            modified: false,
        };
        change(&mut meta);
        meta.modified
    }

    impl MetaMut<'_> {
        pub(super) fn set_tag(&mut self, tag: Tag) {
            if self.terminal.tag != tag {
                self.terminal.tag = tag;
                self.modified = true;
            }
        }

        pub(super) fn set_title(&mut self, title: String) {
            if matches!(self.terminal.program, Program::Runs { .. }) && self.terminal.title != title
            {
                self.terminal.title = title;
                self.modified = true;
            }
        }

        pub(super) fn set_cwd(&mut self, cwd: AbsoluteCwd) {
            if matches!(self.terminal.program, Program::Runs { .. }) && self.terminal.cwd != cwd {
                self.terminal.cwd = cwd;
                self.modified = true;
            }
        }

        pub(super) fn set_looked(&mut self, foreground: Option<Foreground>, ran: Option<Doing>) {
            let Program::Runs {
                foreground: was,
                ran: said,
                ..
            } = &mut self.terminal.program
            else {
                return;
            };
            if *was == foreground && *said == ran {
                return;
            }
            (*was, *said) = (foreground, ran);
            self.modified = true;
        }

        pub(super) fn set_program_statuses(&mut self, statuses: ProgramStatuses) {
            let Program::Runs { statuses: was, .. } = &mut self.terminal.program else {
                return;
            };
            if *was == statuses {
                return;
            }
            *was = statuses;
            self.modified = true;
        }

        pub(super) fn set_program_resumes(&mut self, resumes: Option<TerminalResumes>) {
            let Program::Runs { resumes: was, .. } = &mut self.terminal.program else {
                return;
            };
            let resumes = resumes.map(Box::new);
            if *was == resumes {
                return;
            }
            *was = resumes;
            self.modified = true;
        }
    }

    impl Terminal {
        pub(super) fn running(
            key: TerminalKey,
            tag: Tag,
            size: Size,
            cwd: AbsoluteCwd,
            tty: String,
            pid: ProcessId,
            ends: Ends,
            handle: Handle,
        ) -> Terminal {
            Terminal {
                key,
                tag,
                size,
                title: String::new(),
                cwd,
                tty,
                program: Program::Runs {
                    pid,
                    ends,
                    foreground: None,
                    ran: None,
                    statuses: ProgramStatuses::default(),
                    resumes: None,
                },
                handle,
            }
        }

        pub(super) fn handle(&self) -> &Handle {
            &self.handle
        }

        pub(super) fn set_size(&mut self, size: Size) -> bool {
            if self.size == size {
                return false;
            }
            self.size = size;
            true
        }

        fn said(&self) -> api::Terminal {
            let (program, statuses) = match &self.program {
                Program::Runs {
                    pid,
                    foreground,
                    ran,
                    statuses,
                    resumes,
                    ..
                } => {
                    let foreground = foreground.as_ref().map(Foreground::said);
                    let ran = ran.as_ref().map(Doing::to_mux);
                    let resumes = resumes.as_deref().map(TerminalResumes::to_mux);
                    let program = api::terminal::Program::Runs(api::Running {
                        pid: pid.get(),
                        foreground,
                        ran,
                        resumes,
                    });
                    (program, statuses.to_mux())
                }
                Program::Ended { exit, statuses } => (
                    api::terminal::Program::Ended(exit.said()),
                    statuses.to_mux(),
                ),
            };
            api::Terminal {
                key: Some(self.key.said()),
                tag: self.tag.said(),
                size: Some(self.size.said()),
                title: self.title.clone(),
                cwd: self.cwd.as_str().to_owned(),
                program: Some(program),
                statuses,
                tty: Some(self.tty.clone()),
            }
        }
    }

    impl Model {
        pub(super) fn new() -> Model {
            Model::Holds(Holding::default())
        }

        pub(super) fn settle(&self) {
            if let Model::Holds(holding) = self {
                holding.settle();
            }
        }

        pub(super) fn change_viewers<T>(
            &mut self,
            when_ended: impl FnOnce(&Ending) -> T,
            change: impl FnOnce(&Terminals, &mut Viewers) -> T,
        ) -> T {
            match self {
                Model::Holds(holding) => {
                    let changed = change(&holding.terminals, &mut holding.viewers);
                    holding.settle();
                    changed
                }
                Model::Ended(ending) => when_ended(ending),
            }
        }

        pub(super) fn listed(&self) -> Listed {
            match self {
                Model::Holds(holding) => Listed::Holds(holding.terminals.listing()),
                Model::Ended(ending) => Listed::Ended(ending.clone()),
            }
        }

        pub(super) fn listing(&self) -> api::Listing {
            match self {
                Model::Holds(holding) => holding.terminals.listing(),
                Model::Ended(_) => api::Listing::default(),
            }
        }

        fn holding(&mut self) -> Result<&mut Holding, Status> {
            match self {
                Model::Holds(holding) => Ok(holding),
                Model::Ended(ending) => Err(ending_status(ending)),
            }
        }

        pub(super) fn reserve(&mut self) -> Result<TerminalKey, Status> {
            let holding = self.holding()?;
            refuse_if_full(holding.terminals.slots.len(), TERMINALS_MAX, "terminals")?;
            let made = made_now()?;
            let id = TerminalId::next(&mut holding.terminals.last);
            let was = holding.terminals.slots.insert(id, Slot::Starting);
            assert!(was.is_none(), "terminal {id} is new");
            Ok(TerminalKey { id, made })
        }

        pub(super) fn place(
            &mut self,
            key: TerminalKey,
            spawned: Result<(Terminal, Placing), Status>,
        ) -> Result<TerminalKey, Status> {
            let (terminal, placing) = spawned?;
            let holding = self.holding()?;
            assert_eq!(terminal.key, key);
            let slot = holding
                .terminals
                .slots
                .get_mut(&key.id)
                .expect("a reserved terminal has its slot");
            assert!(
                matches!(slot, Slot::Starting),
                "terminal {} is placed once",
                key.id
            );
            *slot = Slot::Placed(Box::new(terminal));
            holding.terminals.empty = Empty::Ends;
            placing.placed();
            Ok(key)
        }

        pub(super) fn unreserve(&mut self, terminal: TerminalId) {
            if let Model::Holds(holding) = self
                && let Some(Slot::Starting) = holding.terminals.slots.get(&terminal)
            {
                holding.terminals.slots.remove(&terminal);
                self.end_if_empty();
            }
        }

        pub(super) fn end_if_unused(&mut self) {
            if let Model::Holds(holding) = self {
                holding.terminals.empty = Empty::Ends;
            }
            self.end_if_empty();
        }

        fn end_if_empty(&mut self) {
            if let Model::Holds(holding) = self
                && holding.terminals.empty == Empty::Ends
                && holding.terminals.slots.is_empty()
            {
                *self = Model::Ended(Ending::Emptied);
            }
        }

        pub(super) fn end_unreached(&mut self, unreached: Unreached) -> Option<usize> {
            let Model::Holds(holding) = self else {
                return None;
            };
            let closed = holding.terminals.slots.len();
            for terminal in holding.terminals.placed() {
                terminal.handle.close();
            }
            *self = Model::Ended(Ending::Unreached(unreached));
            Some(closed)
        }

        pub(super) fn close(&mut self, terminal: TerminalId) {
            if let Model::Holds(holding) = self
                && holding.close(terminal)
            {
                self.end_if_empty();
            }
        }

        pub(super) fn ended(
            &mut self,
            terminal: TerminalId,
            exit: Exit,
            statuses: EndedProgramStatuses,
            final_meta: FinalMeta,
        ) -> (bool, bool) {
            let Some(found) = self.terminal_of_id_mut(terminal) else {
                return (true, false);
            };
            match found.program {
                Program::Runs {
                    ends: Ends::Closes, ..
                } => {
                    self.close(terminal);
                    (true, true)
                }
                Program::Runs {
                    ends: Ends::Holds, ..
                } => {
                    if let Some(title) = final_meta.title {
                        found.title = title;
                    }
                    if let Some(cwd) = final_meta.cwd {
                        found.cwd = cwd;
                    }
                    found.program = Program::Ended { exit, statuses };
                    (false, true)
                }
                Program::Ended { .. } => (false, false),
            }
        }

        pub(super) fn terminal_of_id_mut(&mut self, terminal: TerminalId) -> Option<&mut Terminal> {
            let Model::Holds(holding) = self else {
                return None;
            };
            holding.terminals.placed_mut(terminal)
        }

        pub(super) fn terminal(&self, key: TerminalKey) -> Result<&Terminal, Status> {
            let Model::Holds(holding) = self else {
                return Err(unmade(key));
            };
            holding.terminals.terminal(key)
        }

        pub(super) fn terminal_mut(&mut self, key: TerminalKey) -> Result<&mut Terminal, Status> {
            let Model::Holds(holding) = self else {
                return Err(unmade(key));
            };
            let found = holding
                .terminals
                .placed_mut(key.id)
                .filter(|found| found.key == key);
            found.ok_or_else(|| unmade(key))
        }

        pub(super) fn typed(
            &self,
            client: ClientId,
            terminal: TerminalKey,
        ) -> Result<Handle, Status> {
            let Model::Holds(holding) = self else {
                return Err(no_client(client));
            };
            holding.viewers.typed(&holding.terminals, client, terminal)
        }
    }

    impl Terminals {
        fn placed(&self) -> impl Iterator<Item = &Terminal> {
            self.slots.values().filter_map(|slot| match slot {
                Slot::Placed(terminal) => Some(terminal.as_ref()),
                Slot::Starting => None,
            })
        }

        fn placed_mut(&mut self, terminal: TerminalId) -> Option<&mut Terminal> {
            match self.slots.get_mut(&terminal)? {
                Slot::Placed(terminal) => Some(terminal.as_mut()),
                Slot::Starting => None,
            }
        }

        fn terminal(&self, key: TerminalKey) -> Result<&Terminal, Status> {
            match self.slots.get(&key.id) {
                Some(Slot::Placed(terminal)) if terminal.key == key => Ok(terminal),
                Some(Slot::Placed(_) | Slot::Starting) | None => Err(unmade(key)),
            }
        }

        fn listing(&self) -> api::Listing {
            let mut terminals: Vec<api::Terminal> = self.placed().map(Terminal::said).collect();
            bound_fair(
                &mut terminals,
                |terminal| {
                    terminal
                        .statuses
                        .as_mut()
                        .map(|statuses| &mut statuses.children)
                },
                |child| child.encoded_len().saturating_add(16),
                LISTING_CHILDREN_BYTES_MAX,
            );
            for terminal in &mut terminals {
                if terminal
                    .statuses
                    .as_ref()
                    .is_some_and(|statuses| statuses.root.is_none() && statuses.children.is_empty())
                {
                    terminal.statuses = None;
                }
            }
            bound_listing_resumes(&mut terminals, LISTING_RESUMES_BYTES_MAX);
            api::Listing { terminals }
        }
    }

    pub(super) fn bound_listing_resumes(terminals: &mut [api::Terminal], budget: usize) {
        let mut lists: Vec<Option<Vec<api::ResumeRecord>>> = terminals
            .iter_mut()
            .map(|terminal| {
                let Some(api::terminal::Program::Runs(runs)) = &mut terminal.program else {
                    return None;
                };
                let resumes = runs.resumes.take()?;
                let mut records = Vec::with_capacity(1 + resumes.others.len());
                if let Some(chosen) = resumes.chosen {
                    records.push(chosen);
                }
                records.extend(resumes.others);
                (!records.is_empty()).then_some(records)
            })
            .collect();
        bound_fair(
            &mut lists,
            Option::as_mut,
            |record| record.encoded_len().saturating_add(16),
            budget,
        );
        for (terminal, records) in terminals.iter_mut().zip(lists) {
            let Some(api::terminal::Program::Runs(runs)) = &mut terminal.program else {
                continue;
            };
            let Some(mut records) = records else {
                continue;
            };
            if records.is_empty() {
                continue;
            }
            let chosen = records.remove(0);
            runs.resumes = Some(api::TerminalResumes {
                chosen: Some(chosen),
                others: records,
            });
        }
    }

    impl Viewers {
        pub(super) fn view(
            &mut self,
            greeting: Greeting,
            out: ViewerOut,
            gone: oneshot::Sender<()>,
        ) -> Result<ClientId, Status> {
            refuse_if_full(self.viewing.len(), CLIENTS_MAX, "clients")?;
            let client = ClientId::next(&mut self.last_client);
            let Greeting { colours, runs_in } = greeting;
            let viewing = Viewing {
                out,
                colours,
                runs_in,
                shows: BTreeMap::new(),
                _gone: gone,
            };
            let was = self.viewing.insert(client, viewing);
            assert!(was.is_none(), "viewer {client} is new");
            Ok(client)
        }

        pub(super) fn show(
            &mut self,
            terminals: &Terminals,
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
                if let Err(refusal) = terminals.terminal(*key) {
                    gone.push(refusal);
                    continue;
                }
                let came = viewing.shows.get(key).copied().unwrap_or_else(|| {
                    self.came += 1;
                    self.came
                });
                now.insert(*key, came);
            }
            assert_eq!(now.len() + gone.len(), keys.len());
            self.viewing
                .get_mut(&client)
                .expect("a viewer found above")
                .shows = now;
            Ok(gone)
        }

        pub(super) fn unview(&mut self, client: ClientId) {
            self.viewing.remove(&client);
        }

        fn typed(
            &self,
            terminals: &Terminals,
            client: ClientId,
            terminal: TerminalKey,
        ) -> Result<Handle, Status> {
            let viewing = self.viewing.get(&client).ok_or_else(|| no_client(client))?;
            if !viewing.shows.contains_key(&terminal) {
                let not_shown = format!("terminal {} is not shown", terminal.id);
                return Err(Status::failed_precondition(not_shown));
            }
            let terminal = terminals
                .terminal(terminal)
                .expect("a shown terminal is the model's");
            Ok(terminal.handle.clone())
        }
    }

    impl Holding {
        fn close(&mut self, terminal: TerminalId) -> bool {
            let Some(Slot::Placed(closed)) = self.terminals.slots.get(&terminal) else {
                return false;
            };
            closed.handle.close();
            let key = closed.key;
            self.terminals.slots.remove(&terminal);
            for (client, viewing) in &mut self.viewers.viewing {
                if viewing.shows.remove(&key).is_some() {
                    told_closed(*client, &viewing.out, key);
                }
            }
            true
        }

        fn settle(&self) {
            assert!(self.viewers.viewing.len() <= CLIENTS_MAX);
            assert!(self.terminals.slots.len() <= TERMINALS_MAX);
            let placed = self.terminals.placed().next().is_some();
            assert!(
                !placed || self.terminals.empty == Empty::Ends,
                "a placed terminal ends the first wait"
            );
            let mut by_terminal: BTreeMap<TerminalKey, Vec<Viewer>> = BTreeMap::new();
            for (client, viewing) in &self.viewers.viewing {
                assert!(viewing.shows.len() <= SHOWN_MAX);
                if let Some(runs_in) = viewing.runs_in {
                    let shown = viewing.shows.contains_key(&runs_in);
                    assert!(
                        !shown,
                        "viewer {client} shows terminal {}, which it runs in",
                        runs_in.id
                    );
                }
                for (key, came) in &viewing.shows {
                    let frames = viewing.out.clone();
                    let colours = viewing.colours;
                    let viewer = Viewer {
                        client: *client,
                        frames,
                        colours,
                        came: *came,
                    };
                    by_terminal.entry(*key).or_default().push(viewer);
                }
            }
            for terminal in self.terminals.placed() {
                let viewers = by_terminal.remove(&terminal.key).unwrap_or_default();
                terminal.handle.want(Wanted {
                    size: terminal.size,
                    viewers: viewers.into(),
                });
            }
            assert!(
                by_terminal.is_empty(),
                "every shown terminal is the model's"
            );
        }
    }

    pub(super) fn ending_status(ending: &Ending) -> Status {
        match ending {
            Ending::Emptied => Status::unavailable("it is ending, as nothing is left in it"),
            Ending::Unreached(unreached) => {
                Status::unavailable(format!("it is ending: {unreached}"))
            }
        }
    }

    pub(super) fn no_client(client: ClientId) -> Status {
        Status::not_found(format!("no client {client}"))
    }

    fn made_now() -> Result<Made, Status> {
        Made::of(SystemTime::now()).ok_or_else(|| {
            Status::failed_precondition("the clock says a time before 1970, or after 2554")
        })
    }

    fn unmade(key: TerminalKey) -> Status {
        Status::not_found(format!("no terminal {} made then", key.id))
    }

    fn told_closed(client: ClientId, out: &ViewerOut, key: TerminalKey) {
        let closed = api::to_viewer::Event::Closed(api::Closed {
            terminal: Some(key.said()),
        });
        if let Err(TrySendError::Full(_)) = out.try_send(Ok(api::ToViewer {
            event: Some(closed),
        })) {
            eprintln!(
                "lune-mux: viewer {client} had no room to hear terminal {} closed",
                key.id
            );
        }
    }

    fn refuse_if_full(count: usize, max: usize, what: &str) -> Result<(), Status> {
        if count < max {
            return Ok(());
        }
        Err(Status::resource_exhausted(format!(
            "it holds {max} {what} at most"
        )))
    }
}

fn spawned(
    mux: &Arc<Mux>,
    key: TerminalKey,
    tag: Tag,
    opens: &Opens,
    size: Size,
) -> Result<(Terminal, Placing), Status> {
    let failed = |what: &str, error: std::io::Error| Status::internal(format!("{what}: {error}"));
    let token =
        ResumeToken::generate().map_err(|error| failed("no resume token generated", error))?;
    let (spec, ends, extra_vars, resume_guard) = match &opens.launch {
        Launch::Direct {
            program,
            args,
            ends,
        } => (
            SpawnSpec {
                cwd: opens.cwd.clone(),
                program: program.clone(),
                args: args.clone(),
            },
            *ends,
            Vec::new(),
            None,
        ),
        Launch::ResumeInShell {
            shell,
            kind,
            resume,
        } => {
            let prep =
                resume_exec::prepare(mux.runtime_dir.as_deref(), &opens.cwd, shell, *kind, resume)
                    .map_err(Status::failed_precondition)?;
            (prep.spec, Ends::Closes, prep.vars, Some(prep.guard))
        }
    };
    let mut vars: Vec<(&str, String)> = vec![
        (pty::TERMINAL_VAR, key.id.to_string()),
        (pty::TERMINAL_MADE_VAR, key.made.nanos().to_string()),
        (TOKEN_VAR, token.as_str().to_owned()),
    ];
    for (name, value) in &extra_vars {
        vars.push((name.as_str(), value.clone()));
    }
    if let Some(socket) = &mux.browser_socket {
        vars.push((
            pty::TERMINAL_BROWSER_SOCKET_VAR,
            socket.to_string_lossy().into_owned(),
        ));
    }
    let pty = Pty::opened(size).map_err(|error| failed("no pseudoterminal opened", error))?;
    let tty = pty.tty().to_owned();
    let child = pty
        .spawned(&spec, &vars)
        .map_err(|error| Status::failed_precondition(format!("opens did not start: {error}")))?;
    let started = terminal::start(Arc::clone(mux), key, pty, child, size, token, resume_guard)
        .map_err(|error| failed("the terminal did not start", error))?;
    let terminal = Terminal::running(
        key,
        tag,
        size,
        opens.cwd.clone(),
        tty,
        started.pid,
        ends,
        started.handle,
    );
    Ok((terminal, started.placing))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::num::NonZeroU16;

    use lune_protocol::socket::{self, Found, Unreached};
    use lune_waits::eventually;
    use prost::Message as _;
    use rustix::io::Errno;
    use rustix::process::{self, Pid};
    use tonic::Code;

    use super::model::bound_listing_resumes;
    use super::{Ending, FinalMeta, Listed, MetaUpdate, Mux, spawned};
    use crate::api::{self, Listing};
    use crate::asked::{AbsoluteCwd, Ends, Launch, Opens, Size, Tag};
    use crate::program_status::EndedProgramStatuses;
    use crate::terminal::Exit;

    pub(crate) fn panic_holding_the_model() {
        Mux::new().change(|_| panic!("the model's assertion fails"));
    }

    fn cwd(path: &str) -> AbsoluteCwd {
        AbsoluteCwd::of_path(path.to_owned()).unwrap()
    }

    fn opens() -> Opens {
        script("read x", &[])
    }

    fn script(script: &str, args: &[&str]) -> Opens {
        let program = "/bin/sh".to_owned();
        let args = ["-c", script]
            .iter()
            .chain(args)
            .map(|arg| (*arg).to_owned())
            .collect();
        Opens {
            cwd: cwd("/"),
            launch: Launch::Direct {
                program,
                args,
                ends: Ends::Holds,
            },
        }
    }

    fn unstartable() -> Opens {
        let program = "/nonexistent/program".to_owned();
        Opens {
            cwd: cwd("/"),
            launch: Launch::Direct {
                program,
                args: Vec::new(),
                ends: Ends::Holds,
            },
        }
    }

    fn size() -> Size {
        Size {
            columns: NonZeroU16::new(20).unwrap(),
            rows: NonZeroU16::new(4).unwrap(),
        }
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
        let reserved = mux.reserve_terminal().unwrap();
        mux.end_if_unused();
        let key = reserved.start(&opens(), tag(), size()).unwrap();
        assert_eq!(listed(&mux), [key.id.said()]);
        mux.close_terminal(key).unwrap();
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Emptied));
    }

    #[test]
    fn a_first_terminal_that_fails_to_start_leaves_the_mux_waiting_for_one() {
        let mux = Mux::new();
        let reserved = mux.reserve_terminal().unwrap();
        let refusal = reserved.start(&unstartable(), tag(), size()).unwrap_err();
        assert_eq!(refusal.code(), Code::FailedPrecondition);
        assert!(
            refusal.message().starts_with("opens did not start: "),
            "{refusal:?}"
        );
        assert_eq!(*mux.watched().borrow(), Listed::Holds(Listing::default()));
        let key = mux
            .reserve_terminal()
            .unwrap()
            .start(&opens(), tag(), size())
            .unwrap();
        assert_eq!(listed(&mux), [key.id.said()]);
    }

    #[test]
    fn a_reservation_dropped_unstarted_gives_its_place_back() {
        let mux = Mux::new();
        drop(mux.reserve_terminal().unwrap());
        assert_eq!(*mux.watched().borrow(), Listed::Holds(Listing::default()));
        mux.end_if_unused();
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Emptied));
    }

    #[test]
    fn a_start_that_fails_once_the_first_wait_is_over_ends_the_mux() {
        let mux = Mux::new();
        let reserved = mux.reserve_terminal().unwrap();
        mux.end_if_unused();
        reserved.start(&unstartable(), tag(), size()).unwrap_err();
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Emptied));
    }

    #[test]
    fn a_start_that_fails_once_the_last_terminal_closed_ends_the_mux() {
        let mux = Mux::new();
        let placed = mux
            .reserve_terminal()
            .unwrap()
            .start(&opens(), tag(), size())
            .unwrap();
        let reserved = mux.reserve_terminal().unwrap();
        mux.close_terminal(placed).unwrap();
        assert_eq!(*mux.watched().borrow(), Listed::Holds(Listing::default()));
        reserved.start(&unstartable(), tag(), size()).unwrap_err();
        assert_eq!(*mux.watched().borrow(), Listed::Ended(Ending::Emptied));
    }

    #[test]
    fn a_terminal_that_starts_as_the_mux_ends_is_not_placed() {
        let folder = tempfile::tempdir().unwrap();
        let said = folder.path().join("pid");
        let mux = Mux::new();
        let reserved = mux.reserve_terminal().unwrap();
        let holds = r#"trap '' HUP TERM; echo $$ > "$0.new" && mv "$0.new" "$0"
            while :; do sleep 1; done"#;
        let holds = script(holds, &[said.to_str().unwrap()]);
        let started = spawned(&mux, reserved.key, tag(), &holds, size());
        let pid = eventually("the program says its pid", || {
            fs::read_to_string(&said).ok()?.trim().parse::<i32>().ok()
        });
        let pid = Pid::from_raw(pid).unwrap();
        let unreached = unreached();
        assert_eq!(mux.end_unreached(unreached.clone()), Some(1));
        let refusal = reserved.place(started).unwrap_err();
        assert_eq!(refusal.code(), Code::Unavailable);
        assert_eq!(
            *mux.watched().borrow(),
            Listed::Ended(Ending::Unreached(unreached))
        );
        assert_eq!(listed(&mux), [] as [u32; 0]);
        eventually(
            "the program ends, though it ignores hangups and terms",
            || (process::test_kill_process(pid) == Err(Errno::SRCH)).then_some(()),
        );
    }

    #[test]
    fn a_terminal_that_fails_to_start_as_the_mux_ends_says_why_it_failed() {
        let mux = Mux::new();
        let reserved = mux.reserve_terminal().unwrap();
        assert_eq!(mux.end_unreached(unreached()), Some(1));
        let refusal = reserved.start(&unstartable(), tag(), size()).unwrap_err();
        assert_eq!(refusal.code(), Code::FailedPrecondition);
    }

    #[test]
    fn what_a_terminal_says_before_it_is_placed_is_not_lost() {
        let folder = tempfile::tempdir().unwrap();
        let said = folder.path().join("said");
        let mux = Mux::new();
        let reserved = mux.reserve_terminal().unwrap();
        let titles = r#"printf '\033]2;early\007'; : > "$0"; read x"#;
        let titles = script(titles, &[said.to_str().unwrap()]);
        let started = spawned(&mux, reserved.key, tag(), &titles, size());
        eventually("the program said its title", || said.exists().then_some(()));
        reserved.place(started).unwrap();
        eventually("the title is listed", || {
            let listing = mux.listing();
            listing
                .terminals
                .iter()
                .any(|terminal| terminal.title == "early")
                .then_some(())
        });
    }

    #[test]
    fn listing_resumes_are_fair_bounded_preserving_chosen_before_others() {
        let make_record = |pid: u32, payload_bytes: usize| api::ResumeRecord {
            pid,
            proc_start: "100".to_owned(),
            uid: 1000,
            process_name: "nvim".to_owned(),
            process_exe: None,
            process_argv: None,
            placement: api::ProcessPlacement::ShellHosted.into(),
            argv: vec!["nvim".to_owned(), "x".repeat(payload_bytes)],
            cwd: None,
            reported_at: None,
        };
        let make_terminal =
            |id: u32, chosen: api::ResumeRecord, others: Vec<api::ResumeRecord>| api::Terminal {
                key: Some(api::TerminalKey { id, made_at: None }),
                program: Some(api::terminal::Program::Runs(api::Running {
                    pid: id,
                    foreground: None,
                    ran: None,
                    resumes: Some(api::TerminalResumes {
                        chosen: Some(chosen),
                        others,
                    }),
                })),
                ..api::Terminal::default()
            };
        let unit_cost = make_record(1, 200).encoded_len() + 16;
        let mut terminals = vec![
            make_terminal(
                1,
                make_record(10, 200),
                vec![make_record(11, 200), make_record(12, 200)],
            ),
            make_terminal(2, make_record(20, 200), vec![make_record(21, 200)]),
            make_terminal(
                3,
                make_record(30, 200),
                vec![make_record(31, 200), make_record(32, 200)],
            ),
        ];
        bound_listing_resumes(&mut terminals, unit_cost * 4);
        let counts: Vec<(Option<u32>, usize)> = terminals
            .iter()
            .map(|t| match t.program.as_ref() {
                Some(api::terminal::Program::Runs(runs)) => match &runs.resumes {
                    Some(res) => (res.chosen.as_ref().map(|r| r.pid), res.others.len()),
                    None => (None, 0),
                },
                _ => (None, 0),
            })
            .collect();
        assert_eq!(counts, [(Some(10), 1), (Some(20), 0), (Some(30), 0)]);

        bound_listing_resumes(&mut terminals, unit_cost);
        let kept: Vec<bool> = terminals
            .iter()
            .map(|t| match t.program.as_ref() {
                Some(api::terminal::Program::Runs(runs)) => runs.resumes.is_some(),
                _ => false,
            })
            .collect();
        assert_eq!(kept, [true, false, false]);
    }

    #[test]
    fn unchanged_metadata_and_viewer_changes_wake_no_listing_watch() {
        use std::collections::BTreeSet;

        use tokio::sync::{mpsc, oneshot};

        use crate::asked::{Colours, Greeting};

        let mux = Mux::new();
        let key = mux
            .reserve_terminal()
            .unwrap()
            .start(&opens(), tag(), size())
            .unwrap();
        let mut watched = mux.watched();
        let _ = watched.borrow_and_update();

        mux.titled(key.id, MetaUpdate::Title("one".to_owned()));
        assert!(watched.has_changed().unwrap());
        let _ = watched.borrow_and_update();

        mux.titled(key.id, MetaUpdate::Title("one".to_owned()));
        assert!(!watched.has_changed().unwrap());

        mux.titled(key.id, MetaUpdate::Cwd(cwd("/")));
        assert!(!watched.has_changed().unwrap());

        mux.size_terminal(key, size()).unwrap();
        assert!(!watched.has_changed().unwrap());

        let (out, _in) = mpsc::channel(1);
        let (gone, _let_go) = oneshot::channel();
        let client = mux
            .view(
                Greeting {
                    colours: Colours::default(),
                    runs_in: None,
                },
                out,
                gone,
            )
            .unwrap();
        assert!(!watched.has_changed().unwrap());
        assert!(mux.show(client, &BTreeSet::from([key])).unwrap().is_empty());
        assert!(!watched.has_changed().unwrap());
        mux.unview(client);
        assert!(!watched.has_changed().unwrap());

        mux.titled(key.id, MetaUpdate::Cwd(cwd("/tmp")));
        assert!(watched.has_changed().unwrap());
        let _ = watched.borrow_and_update();

        assert!(!mux.ended(
            key.id,
            Exit::Code(0),
            EndedProgramStatuses::default(),
            FinalMeta {
                title: Some("done".to_owned()),
                cwd: Some(cwd("/var"))
            },
        ));
        assert!(watched.has_changed().unwrap());
        let listed = watched.borrow_and_update().clone();
        let Listed::Holds(listing) = listed else {
            panic!("held")
        };
        assert_eq!(listing.terminals[0].title, "done");
        assert_eq!(listing.terminals[0].cwd, "/var");

        mux.titled(
            key.id,
            MetaUpdate::Both {
                title: "late".to_owned(),
                cwd: cwd("/opt"),
            },
        );
        assert!(!watched.has_changed().unwrap());
        assert!(!mux.ended(
            key.id,
            Exit::Code(1),
            EndedProgramStatuses::default(),
            FinalMeta {
                title: Some("again".to_owned()),
                cwd: None
            },
        ));
        assert!(!watched.has_changed().unwrap());
        let listing = mux.listing();
        assert_eq!(listing.terminals[0].title, "done");
        assert_eq!(listing.terminals[0].cwd, "/var");

        mux.close_terminal(key).unwrap();
    }

    #[test]
    fn closing_terminal_ended_removes_it_without_applying_final_metadata() {
        let mux = Mux::new();
        let keeper = mux
            .reserve_terminal()
            .unwrap()
            .start(&opens(), tag(), size())
            .unwrap();
        let closing_opens = Opens {
            cwd: cwd("/"),
            launch: Launch::Direct {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "read x".to_owned()],
                ends: Ends::Closes,
            },
        };
        let closing = mux
            .reserve_terminal()
            .unwrap()
            .start(&closing_opens, tag(), size())
            .unwrap();
        let mut watched = mux.watched();
        let _ = watched.borrow_and_update();

        assert!(mux.ended(
            closing.id,
            Exit::Code(0),
            EndedProgramStatuses::default(),
            FinalMeta {
                title: Some("bye".to_owned()),
                cwd: Some(cwd("/tmp"))
            },
        ));
        assert!(watched.has_changed().unwrap());
        let Listed::Holds(listing) = watched.borrow_and_update().clone() else {
            panic!("keeper holds the mux open");
        };
        assert_eq!(listing.terminals.len(), 1);
        assert_eq!(listing.terminals[0].key.unwrap().id, keeper.id.said());

        mux.close_terminal(keeper).unwrap();
    }
}
