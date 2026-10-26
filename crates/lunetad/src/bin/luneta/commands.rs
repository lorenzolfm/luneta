//! What the CLI asks lunetad, the daemon of this host.

use std::error::Error;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Subcommand;
use lunetad::api::agent::Status as Reported;
use lunetad::api::checkout::Head;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::work_tree::Stands;
use lunetad::api::{
    Agent, InfoRequest, ListAgentsRequest, Status, Version, WatchRequest, WorkTree,
};
use lunetad::folder::SOCKET;
use prost::Message as _;
use tonic::transport::Channel;

use crate::daemon::{self, Reach};

/// What the CLI asks lunetad. Only `watch` starts one where none runs, as it
/// is what luneta's zellij plugin asks: the rest say that none does.
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
    /// where none runs, and again where one of another version does.
    Watch {
        /// The host as last seen, `<pid>:<seq>` from its version, to wait for
        /// the next. Without it, the host is written out at once.
        #[arg(long, value_parser = version)]
        after: Option<Version>,
    },
}

/// A version as `--after` spells it: `<pid>:<seq>`.
fn version(spelled: &str) -> Result<Version, String> {
    let (pid, seq) = spelled.split_once(':').ok_or("not <pid>:<seq>")?;
    let pid = pid.parse().map_err(|error| format!("the pid: {error}"))?;
    let seq = seq.parse().map_err(|error| format!("the seq: {error}"))?;
    Ok(Version { pid, seq })
}

/// Asks the daemon in `folder` what `command` asks.
pub fn run(command: &Command, folder: &Path) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("luneta: no runtime: {error}");
            return ExitCode::FAILURE;
        },
    };
    let wanted = match command {
        Command::Info | Command::Agents | Command::Sessions => Reach::Running,
        Command::Watch { .. } => Reach::Current,
    };
    let daemon = match runtime.block_on(daemon::reach(folder, wanted)) {
        Ok(daemon) => daemon,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let done = match command {
        Command::Info => runtime.block_on(info(daemon)),
        Command::Agents => runtime.block_on(agents(daemon)),
        Command::Sessions => runtime.block_on(sessions(daemon)),
        Command::Watch { after } => runtime.block_on(watch(daemon, *after)),
    };
    if let Err(error) = done {
        eprintln!("luneta: {}: {}", folder.join(SOCKET).display(), causes(&*error));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
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

/// One line to an agent, by its session and pane: the pane, its status, its pid,
/// and its name or else where it works. An agent outside zellij has `-` for a
/// pane. One whose session was renamed says what it was named, and one whose
/// session the daemon cannot tell has `?` for one. A status the CLI does not
/// know is the word Claude Code wrote, and none at all is `?`.
async fn agents(mut daemon: DaemonClient<Channel>) -> Result<(), Box<dyn Error>> {
    let listed = daemon.list_agents(ListAgentsRequest {}).await?.into_inner();
    let pane = |agent: &Agent| match &agent.zellij {
        None => "-".to_owned(),
        Some(zellij) => match zellij.session.as_deref() {
            None => format!("?:{} (named {})", zellij.pane, zellij.named),
            Some(now) if now != zellij.named => {
                format!("{now}:{} (named {})", zellij.pane, zellij.named)
            },
            Some(now) => format!("{now}:{}", zellij.pane),
        },
    };
    let mut rows: Vec<(String, &Agent)> =
        listed.agents.iter().map(|agent| (pane(agent), agent)).collect();
    rows.sort_by(|(a, _), (b, _)| a.cmp(b));
    let width = rows.iter().map(|(pane, _)| pane.len()).max().unwrap_or(0);
    for (pane, agent) in rows {
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
        println!("{pane:<width$}  {status:<7}  {:>7}  {label}", agent.pid);
    }
    if listed.unread > 0 {
        eprintln!(
            "luneta: lunetad cannot read {} of Claude Code's session files, and its log says which",
            listed.unread
        );
    }
    Ok(())
}

/// A session to a line, with its panes under it, each by its tab and its id:
/// where it works, then what is checked out there and what changed on it. A
/// pane zellij says no directory of has `-` for one, and a directory git has
/// said nothing of yet has nothing after it.
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
            println!("  {at}  {cwd}  {stands}");
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
