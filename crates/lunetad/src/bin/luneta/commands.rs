use std::error::Error;
use std::future::Future;
use std::io::{self, Read as _, Write as _};
use std::num::NonZeroU32;
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
    Agent, ListAgentsResponse, PullRequest, Say, SayRequest, Session, Status, UnreadFile,
    Version, WatchRequest, WorkTree, Zellij,
};
use lunetad::asks::User;
use lunetad::folder::SOCKET;
use lunetad::mux::own::server::{self, Listing};
use lunetad::mux::{Failure, OwnId, OwnPane, SessionName, ZellijPane};
pub use lunetad::log::causes;
use lunetad::prunes::Occupied;
use lunetad::sayings::{self, Asker, BYTES_MAX};
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

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "Say which lunetad runs, and for how long it has")]
    Info,
    #[command(about = "List the agents that run, and the pane each is in")]
    Agents,
    #[command(
        about = "List the zellij sessions that run, their panes, and how the work tree each pane \
                 works in stands"
    )]
    Sessions,
    #[command(
        about = "Wait for the host to change, and write it out as the protocol encodes it, for a \
                 program to read. lunetad is started where none runs, and again where an older \
                 one does"
    )]
    Watch {
        #[arg(
            long,
            value_parser = version,
            help = "The host as last seen, `<pid>:<seq>` from its version, to wait for the next. \
                    Without it, the host is written out at once"
        )]
        after: Option<Version>,
    },
    #[command(
        hide = true,
        about = "Hand on what comes on stdin to lunetad's socket, and what comes on the socket to \
                 stdout, until either end hangs up, for a client on another host to ask it \
                 through ssh. lunetad is started where none runs, and again where an older one \
                 does, and once it answers, `lunetad` and a newline go to stdout before anything \
                 it says"
    )]
    Bridge,
    #[command(
        about = "Have lunetad watch another machine through ssh, as the sidebar's `n m` does: add \
                 it to luneta's hosts file, and say how the first ssh to it goes, where lunetad \
                 runs. lunetad's ssh asks you nothing, so a machine whose host key ssh has yet to \
                 know is reached once you have run `ssh` to it yourself"
    )]
    Ssh {
        #[arg(
            help = "What ssh takes for a destination: a Tailscale name, a `Host` of your ssh \
                    config, or `me@vps`"
        )]
        destination: Destination,
    },
    #[command(
        about = "Say which pane this runs in, as `<session>:<pane>`, by the name its session has \
                 now: zellij's variables keep the name it had as the pane opened, and lunetad \
                 follows a rename"
    )]
    Here,
    #[command(about = "Open a pane, a tab or a session, as the menu's New group does")]
    Open {
        #[command(subcommand)]
        what: Opening,
    },
    #[command(
        about = "Take you to a session, or to a pane of one: the client that last typed in the \
                 session this runs in, which zellij moves"
    )]
    Go {
        #[arg(
            value_parser = drive::address,
            help = "`<session>` or `<session>:<pane>`, as `luneta agents` names them"
        )]
        to: Address,
    },
    #[command(
        about = "Turn a pane into a tab, or a tab into panes of another, as the menu's Turn group \
                 does. What runs in them runs on"
    )]
    Turn {
        #[command(subcommand)]
        what: Turning,
    },
    #[command(
        about = "Close panes and delete sessions, as the menu's Delete group does. The panes of a \
                 session close together: a tab goes once nothing left holds it open, and the \
                 session once all its tabs do. Without `--yes`, say what would go and which \
                 agents with it, and the plan to tell `--yes`, and close nothing"
    )]
    Close {
        #[arg(
            required = true,
            value_parser = drive::address,
            help = "Each `<session>` to delete, or `<session>:<pane>` to close"
        )]
        what: Vec<Address>,
        #[arg(
            long,
            value_name = "PLAN",
            value_parser = drive::agreed,
            help = "Close them, while what would go is still the plan said as `PLAN`"
        )]
        yes: Option<Agreed>,
    },
    #[command(
        about = "Delete the worktrees of a repository whose pull requests are merged, with their \
                 branches, as the menu's `d m` does. Without `--yes`, say what would go, what \
                 stays and why, and the plan to tell `--yes`, and delete nothing. It asks gh of \
                 each worktree of the repository"
    )]
    Prune {
        #[arg(
            long,
            value_name = "DIR",
            help = "A directory of the repository. Without one, this one"
        )]
        repo: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PLAN",
            value_parser = drive::agreed,
            help = "Delete them, while what would go is still the plan said as `PLAN`"
        )]
        yes: Option<Agreed>,
    },
    #[command(
        about = "Read text aloud on the machine you sit at, whichever machine this runs on: queue \
                 it, after what waits to be read, and return once lunetad has it, without waiting \
                 for it to be read. It fails at once where no lunetad runs"
    )]
    Say {
        #[arg(allow_hyphen_values = true, help = "What to read. Without it, what comes on stdin")]
        text: Option<String>,
        #[arg(long, help = "Read it first, and cut off what is being read")]
        now: bool,
        #[arg(
            long,
            conflicts_with_all = ["text", "now"],
            help = "Stop what is being read, and let go of what waits"
        )]
        stop: bool,
    },
}

fn version(spelled: &str) -> Result<Version, String> {
    let (pid, seq) = spelled.split_once(':').ok_or("not <pid>:<seq>")?;
    let pid = pid.parse().map_err(|error| format!("the pid: {error}"))?;
    let seq = seq.parse().map_err(|error| format!("the seq: {error}"))?;
    Ok(Version { pid, seq })
}

pub fn run(command: &Command, folder: &Path) -> ExitCode {
    match command {
        Command::Info => ask(folder, daemon::running(folder), info),
        Command::Agents => {
            let own = server::listed();
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

fn say(text: Option<&str>, now: bool, stop: bool, folder: &Path) -> ExitCode {
    let mut untold = None;
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
        let seat = zellij::here().unwrap_or_else(|why| {
            untold = Some(why);
            None
        });
        let asker = match seat.map(asker_of).transpose() {
            Ok(asker) => asker.flatten(),
            Err(why) => {
                eprintln!("luneta: speaker: {why}");
                return ExitCode::FAILURE;
            },
        };
        let utterance = sayings::asked(text, asker);
        if now { Does::Now(utterance) } else { Does::InTurn(utterance) }
    };
    let say = Some(Say { does: Some(does) });
    let asked = ask(folder, daemon::running(folder), |mut daemon| async move {
        daemon.say(SayRequest { say }).await?;
        Ok(())
    });
    if let (Some(why), true) = (untold, asked == ExitCode::SUCCESS) {
        eprintln!("luneta: said from no pane, as it cannot tell the one it runs in: {why}");
    }
    asked
}

fn from_stdin() -> Result<String, String> {
    let limit = BYTES_MAX as u64 + 1;
    let mut read = Vec::new();
    let got = io::stdin().take(limit).read_to_end(&mut read);
    got.map_err(|error| format!("stdin does not read: {error}"))?;
    Ok(String::from_utf8_lossy(&read).into_owned())
}

fn asker_of(seat: Seat) -> Result<Option<Asker>, String> {
    let asker = match seat {
        Seat::Opened(ZellijPane { session, pane }) => {
            let named = SessionName::running(&session)
                .map_err(|why| format!("session {session:?}: {why}"))?;
            Asker::Zellij { named, pane }
        },
        Seat::Asked { user: User::Pane { session, pane }, .. } => {
            Asker::Zellij { named: session, pane }
        },
        Seat::Own(pane) | Seat::Asked { user: User::Own { pane, .. }, .. } => Asker::Own(pane),
        Seat::Asked { user: User::Away | User::Untold, .. } => return Ok(None),
    };
    Ok(Some(asker))
}

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
            None | Some(Reached::Reaching(())) => after = host.version,
            reached => return Ok(reached),
        }
    }
}

pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    let built = tokio::runtime::Builder::new_current_thread().enable_all().build();
    built.map_err(|error| format!("no runtime: {error}"))
}

pub fn listed_agents(folder: &Path) -> Result<ListAgentsResponse, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut daemon = daemon::running(folder).await?;
        let listed = daemon.list_agents(()).await;
        listed.map(tonic::Response::into_inner).map_err(|error| causes(&error))
    })
}

pub fn occupied(folder: &Path) -> Result<Occupied, String> {
    Occupied::of(&host(folder)?)
}

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

async fn agents(
    mut daemon: DaemonClient<Channel>,
    own: Result<(Vec<Listing>, Vec<String>), Failure>,
) -> Result<(), Box<dyn Error>> {
    let listed = daemon.list_agents(()).await?.into_inner();
    let (own, left_out) = own.unwrap_or_else(|failure| {
        let why = "own sessions go by their ids, as luneta-mux cannot list them";
        eprintln!("luneta: {why}: {failure}");
        (Vec::new(), Vec::new())
    });
    for why in left_out {
        let left = "luneta-mux lists a session that does not read, whose agents go by its id";
        eprintln!("luneta: {left}: {why}");
    }
    let seated = |agent: &Agent| {
        seat_of(agent, &own).unwrap_or_else(|(seat, why)| {
            eprintln!("luneta: lunetad lists an agent in {why}");
            seat
        })
    };
    let mut rows: Vec<(String, &Agent)> =
        listed.agents.iter().map(|agent| (seated(agent), agent)).collect();
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

pub fn seat_of(agent: &Agent, own: &[Listing]) -> Result<String, (String, String)> {
    match &agent.pane {
        Some(agent::Pane::Own(pane)) => {
            OwnPane::of(pane).map(|read| own_seat(read, own)).map_err(|why| {
                let session = pane.session.and_then(|session| OwnId::new(session.id));
                (unnamed_own(session, NonZeroU32::new(pane.pane)), why)
            })
        },
        Some(agent::Pane::Zellij(_)) | None => Ok(seat(agent.zellij())),
    }
}

pub fn own_seat(pane: OwnPane, own: &[Listing]) -> String {
    match own.iter().find(|listed| listed.key() == pane.session) {
        Some(named) => format!("{}:{}", named.name, pane.id),
        None => unnamed_own(Some(pane.session.id), Some(pane.id)),
    }
}

fn unnamed_own(session: Option<OwnId>, pane: Option<NonZeroU32>) -> String {
    let id = session.map_or_else(|| "?".to_owned(), |id| id.to_string());
    let pane = pane.map_or_else(|| "?".to_owned(), |id| id.to_string());
    let seat = format!("?:{pane} (own {id})");
    assert!(zellij::pane_address(&seat).is_err(), "{seat} reads as a pane");
    seat
}

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

pub fn line(seat: &str, width: usize, agent: &Agent) -> String {
    let status = match &agent.status {
        Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
            Ok(Status::Busy) => "busy",
            Ok(Status::Idle) => "idle",
            Ok(Status::Waiting) => "waiting",
            Ok(Status::Shell) => "shell",
            Ok(Status::Unspecified) | Err(_) => "?",
        },
        Some(Reported::OtherStatus(word)) => word.as_str(),
        None => "?",
    };
    let label = agent.name.as_ref().map_or(agent.cwd.as_str(), |name| name.text.as_str());
    format!("{seat:<width$}  {status:<7}  {:>7}  {label}", agent.pid)
}

pub fn unread_line(seat: &str, width: usize, file: &UnreadFile) -> String {
    format!("{seat:<width$}  {:<7}  {:>7}  its session file does not read", "?", file.pid)
}

pub fn unread(unread: Unread) -> String {
    format!("{unread}, and its log says which")
}

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
            None => println!("{machine}/  (does not read)"),
        }
    }
    if host.unread_hosts.is_some_and(|unread| unread > 0) {
        println!("(lunetad could not read all of luneta's hosts file: its log says why)");
    }
    Ok(())
}

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

fn span(secs: u64) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m"),
    }
}
