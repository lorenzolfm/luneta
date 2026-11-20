use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::mem;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::SystemTime;

use luneta_protocol::git::{GitStatus, Head};
use luneta_protocol::said::Said;
use tokio::sync::oneshot;

use crate::agent::{Agent, Unread};
use crate::api;
use crate::claude::Scan;
use crate::git;
use crate::github::Found;
use crate::log;
use crate::mux::own::server::{self, Is};
use crate::titles::{self, Titled};
use crate::projects::{Dir, Holds, Name, Project, Projects, Revision, Seen, Stage, Subfolder};
use crate::projects::{ViewId, Worktree, WorktreeId, CLONE_WORKTREES_MAX};
use crate::remote::{Destination, Line, Named, Reached};
use crate::sayings::{Sayings, ToSay};
use crate::shells::Doing;
use crate::transcripts::SessionId;
use crate::mux::TerminalKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub key: TerminalKey,
    pub listed: Option<Box<server::Terminal>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    Shown(Shown),
    Lost,
    Looking,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Branch {
    pub repository: Said,
    pub name: Said,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    Listed(BTreeSet<Subfolder>),
    Unlisted(String),
}

pub enum Command {
    Agents(Scan),
    PiAgents(Vec<Agent>),
    Terminals(BTreeMap<ViewId, Located>),
    GitStatus { dir: String, status: GitStatus, why: Option<git::Why> },
    PullRequests(BTreeMap<String, Found>),
    Topics(HashMap<SessionId, Said>),
    Hosts(Named),
    Projects { projects: Result<Projects, String>, revision: Revision },
    Missing(BTreeSet<Dir>),
    Listed(BTreeMap<Dir, Listing>),
    Remote { destination: Destination, reached: Reached },
    Say { said: ToSay, at: SystemTime, answer: oneshot::Sender<()> },
    Dirs(oneshot::Sender<BTreeSet<String>>),
    ProjectFolders(oneshot::Sender<BTreeSet<Dir>>),
    Clones(oneshot::Sender<BTreeMap<Dir, BTreeSet<WorktreeId>>>),
    Plain(oneshot::Sender<BTreeSet<Dir>>),
    Branches(oneshot::Sender<BTreeMap<String, String>>),
    Tracking(oneshot::Sender<BTreeMap<String, Branch>>),
    Claudes(oneshot::Sender<Vec<Agent>>),
    AllAgents(oneshot::Sender<Vec<Agent>>),
    List(oneshot::Sender<api::ListAgentsResponse>),
    Watch { after: Option<After>, answer: oneshot::Sender<Option<api::Host>> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct After {
    pub version: api::Version,
    pub parked: Parked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parked {
    Quietly,
    Aloud,
}

pub struct Wakes {
    pub git: SyncSender<()>,
    pub github: SyncSender<()>,
}

const COMMANDS_MAX: usize = 64;

const WAITING_MAX: usize = 256;

pub fn ask<T>(
    core: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (answer, answered) = oneshot::channel();
    core.send(question(answer)).ok()?;
    answered.blocking_recv().ok()
}

pub fn start(wakes: Wakes) -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    thread::Builder::new().name("core".to_owned()).spawn(move || run(&commands, &wakes))?;
    Ok(send)
}

enum Remote {
    Far(Destination, Reached),
    Here(Destination),
}

impl Remote {
    fn to_api(&self) -> api::Remote {
        match self {
            Remote::Far(destination, reached) => reached.to_api(destination),
            Remote::Here(destination) => api::Remote {
                destination: destination.to_string(),
                reached: Some(api::remote::Reached::Here(())),
            },
        }
    }
}

struct Known {
    found: Scan,
    pi: Vec<Agent>,
    terminals: BTreeMap<ViewId, Located>,
    statuses: BTreeMap<String, GitStatus>,
    pull_requests: BTreeMap<String, Found>,
    topics: HashMap<SessionId, Said>,
    remotes: Vec<Remote>,
    unread_hosts: u32,
    projects: Result<Projects, String>,
    revision: Revision,
    missing: BTreeSet<Dir>,
    listed: BTreeMap<Dir, Listing>,
    host: api::Host,
    seq: u64,
    waiting: Vec<oneshot::Sender<Option<api::Host>>>,
    sayings: Sayings,
}

fn run(commands: &Receiver<Command>, wakes: &Wakes) {
    let mut known = Known {
        found: Scan { agents: Vec::new(), unread: Vec::new() },
        pi: Vec::new(),
        terminals: BTreeMap::new(),
        statuses: BTreeMap::new(),
        pull_requests: BTreeMap::new(),
        topics: HashMap::new(),
        remotes: Vec::new(),
        unread_hosts: 0,
        projects: Err("lunetad has read no projects yet".to_owned()),
        revision: Revision::default(),
        missing: BTreeSet::new(),
        listed: BTreeMap::new(),
        host: api::Host::default(),
        seq: 0,
        waiting: Vec::with_capacity(WAITING_MAX),
        sayings: Sayings::default(),
    };
    known.host = api::Host { version: Some(known.version()), ..known.built() };
    for command in commands {
        match command {
            Command::Agents(now) => {
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                known.found = now;
            },
            Command::PiAgents(now) => {
                let pids: HashSet<u32> = now.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.len(), "an agent's pid comes twice");
                known.pi = now;
            },
            Command::Terminals(now) => {
                let before = known.dirs();
                known.terminals = now;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
            },
            Command::GitStatus { dir, status, why } => {
                if !known.dirs().contains(&dir) {
                    continue;
                }
                let before = known.branch(&dir).map(str::to_owned);
                let changed = known.statuses.get(&dir).is_some_and(|was| *was != status);
                known.statuses.insert(dir.clone(), status);
                if changed && let Some(said) = why.and_then(|why| why.said(&dir)) {
                    log::say(&said);
                }
                if known.branch(&dir).is_some_and(|now| before.as_deref() != Some(now)) {
                    wake(&wakes.github);
                }
            },
            Command::PullRequests(now) => known.pull_requests = now,
            Command::Topics(now) => known.topics = now,
            Command::Hosts(now) => known.named(now),
            Command::Projects { projects, revision } => {
                let before = known.dirs();
                known.projects = projects;
                known.revision = revision;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
            },
            Command::Missing(now) if now == known.missing => continue,
            Command::Missing(now) => {
                let before = known.dirs();
                known.missing = now;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
            },
            Command::Listed(now) if now == known.listed => continue,
            Command::Listed(now) => known.listed = now,
            Command::Remote { destination, reached } => {
                let far = known.remotes.iter_mut().find_map(|remote| match remote {
                    Remote::Far(far, was) if *far == destination => Some(was),
                    Remote::Far(..) | Remote::Here(_) => None,
                });
                let Some(was) = far else {
                    continue;
                };
                *was = reached;
            },
            Command::Say { said, at, answer } => {
                known.sayings.take(said, at);
                let _ = answer.send(());
            },
            Command::Dirs(answer) => {
                let _ = answer.send(known.dirs());
                continue;
            },
            Command::ProjectFolders(answer) => {
                let _ = answer.send(known.projects.iter().flat_map(Projects::folders).collect());
                continue;
            },
            Command::Plain(answer) => {
                let _ = answer.send(known.projects.iter().flat_map(Projects::plain).collect());
                continue;
            },
            Command::Clones(answer) => {
                let clones = known.projects.iter().flat_map(Projects::clones);
                let _ = answer.send(clones.collect());
                continue;
            },
            Command::Branches(answer) => {
                let _ = answer.send(known.branches());
                continue;
            },
            Command::Tracking(answer) => {
                let _ = answer.send(known.tracking());
                continue;
            },
            Command::Claudes(answer) => {
                let _ = answer.send(known.found.agents.clone());
                continue;
            },
            Command::AllAgents(answer) => {
                let _ = answer.send(known.found.agents.iter().chain(&known.pi).cloned().collect());
                continue;
            },
            Command::List(answer) => {
                let (unread, unread_files) = known.unread();
                let agents = known.agents();
                let _ = answer.send(api::ListAgentsResponse { agents, unread, unread_files });
                continue;
            },
            Command::Watch { after, answer } => {
                known.watch(after, answer);
                continue;
            },
        }
        known.update();
    }
}

fn wake(thread: &SyncSender<()>) {
    match thread.try_send(()) {
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

impl Known {
    fn named(&mut self, now: Named) {
        let unique: HashSet<&Destination> = now.lines.iter().map(Line::destination).collect();
        assert_eq!(unique.len(), now.lines.len(), "a machine is named twice");
        self.unread_hosts = now.unread;
        let before = mem::take(&mut self.remotes).into_iter().filter_map(|remote| match remote {
            Remote::Far(destination, reached) => Some((destination, reached)),
            Remote::Here(_) => None,
        });
        let mut before: HashMap<Destination, Reached> = before.collect();
        for line in now.lines {
            let remote = match line {
                Line::Far(destination) => {
                    let reached = before.remove(&destination).unwrap_or(Reached::Reaching);
                    Remote::Far(destination, reached)
                },
                Line::Here(destination) => Remote::Here(destination),
            };
            self.remotes.push(remote);
        }
    }

    fn dirs(&self) -> BTreeSet<String> {
        let listed = self.terminals.values().filter_map(|found| match found {
            Located::Shown(Shown { listed: Some(listed), .. }) => Dir::parse(&listed.cwd).ok(),
            Located::Shown(Shown { listed: None, .. }) | Located::Lost | Located::Looking => None,
        });
        let cwds = listed.map(|cwd| cwd.as_str().to_owned());
        let present = |folder: &Dir| !self.missing.contains(folder);
        let folders = self.projects.iter().flat_map(Projects::worktree_folders).filter(present);
        cwds.chain(folders.map(|folder| folder.as_str().to_owned())).collect()
    }

    fn forget_dirs(&mut self) {
        let dirs = self.dirs();
        self.statuses.retain(|dir, _| dirs.contains(dir));
        self.pull_requests.retain(|dir, _| dirs.contains(dir));
    }

    fn branch(&self, dir: &str) -> Option<&str> {
        match self.statuses.get(dir)? {
            GitStatus::Checkout { head: Head::Branch { name, .. }, .. } => Some(name.as_str()),
            GitStatus::Checkout { head: Head::Detached, .. }
            | GitStatus::NoCheckout { .. }
            | GitStatus::Outside => None,
        }
    }

    fn branches(&self) -> BTreeMap<String, String> {
        let branch = |dir: &String| Some((dir.clone(), self.branch(dir)?.to_owned()));
        self.statuses.keys().filter_map(branch).collect()
    }

    fn tracking(&self) -> BTreeMap<String, Branch> {
        let tracking = |(dir, status): (&String, &GitStatus)| {
            let GitStatus::Checkout {
                repository: Some(repository),
                head: Head::Branch { name, upstream: Some(_), .. },
                ..
            } = status
            else {
                return None;
            };
            Some((dir.clone(), Branch { repository: repository.clone(), name: name.clone() }))
        };
        self.statuses.iter().filter_map(tracking).collect()
    }

    fn agents(&self) -> Vec<api::Agent> {
        let claude = self.found.agents.iter().map(|agent| {
            let topic = self.topics.get(agent.session_id.as_str()).cloned();
            agent.to_api(topic)
        });
        claude.chain(self.pi.iter().map(|agent| agent.to_api(None))).collect()
    }

    fn unread(&self) -> (u32, Vec<api::UnreadFile>) {
        let mut files: Vec<api::UnreadFile> =
            self.found.unread.iter().map(Unread::to_api).collect();
        files.sort_by_key(|file| file.pid);
        let count =
            u32::try_from(files.len()).expect("a folder listed in memory has under 2^32 files");
        (count, files)
    }

    fn built(&self) -> api::Host {
        let mut agents = self.agents();
        agents.sort_by_key(|agent| agent.pid);
        let (unread, unread_files) = self.unread();
        let pull_requests = self
            .pull_requests
            .iter()
            .filter(|(dir, found)| self.branch(dir) == Some(&found.branch))
            .map(|(dir, found)| found.pull_request.to_api(dir))
            .collect();
        let remotes = self.remotes.iter().map(Remote::to_api).collect();
        api::Host {
            version: None,
            git_statuses: self.statuses.iter().map(|(dir, status)| status.to_api(dir)).collect(),
            agents,
            unread,
            pull_requests,
            unread_files,
            remotes,
            unread_hosts: Some(self.unread_hosts),
            sayings: self.sayings.fresh(SystemTime::now()),
            projects: Some(self.built_projects()),
        }
    }

    fn built_projects(&self) -> api::Projects {
        let projects = match &self.projects {
            Ok(projects) => projects,
            Err(why) => {
                let unkept = api::projects::Held::Unkept(why.clone());
                return api::Projects { held: Some(unkept) };
            },
        };
        let worktree = |folder: Dir, given: &str, worktree: &Worktree| {
            let renamed = worktree.renamed().map(Name::as_str);
            api::Worktree {
                id: worktree.id().get(),
                name: renamed.unwrap_or(given).to_owned(),
                renamed: renamed.is_some(),
                seen: Some(self.seen(&folder).to_api()),
                folder: folder.as_str().to_owned(),
                terminals: self.built_terminals(worktree),
                used_at: worktree.used().map(|used| used.time().into()),
            }
        };
        let holds = |project: &Project| match project.holds() {
            Holds::Folder(own) => {
                let dir = project.dir();
                let given = self.branch(dir.as_str()).unwrap_or_else(|| dir.last());
                api::project::Holds::Folder(worktree(dir.clone(), given, own))
            },
            Holds::Clone(rows) => {
                let listing = Some(self.listing(project.dir(), rows));
                let rows = rows.iter().map(|(subfolder, it)| {
                    let folder = project.dir().at(subfolder);
                    let given = match self.branch(folder.as_str()) {
                        Some(branch) if subfolder.named_for(branch) => branch,
                        Some(_) | None => subfolder.as_str(),
                    };
                    worktree(folder, given, it)
                });
                let clone = api::project::Clone { worktrees: rows.collect(), listing };
                api::project::Holds::Clone(clone)
            },
        };
        let project = |project: &Project| api::Project {
            id: project.id().get(),
            name: project.name().as_str().to_owned(),
            dir: project.dir().as_str().to_owned(),
            missing: self.missing.contains(project.dir()),
            holds: Some(holds(project)),
        };
        let projects = projects.all().iter().map(project).collect();
        let kept = api::projects::Kept { projects, version: Some(self.revision.to_api()) };
        api::Projects { held: Some(api::projects::Held::Kept(kept)) }
    }

    fn listing(&self, dir: &Dir, rows: &[(Subfolder, Worktree)]) -> api::project::clone::Listing {
        use api::project::clone::Listing as Said;
        let listed = match self.listed.get(dir) {
            None => return Said::Unlisted("not listed yet".to_owned()),
            Some(Listing::Unlisted(why)) => return Said::Unlisted(why.clone()),
            Some(Listing::Listed(listed)) => listed,
        };
        let opened: BTreeSet<&Subfolder> = rows.iter().map(|(subfolder, _)| subfolder).collect();
        let unopened = listed.iter().filter(|it| !opened.contains(it));
        let subfolders: Vec<String> = unopened.map(|it| it.as_str().to_owned()).collect();
        assert!(subfolders.len() <= CLONE_WORKTREES_MAX, "a clone lists so many worktrees at most");
        Said::Unopened(api::project::Unopened { subfolders })
    }

    fn built_terminals(&self, worktree: &Worktree) -> Vec<api::Terminal> {
        use api::terminal::Found as Told;
        let built = worktree.layout().terminals().map(|terminal| {
            let id = terminal.id().view();
            let found = self.terminals.get(&id).unwrap_or(&Located::Looking);
            let (told, listed) = match found {
                Located::Shown(Shown { key, listed }) => {
                    (Told::Key((*key).into()), listed.as_deref())
                },
                Located::Lost => (Told::Lost(()), None),
                Located::Looking => (Told::Looking(()), None),
            };
            let cwd = listed.and_then(|listed| Dir::parse(&listed.cwd).ok());
            let folder = cwd.as_ref().unwrap_or_else(|| terminal.folder());
            let (command, doing, held) = match listed.map(|listed| &listed.is) {
                Some(Is::Running { front, doing, .. }) => {
                    let command = front.as_ref().and_then(|front| Said::new(front.argv.join(" ")));
                    (command, doing.as_ref(), false)
                },
                Some(Is::Ended) => (None, None, true),
                None => (None, None, false),
            };
            let titled = listed.and_then(|listed| titles::titled(&listed.title));
            api::Terminal {
                id: id.get(),
                found: Some(told),
                folder: folder.as_str().to_owned(),
                command: command.map(String::from),
                titled: titled.as_ref().map(Titled::to_api),
                shell: doing.map(Doing::to_api),
                held,
                name: terminal.name().map(|name| name.as_str().to_owned()),
            }
        });
        built.collect()
    }

    fn seen(&self, folder: &Dir) -> Seen {
        if self.missing.contains(folder) {
            return Seen::Missing;
        }
        if !self.statuses.contains_key(folder.as_str()) {
            return Seen::Unread;
        }
        let branch = self.branch(folder.as_str());
        let found = self.pull_requests.get(folder.as_str());
        let found = found.filter(|found| branch == Some(found.branch.as_str()));
        Seen::Read(Stage::of(branch, found.map(|found| &found.pull_request)))
    }

    fn version(&self) -> api::Version {
        api::Version { pid: std::process::id(), seq: self.seq }
    }

    fn update(&mut self) {
        self.forget_dirs();
        let mut host = self.built();
        host.version = self.host.version;
        if host == self.host {
            return;
        }
        self.seq = self.seq.checked_add(1).expect("a host changes fewer than 2^64 times");
        host.version = Some(self.version());
        self.host = host;
        for waiting in self.waiting.drain(..) {
            let _ = waiting.send(Some(self.host.clone()));
        }
    }

    fn watch(&mut self, after: Option<After>, answer: oneshot::Sender<Option<api::Host>>) {
        self.update();
        let version = self.version();
        assert_eq!(self.host.version, Some(version));
        let parked = match after {
            Some(after) if after.version == version => after.parked,
            Some(_) | None => {
                let _ = answer.send(Some(self.host.clone()));
                return;
            },
        };
        self.waiting.retain(|waiting| !waiting.is_closed());
        assert!(self.waiting.len() <= WAITING_MAX);
        if answer.is_closed() {
            return;
        }
        if self.waiting.len() == WAITING_MAX {
            let _ = answer.send(None);
            return;
        }
        self.waiting.push(answer);
        match parked {
            Parked::Quietly => {},
            Parked::Aloud => {
                let api::Version { pid, seq } = version;
                log::say(&format!("a watch waits for a change past {pid}:{seq}"));
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_naming_this_machine_takes_no_word_of_a_watch_until_it_names_another() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake() }).unwrap();
        let machine = |name: &str| name.parse::<Destination>().unwrap();
        let hosts = |lines| Command::Hosts(Named { lines, unread: 0 });
        let heard = |name: &str| Command::Remote {
            destination: machine(name),
            reached: Reached::Host(api::remote::Host::default()),
        };
        let told = || {
            let host = ask(&core, |answer| Command::Watch { after: None, answer });
            let remotes = host.flatten().unwrap().remotes.into_iter();
            remotes.map(|remote| (remote.destination, remote.reached.unwrap())).collect::<Vec<_>>()
        };
        let said = |name: &str, reached| (name.to_owned(), reached);
        let host = || api::remote::Reached::Host(api::remote::Host::default());
        let (here, reaching) = (api::remote::Reached::Here(()), api::remote::Reached::Reaching(()));

        core.send(hosts(vec![Line::Here(machine("nixos")), Line::Far(machine("vps"))])).unwrap();
        assert_eq!(told(), [said("nixos", here.clone()), said("vps", reaching.clone())]);
        core.send(heard("nixos")).unwrap();
        core.send(heard("vps")).unwrap();
        assert_eq!(told(), [said("nixos", here), said("vps", host())]);

        core.send(hosts(vec![Line::Far(machine("nixos")), Line::Far(machine("vps"))])).unwrap();
        assert_eq!(told(), [said("nixos", reaching), said("vps", host())]);
    }
}
