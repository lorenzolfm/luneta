//! What the CLI asks lunetad, the daemon of this host, and what it has zellij
//! do, as `drive` does it.

use std::error::Error;
use std::future::Future;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Subcommand;
use luneta_protocol::github::{self, State};
use luneta_protocol::unread::Unread;
use lunetad::api::agent::Status as Reported;
use lunetad::api::checkout::Head;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::work_tree::Stands;
use lunetad::api::{
    Agent, InfoRequest, ListAgentsRequest, ListAgentsResponse, PullRequest, Status, UnreadFile,
    Version, WatchRequest, WorkTree, Zellij,
};
use lunetad::folder::SOCKET;
use prost::Message as _;
use tonic::transport::Channel;

use crate::daemon::{self, Reach};
use crate::drive::{self, Address, Agreed, Opening};

/// What the CLI asks lunetad, and what it has zellij do. Only `watch` starts
/// a lunetad where none runs, as it is what luneta's zellij plugin asks: the
/// rest say that none does, or, for `close`, that it cannot tell the agents.
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
    /// it, for a program to read: luneta's zellij plugin. lunetad is started
    /// where none runs, and again where an older one does.
    Watch {
        /// The host as last seen, `<pid>:<seq>` from its version, to wait for
        /// the next. Without it, the host is written out at once.
        #[arg(long, value_parser = version)]
        after: Option<Version>,
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
        Command::Info => ask(folder, Reach::Running, info),
        Command::Agents => ask(folder, Reach::Running, agents),
        Command::Sessions => ask(folder, Reach::Running, sessions),
        Command::Watch { after } => ask(folder, Reach::Current, |daemon| watch(daemon, *after)),
        Command::Here => drive::here(folder),
        Command::Open { what } => drive::open(what, folder),
        Command::Go { to } => drive::go(to, folder),
        Command::Close { what, yes } => drive::close(what, *yes, folder),
    }
}

/// Reaches the daemon in `folder` as `wanted`, and has `asking` ask it.
fn ask<F, Asked>(folder: &Path, wanted: Reach, asking: F) -> ExitCode
where
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
    let daemon = match runtime.block_on(daemon::reach(folder, wanted)) {
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

/// A runtime for the CLI's one thread, or why there is none.
fn runtime() -> Result<tokio::runtime::Runtime, String> {
    let built = tokio::runtime::Builder::new_current_thread().enable_all().build();
    built.map_err(|error| format!("no runtime: {error}"))
}

/// The agents lunetad lists, for the CLI to say which a close ends, or why
/// it does not list them: none runs, or it does not answer. It starts none,
/// as `agents` does not.
pub fn listed_agents(folder: &Path) -> Result<ListAgentsResponse, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut daemon = daemon::reach(folder, Reach::Running).await?;
        let listed = daemon.list_agents(ListAgentsRequest {}).await;
        listed.map(tonic::Response::into_inner).map_err(|error| causes(&error))
    })
}

async fn info(mut daemon: DaemonClient<Channel>) -> Result<(), Box<dyn Error>> {
    let info = daemon.info(InfoRequest {}).await?.into_inner();
    let started = info.started_at.and_then(|at| SystemTime::try_from(at).ok());
    match started.and_then(|at| at.elapsed().ok()) {
        Some(up) => {
            println!("lunetad {} (pid {}), up {}", info.version, info.pid, span(up.as_secs()));
        },
        None => println!("lunetad {} (pid {})", info.version, info.pid),
    }
    Ok(())
}

/// One line to an agent, by its session and pane, as `line` writes it.
async fn agents(mut daemon: DaemonClient<Channel>) -> Result<(), Box<dyn Error>> {
    let listed = daemon.list_agents(ListAgentsRequest {}).await?.into_inner();
    let mut rows: Vec<(String, &Agent)> =
        listed.agents.iter().map(|agent| (seat(agent.zellij.as_ref()), agent)).collect();
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

/// A session to a line, with its panes under it, each by its tab and its id:
/// where it works, then what is checked out there and what changed on it, and
/// the pull request of its branch. A pane zellij says no directory of has `-`
/// for one, and a directory git has said nothing of yet has nothing after it.
async fn sessions(mut daemon: DaemonClient<Channel>) -> Result<(), Box<dyn Error>> {
    let mut host = daemon.watch(WatchRequest { after: None }).await?.into_inner();
    host.sessions.sort_by(|a, b| a.name.cmp(&b.name));
    for session in &host.sessions {
        let Some(panes) = &session.panes else {
            println!("{}  (no panes listed yet)", session.name);
            continue;
        };
        println!("{}", session.name);
        for pane in &panes.panes {
            let at = format!("{}:{}", pane.tab_name, pane.id);
            let Some(cwd) = pane.cwd.as_deref() else {
                println!("  {at}  -");
                continue;
            };
            let Some(tree) = host.work_trees.iter().find(|tree| tree.dir == cwd) else {
                println!("  {at}  {cwd}");
                continue;
            };
            let stands = stands(tree).unwrap_or_else(|| "(does not read)".to_owned());
            let pull_request = host.pull_requests.iter().find(|it| it.dir == cwd);
            let pulled = pull_request.map(|it| {
                format!("  {}", pulled(it).unwrap_or_else(|| "(does not read)".to_owned()))
            });
            let pulled = pulled.unwrap_or_default();
            println!("  {at}  {cwd}  {stands}{pulled}");
        }
    }
    Ok(())
}

/// How a work tree stands, in words: its branch, or `detached`, and then how
/// many paths of each kind changed, the stashes, and where the branch is from
/// its upstream. A directory in no repository says so. None for an answer
/// that leaves out what the protocol says is always there, as a newer
/// daemon's could.
fn stands(tree: &WorkTree) -> Option<String> {
    let checkout = match tree.stands.as_ref()? {
        Stands::Outside(_) => return Some("no repository".to_owned()),
        Stands::Checkout(checkout) => checkout,
    };
    let head = checkout.head.as_ref()?;
    let changes = checkout.changes?;
    let mut said = vec![match head {
        Head::Detached(_) => "detached".to_owned(),
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

/// A pull request, in words: its number and its state. None for one that
/// does not read, as a newer daemon's could send.
fn pulled(pull_request: &PullRequest) -> Option<String> {
    let github::PullRequest { number, state, .. } = github::PullRequest::of_api(pull_request)?;
    let state = match state {
        State::Open => "open",
        State::Draft => "draft",
        State::Merged => "merged",
        State::Closed => "closed",
    };
    Some(format!("#{number} {state}"))
}

/// Writes out the host once it is no longer as `after` saw it.
async fn watch(
    mut daemon: DaemonClient<Channel>,
    after: Option<Version>,
) -> Result<(), Box<dyn Error>> {
    let host = daemon.watch(WatchRequest { after }).await?.into_inner();
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&host.encode_to_vec())?;
    stdout.flush()?;
    Ok(())
}

/// An error and what it came of, on one line. tonic's own says only
/// "transport error", and why is further down. An error that says what it
/// came of already is not said twice.
pub fn causes(error: &dyn Error) -> String {
    let mut line = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        let said = error.to_string();
        if !line.ends_with(&said) {
            line.push_str(": ");
            line.push_str(&said);
        }
        cause = error.source();
    }
    line
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
