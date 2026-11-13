//! What the CLI asks lunetad, the daemon of this host, and what it has zellij
//! do, as `drive` does it.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::Subcommand;
use luneta_protocol::github::{self, State, Verdict};
use luneta_protocol::unread::Unread;
use lunetad::api::agent::{self, Status as Reported};
use lunetad::api::checkout::Head;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::remote::Reached;
use lunetad::api::say::Does;
use lunetad::api::work_tree::Stands;
use lunetad::api::{
    Agent, ListAgentsResponse, PullRequest, Say, SayRequest, Session, Status, UnreadFile, Utterance,
    Version, WatchRequest, WorkTree, Zellij,
};
use lunetad::asks::User;
use lunetad::folder::SOCKET;
use lunetad::mux::own::server::{self, Listing};
use lunetad::mux::{OwnPane, ZellijPane};
pub use lunetad::log::causes;
use lunetad::prunes::Occupied;
use lunetad::sayings::BYTES_MAX;
use lunetad::{release, Destination};
use prost::Message as _;
use tonic::transport::Channel;

use crate::bridge;
use crate::daemon;
use crate::drive::{self, Address, Agreed, Opening, Turning};
use crate::machines::{self, Added, Machine};
use crate::order::Order;
use crate::prune;
use crate::zellij::{self, Seat};

/// What the CLI asks lunetad, and what it has zellij do. Only `watch` and
/// `bridge` start a lunetad where none runs, as a program reads the host by
/// them: the rest say that none does, or, for `close`, that it cannot tell the
/// agents, or, for `ssh`, that it reaches the machine once one runs, or, for
/// `prune`, that it cannot tell where the panes are, and deletes nothing.
#[derive(Subcommand)]
pub enum Command {
    /// Say which lunetad runs, and for how long it has.
    Info,
    /// List the agents that run, and the pane each is in.
    Agents,
    /// List the zellij sessions that run, their panes, and how the work tree
    /// each pane works in stands.
    Sessions,
    /// Wait for the host to change, and write it out as the protocol encodes
    /// it, for a program to read. lunetad is started where none runs, and
    /// again where an older one does.
    Watch {
        /// The host as last seen, `<pid>:<seq>` from its version, to wait for
        /// the next. Without it, the host is written out at once.
        #[arg(long, value_parser = version)]
        after: Option<Version>,
    },
    /// Hand on what comes on stdin to lunetad's socket, and what comes on the
    /// socket to stdout, until either end hangs up, for a client on another
    /// host to ask it through ssh. lunetad is started where none runs, and
    /// again where an older one does, and once it answers, `lunetad` and a
    /// newline go to stdout before anything it says.
    #[command(hide = true)]
    Bridge,
    /// Have lunetad watch another machine through ssh, as the sidebar's
    /// `n m` does: add it to luneta's hosts file, and say how the first ssh
    /// to it goes, where lunetad runs. lunetad's ssh asks you nothing, so a
    /// machine whose host key ssh has yet to know is reached once you have
    /// run `ssh` to it yourself.
    Ssh {
        /// What ssh takes for a destination: a Tailscale name, a `Host` of
        /// your ssh config, or `me@vps`.
        destination: Destination,
    },
    /// Say which pane this runs in, as `<session>:<pane>`, by the name its
    /// session has now: zellij's variables keep the name it had as the pane
    /// opened, and lunetad follows a rename.
    Here,
    /// Open a pane, a tab or a session, as the menu's New group does.
    Open {
        #[command(subcommand)]
        what: Opening,
    },
    /// Take you to a session, or to a pane of one: the client that last typed
    /// in the session this runs in, which zellij moves.
    Go {
        /// `<session>` or `<session>:<pane>`, as `luneta agents` names them.
        #[arg(value_parser = drive::address)]
        to: Address,
    },
    /// Turn a pane into a tab, or a tab into panes of another, as the menu's
    /// Turn group does. What runs in them runs on.
    Turn {
        #[command(subcommand)]
        what: Turning,
    },
    /// Close panes and delete sessions, as the menu's Delete group does. The
    /// panes of a session close together: a tab goes once nothing left holds
    /// it open, and the session once all its tabs do. Without `--yes`, say
    /// what would go and which agents with it, and the plan to tell `--yes`,
    /// and close nothing.
    Close {
        /// Each `<session>` to delete, or `<session>:<pane>` to close.
        #[arg(required = true, value_parser = drive::address)]
        what: Vec<Address>,
        /// Close them, while what would go is still the plan said as `PLAN`.
        #[arg(long, value_name = "PLAN", value_parser = drive::agreed)]
        yes: Option<Agreed>,
    },
    /// Delete the worktrees of a repository whose pull requests are merged, with
    /// their branches, as the menu's `d m` does. Without `--yes`, say what would
    /// go, what stays and why, and the plan to tell `--yes`, and delete nothing.
    /// It asks gh of each worktree of the repository.
    Prune {
        /// A directory of the repository. Without one, this one.
        #[arg(long, value_name = "DIR")]
        repo: Option<PathBuf>,
        /// Delete them, while what would go is still the plan said as `PLAN`.
        #[arg(long, value_name = "PLAN", value_parser = drive::agreed)]
        yes: Option<Agreed>,
    },
    /// Read text aloud on the machine you sit at, whichever machine this runs
    /// on: queue it, after what waits to be read, and return once lunetad has
    /// it, without waiting for it to be read. It fails at once where no
    /// lunetad runs.
    Say {
        /// What to read. Without it, what comes on stdin.
        #[arg(allow_hyphen_values = true)]
        text: Option<String>,
        /// Read it first, and cut off what is being read.
        #[arg(long)]
        now: bool,
        /// Stop what is being read, and let go of what waits.
        #[arg(long, conflicts_with_all = ["text", "now"])]
        stop: bool,
    },
}

/// A version as `--after` spells it: `<pid>:<seq>`.
fn version(spelled: &str) -> Result<Version, String> {
    let (pid, seq) = spelled.split_once(':').ok_or("not <pid>:<seq>")?;
    let pid = pid.parse().map_err(|error| format!("the pid: {error}"))?;
    let seq = seq.parse().map_err(|error| format!("the seq: {error}"))?;
    Ok(Version { pid, seq })
}

/// Asks the daemon in `folder` what `command` asks, or has zellij do it.
pub fn run(command: &Command, folder: &Path) -> ExitCode {
    match command {
        Command::Info => ask(folder, daemon::running(folder), info),
        Command::Agents => {
            // Without the own sessions' names, an agent in one goes by its ids.
            let own = server::sessions().unwrap_or_else(|failure| {
                let why = "own sessions go by their ids, as luneta-mux cannot list them";
                eprintln!("luneta: {why}: {failure}");
                Vec::new()
            });
            ask(folder, daemon::running(folder), |daemon| agents(daemon, own))
        },
        Command::Sessions => ask(folder, daemon::running(folder), sessions),
        Command::Watch { after } => {
            let current = async { daemon::current(folder).await.map(|current| current.daemon) };
            ask(folder, current, |daemon| watch(daemon, *after))
        },
        Command::Bridge => bridge::run(folder),
        Command::Ssh { destination } => ssh(destination, folder),
        Command::Here => drive::here(folder),
        Command::Open { what } => drive::open(what, folder),
        Command::Go { to } => drive::go(to, folder),
        Command::Turn { what } => drive::turn(what),
        Command::Close { what, yes } => drive::close(what, *yes, folder),
        Command::Prune { repo, yes } => prune::run(repo.as_deref(), *yes, folder),
        Command::Say { text, now, stop } => say(text.as_deref(), *now, *stop, folder),
    }
}

/// Reaches the daemon in `folder` by `reaching`, and has `asking` ask it.
fn ask<R, F, Asked>(folder: &Path, reaching: R, asking: F) -> ExitCode
where
    R: Future<Output = Result<DaemonClient<Channel>, String>>,
    F: FnOnce(DaemonClient<Channel>) -> Asked,
    Asked: Future<Output = Result<(), Box<dyn Error>>>,
{
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let daemon = match runtime.block_on(reaching) {
        Ok(daemon) => daemon,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    if let Err(error) = runtime.block_on(asking(daemon)) {
        eprintln!("luneta: {}: {}", folder.join(SOCKET).display(), causes(&*error));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Gives lunetad in `folder` what to read aloud, `text` or else what comes on
/// stdin, from the pane this runs in, first where `now` says; or has it `stop`.
fn say(text: Option<&str>, now: bool, stop: bool, folder: &Path) -> ExitCode {
    let does = if stop {
        Does::Stop(())
    } else {
        let text = match text.map_or_else(from_stdin, |text| Ok(text.to_owned())) {
            Ok(text) => text,
            Err(why) => {
                eprintln!("luneta: {why}");
                return ExitCode::FAILURE;
            },
        };
        let utterance = Utterance { text, speaker: speaker_here() };
        if now { Does::Now(utterance) } else { Does::InTurn(utterance) }
    };
    let say = Some(Say { does: Some(does) });
    ask(folder, daemon::running(folder), |mut daemon| async move {
        daemon.say(SayRequest { say }).await?;
        Ok(())
    })
}

/// What comes on stdin, up to one byte over what lunetad takes, for it to say
/// that it does not. Or why it does not read.
fn from_stdin() -> Result<String, String> {
    let limit = BYTES_MAX as u64 + 1;
    let mut read = Vec::new();
    let got = io::stdin().take(limit).read_to_end(&mut read);
    got.map_err(|error| format!("stdin does not read: {error}"))?;
    Ok(String::from_utf8_lossy(&read).into_owned())
}

/// The pane this runs in, by the name its session had as it opened, or as you
/// asked from it in the ask box, for lunetad to say what it is in now: none
/// outside zellij.
fn speaker_here() -> Option<Zellij> {
    let (named, pane) = match zellij::here().ok()?? {
        Seat::Opened(ZellijPane { session, pane }) => (session, pane),
        Seat::Asked { user: User::Pane { session, pane }, .. } => (session.to_string(), pane),
        Seat::Asked { .. } | Seat::Own { .. } => return None,
    };
    Some(Zellij { named, session: None, pane })
}

/// Adds `destination` to luneta's hosts file, and says how lunetad's first
/// ssh to it goes, where one runs: it fails where that does.
fn ssh(destination: &Destination, folder: &Path) -> ExitCode {
    match machines::add(destination) {
        Ok(Added::Named) => println!("{destination}: added to luneta's hosts file"),
        Ok(Added::Already) => println!("{destination}: luneta's hosts file names it already"),
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    }
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let daemon = match runtime.block_on(daemon::running(folder)) {
        Ok(daemon) => daemon,
        Err(why) => {
            println!("{destination}: lunetad reaches it once it runs: {why}");
            return ExitCode::SUCCESS;
        },
    };
    match runtime.block_on(first_reach(daemon, destination)) {
        Ok(Some(Reached::Host(host))) => {
            let sessions = match host.sessions.len() {
                1 => "1 session".to_owned(),
                n => format!("{n} sessions"),
            };
            println!("{destination}: reached, {sessions}");
            ExitCode::SUCCESS
        },
        Ok(Some(Reached::Unreached(unreached))) => {
            let log = lunetad::state::path(lunetad::log::NAME);
            let log = log.map_or_else(|_| lunetad::log::NAME.to_owned(), |log| {
                log.display().to_string()
            });
            eprintln!("luneta: {destination}: unreached: {}", unreached.why);
            eprintln!("luneta: what ssh said of it is in {log}");
            ExitCode::FAILURE
        },
        Ok(Some(Reached::Reaching(())) | None) => {
            println!("{destination}: lunetad reaches it still; `luneta sessions` says how it went");
            ExitCode::SUCCESS
        },
        Err(error) => {
            eprintln!("luneta: {}: {}", folder.join(SOCKET).display(), causes(&*error));
            ExitCode::FAILURE
        },
    }
}

/// What lunetad has of `destination` once its first ssh there reached it or
/// failed: none where that takes longer than the most ssh is given, and the
/// hosts file a sweep or two to be read again.
async fn first_reach(
    mut daemon: DaemonClient<Channel>,
    destination: &Destination,
) -> Result<Option<Reached>, Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + lunetad::REACH + Duration::from_secs(5);
    let spelled = destination.to_string();
    let mut after = None;
    loop {
        let asked = daemon.watch(WatchRequest { after, kept: None, shown: false });
        let Ok(host) = tokio::time::timeout_at(deadline, asked).await else {
            return Ok(None);
        };
        let host = host?.into_inner();
        let named = host.remotes.into_iter().find(|remote| remote.destination == spelled);
        match named.and_then(|remote| remote.reached) {
            // Not read yet, or not reached yet, or a newer daemon's.
            None | Some(Reached::Reaching(())) => after = host.version,
            reached => return Ok(reached),
        }
    }
}

/// A runtime for the CLI's one thread, or why there is none.
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    let built = tokio::runtime::Builder::new_current_thread().enable_all().build();
    built.map_err(|error| format!("no runtime: {error}"))
}

/// The agents lunetad lists, for the CLI to say which a close ends, or why
/// it does not list them: none runs, or it does not answer. It starts none,
/// as `agents` does not.
pub fn listed_agents(folder: &Path) -> Result<ListAgentsResponse, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut daemon = daemon::running(folder).await?;
        let listed = daemon.list_agents(()).await;
        listed.map(tonic::Response::into_inner).map_err(|error| causes(&error))
    })
}

/// Where the panes and the agents of this machine work, as lunetad says now, or
/// why it does not say: none runs, or it does not answer, or it has yet to list
/// them. It starts none, as `agents` does not.
pub fn occupied(folder: &Path) -> Result<Occupied, String> {
    Occupied::of(&host(folder)?)
}

/// The host as the daemon in `folder` says it stands now, or why it does not
/// say.
pub fn host(folder: &Path) -> Result<lunetad::api::Host, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut daemon = daemon::running(folder).await?;
        let asked = daemon.watch(WatchRequest { after: None, kept: None, shown: false }).await;
        Ok(asked.map_err(|error| causes(&error))?.into_inner())
    })
}

async fn info(mut daemon: DaemonClient<Channel>) -> Result<(), Box<dyn Error>> {
    let info = daemon.info(()).await?.into_inner();
    let started = info.started_at.and_then(|at| SystemTime::try_from(at).ok());
    let build = release::spelled(&info.version, info.built);
    match started.and_then(|at| at.elapsed().ok()) {
        Some(up) => println!("lunetad {build} (pid {}), up {}", info.pid, span(up.as_secs())),
        None => println!("lunetad {build} (pid {})", info.pid),
    }
    Ok(())
}

/// One line to an agent, by its session and pane, as `line` writes it, an
/// own session's by its name among `own`.
async fn agents(
    mut daemon: DaemonClient<Channel>,
    own: Vec<Listing>,
) -> Result<(), Box<dyn Error>> {
    let listed = daemon.list_agents(()).await?.into_inner();
    let mut rows: Vec<(String, &Agent)> =
        listed.agents.iter().map(|agent| (seat_of(agent, &own), agent)).collect();
    rows.sort_by(|(a, _), (b, _)| a.cmp(b));
    let width = rows.iter().map(|(seat, _)| seat.len()).max().unwrap_or(0);
    for (seat, agent) in rows {
        println!("{}", line(&seat, width, agent));
    }
    if let Some(count) = listed.unread_count() {
        eprintln!("luneta: {}", unread(count));
    }
    Ok(())
}

/// Where `agent` runs: in zellij, as `seat` says, or in an own session, as
/// `<session>:<pane>` by the name it has among `own`, or as `?:<pane> (own
/// <session id>)` where `own` has none of its id made then, with `?` for an
/// id its daemon does not say.
pub fn seat_of(agent: &Agent, own: &[Listing]) -> String {
    match &agent.pane {
        Some(agent::Pane::Own(pane)) => {
            if let Ok(pane) = OwnPane::of(pane) {
                return own_seat(pane, own);
            }
            let id = pane.session.map_or_else(|| "?".to_owned(), |session| session.id.to_string());
            unnamed_own(&id, pane.pane)
        },
        Some(agent::Pane::Zellij(_)) | None => seat(agent.zellij()),
    }
}

/// Where an agent runs in the own pane `pane`, as `seat_of` says: by the
/// name its session has among `own`, or by its id where `own` has none of
/// its id made then.
pub fn own_seat(pane: OwnPane, own: &[Listing]) -> String {
    match own.iter().find(|listed| listed.key() == pane.session) {
        Some(named) => format!("{}:{}", named.name, pane.id),
        None => unnamed_own(&pane.session.id.to_string(), pane.id),
    }
}

/// The pane `pane` of the own session `id`, which goes by no name: `?` for
/// the name, as `seat` has for a zellij session it cannot tell, so that no
/// verb reads it as a pane of a session by a name.
fn unnamed_own(id: &str, pane: impl fmt::Display) -> String {
    format!("?:{pane} (own {id})")
}

/// Where an agent runs in `zellij`, as `<session>:<pane>`: `-` outside
/// zellij. One whose session was renamed says what it was named, and one whose
/// session the daemon cannot tell has `?` for one.
pub fn seat(zellij: Option<&Zellij>) -> String {
    match zellij {
        None => "-".to_owned(),
        Some(zellij) => match zellij.session.as_deref() {
            None => format!("?:{} (named {})", zellij.pane, zellij.named),
            Some(now) if now != zellij.named => {
                format!("{now}:{} (named {})", zellij.pane, zellij.named)
            },
            Some(now) => format!("{now}:{}", zellij.pane),
        },
    }
}

/// An agent on a line: `seat`, as wide as `width`, its status, its pid, and
/// its name or else where it works. A status the CLI does not know is the
/// word Claude Code wrote, and none at all is `?`.
pub fn line(seat: &str, width: usize, agent: &Agent) -> String {
    let status = match &agent.status {
        Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
            Ok(Status::Busy) => "busy",
            Ok(Status::Idle) => "idle",
            Ok(Status::Waiting) => "waiting",
            Ok(Status::Shell) => "shell",
            // A daemon newer than this CLI's.
            Ok(Status::Unspecified) | Err(_) => "?",
        },
        Some(Reported::OtherStatus(word)) => word.as_str(),
        None => "?",
    };
    let label = agent.name.as_ref().map_or(agent.cwd.as_str(), |name| name.text.as_str());
    format!("{seat:<width$}  {status:<7}  {:>7}  {label}", agent.pid)
}

/// The agent of a session file lunetad cannot read on a line, as `line` has
/// an agent: by its pid alone, as what it is doing no one can tell.
pub fn unread_line(seat: &str, width: usize, file: &UnreadFile) -> String {
    format!("{seat:<width$}  {:<7}  {:>7}  its session file does not read", "?", file.pid)
}

/// What the CLI says of the session files lunetad cannot read, `unread` of
/// them, whose agents it does not list.
pub fn unread(unread: Unread) -> String {
    format!("{unread}, and its log says which")
}

/// A session to a line, as the sidebar lists them, with its panes under it, each
/// by its tab and its id: where it works, then what is checked out there and what
/// changed on it, and the pull request of its branch, and the directory of its
/// repository. A pane
/// zellij says no directory of has `-` for one, and a directory git has said
/// nothing of has nothing after it.
/// Then each other machine lunetad watches, as its hosts file names it, with
/// a `/` after it, which no session's name holds, and its sessions under it,
/// or why it has none; and a word that the file names more than these, or
/// other machines, where lunetad could not read all of it.
async fn sessions(mut daemon: DaemonClient<Channel>) -> Result<(), Box<dyn Error>> {
    let asked = WatchRequest { after: None, kept: None, shown: false };
    let host = daemon.watch(asked).await?.into_inner();
    let order = Order::shared();
    let here = as_rows(&host.sessions, Some(&Machine::Local), &order);
    sessions_of(&here, &host.work_trees, &host.pull_requests, "");
    for remote in &host.remotes {
        let machine = &remote.destination;
        match &remote.reached {
            Some(Reached::Host(host)) => {
                println!("{machine}/");
                let far = machine.parse().map(Machine::Remote).ok();
                let far = as_rows(&host.sessions, far.as_ref(), &order);
                sessions_of(&far, &host.work_trees, &host.pull_requests, "  ");
            },
            Some(Reached::Reaching(())) => println!("{machine}/  (reaching it)"),
            Some(Reached::Unreached(unreached)) => {
                let since = unreached.since.and_then(|at| SystemTime::try_from(at).ok());
                let long = since.and_then(|at| at.elapsed().ok());
                let long = long.map(|long| format!(" for {}", span(long.as_secs())));
                let why = &unreached.why;
                println!("{machine}/  (unreached{}: {why})", long.unwrap_or_default());
            },
            // A newer daemon's.
            None => println!("{machine}/  (does not read)"),
        }
    }
    if host.unread_hosts.is_some_and(|unread| unread > 0) {
        println!("(lunetad could not read all of luneta's hosts file: its log says why)");
    }
    Ok(())
}

/// `sessions` in the order of the sidebar's rows on `machine`: oldest first, and
/// as `order` has them.
fn as_rows<'a>(
    sessions: &'a [Session],
    machine: Option<&Machine>,
    order: &Order,
) -> Vec<&'a Session> {
    let mut rows: Vec<&Session> = sessions.iter().collect();
    rows.sort_by_cached_key(|session| {
        let started = session.started();
        (started.is_none(), started, session.name.clone())
    });
    if let Some(machine) = machine {
        order.sessions(machine, &mut rows, |session| session.name.as_str());
    }
    rows
}

/// The `sessions` of a host, with what it says of the `work_trees` and the
/// `pull_requests` of their panes, as `sessions` says them, each line led by
/// `indent`.
fn sessions_of(
    sessions: &[&Session],
    work_trees: &[WorkTree],
    pull_requests: &[PullRequest],
    indent: &str,
) {
    for session in sessions {
        let Some(panes) = &session.panes else {
            println!("{indent}{}  (no panes listed yet)", session.name);
            continue;
        };
        println!("{indent}{}", session.name);
        for pane in &panes.panes {
            let at = format!("{}:{}", pane.tab_name, pane.id);
            let Some(cwd) = pane.cwd.as_deref() else {
                println!("{indent}  {at}  -");
                continue;
            };
            let Some(tree) = work_trees.iter().find(|tree| tree.dir == cwd) else {
                println!("{indent}  {at}  {cwd}");
                continue;
            };
            let stands = stands(tree).unwrap_or_else(|| "(does not read)".to_owned());
            let pull_request = pull_requests.iter().find(|it| it.dir == cwd);
            let pulled = pull_request.map(|it| {
                format!("  {}", pulled(it).unwrap_or_else(|| "(does not read)".to_owned()))
            });
            let pulled = pulled.unwrap_or_default();
            let repo = match tree.stands.as_ref() {
                Some(Stands::Checkout(checkout)) => checkout.repository.as_ref(),
                Some(Stands::NoCheckout(it)) => Some(&it.repository),
                Some(Stands::Outside(())) | None => None,
            };
            let repo = repo.map_or_default(|it| format!("  repo {it}"));
            println!("{indent}  {at}  {cwd}  {stands}{pulled}{repo}");
        }
    }
}

/// How a work tree stands, in words: its branch, or `detached`, and then how
/// many paths of each kind changed, the stashes, and where the branch is from
/// its upstream. A directory in no repository says so, and one in a repository
/// but outside its work trees says that. None for an answer
/// that leaves out what the protocol says is always there, as a newer
/// daemon's could.
fn stands(tree: &WorkTree) -> Option<String> {
    let checkout = match tree.stands.as_ref()? {
        Stands::NoCheckout(_) => return Some("no checkout".to_owned()),
        Stands::Outside(()) => return Some("no repository".to_owned()),
        Stands::Checkout(checkout) => checkout,
    };
    let head = checkout.head.as_ref()?;
    let changes = checkout.changes?;
    let mut said = vec![match head {
        Head::Detached(()) => "detached".to_owned(),
        Head::Branch(branch) => branch.name.clone(),
    }];
    let counts = [
        (changes.conflicted, "conflicted"),
        (changes.staged, "staged"),
        (changes.modified, "modified"),
        (changes.renamed, "renamed"),
        (changes.deleted, "deleted"),
        (changes.untracked, "untracked"),
        (checkout.stashes, "stashed"),
    ];
    let some = counts.iter().filter(|(count, _)| *count > 0);
    said.extend(some.map(|(count, kind)| format!("{count} {kind}")));
    if let Head::Branch(branch) = head
        && let Some(upstream) = branch.upstream
    {
        let (ahead, behind) = (upstream.ahead, upstream.behind);
        said.extend((ahead > 0).then(|| format!("{ahead} ahead")));
        said.extend((behind > 0).then(|| format!("{behind} behind")));
    }
    Some(said.join(", "))
}

/// A pull request, in words: its number and its state, and while it is open,
/// what its checks come to, where it has any, and that gh failed since it
/// said all that, where it did. None for one that does not read, as a newer
/// daemon's could send.
fn pulled(pull_request: &PullRequest) -> Option<String> {
    let pull_request = github::PullRequest::of_api(pull_request)?;
    let state = match pull_request.state {
        State::Open => "open",
        State::Draft => "draft",
        State::Merged => "merged",
        State::Closed => "closed",
    };
    let checks = match pull_request.verdict() {
        Some(Verdict::Failing) => ", checks failing",
        Some(Verdict::Pending) => ", checks pending",
        Some(Verdict::Passing) => ", checks passing",
        None => "",
    };
    let old = if pull_request.stale { ", as gh said before it failed" } else { "" };
    Some(format!("#{} {state}{checks}{old}", pull_request.number))
}

/// Writes out the host once it is no longer as `after` saw it.
async fn watch(
    mut daemon: DaemonClient<Channel>,
    after: Option<Version>,
) -> Result<(), Box<dyn Error>> {
    let host = daemon.watch(WatchRequest { after, kept: None, shown: false }).await?.into_inner();
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&host.encode_to_vec())?;
    stdout.flush()?;
    Ok(())
}

/// `secs` as the two units that say the most of it.
fn span(secs: u64) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m"),
    }
}
