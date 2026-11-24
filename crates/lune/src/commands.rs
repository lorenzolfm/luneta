use std::error::Error;
use std::future::Future;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::Subcommand;
use lune_protocol::unread::Unread;
use luned::api::agent::Status as Reported;
use luned::api::projects::Held;
use luned::api::remote::Reached;
use luned::api::say::Does;
use luned::api::{
    Agent, ListAgentsResponse, Say, SayRequest, Status, Utterance, Version, WatchRequest,
};
use luned::folder::SOCKET;
use luned::mux::TerminalKey;
pub use luned::log::causes;
use luned::prunes::Occupied;
use luned::sayings::BYTES_MAX;
use luned::{release, Daemon, Destination};
use prost::Message as _;
use tokio::runtime::Handle;

use crate::agreed::{self, Agreed};
use crate::bridge;
use crate::daemon::{self, Unreached};
use crate::machines::{self, Added};
use crate::prune;
use crate::rebase;
use crate::start;
use crate::terminate;

#[derive(Subcommand)]
pub enum Command {
    #[command(
        about = "Say which luned runs, and for how long it has, and whether it is another build \
                 than this lune. Where none runs, it says so and exits 5"
    )]
    Info,
    #[command(
        about = "List the agents that run, and the terminal each is in. Where no luned runs, it \
                 says so and exits 5"
    )]
    Agents,
    #[command(
        about = "Wait for the host to change, and write it out as the protocol encodes it, for a \
                 program to read. luned is started where none runs, and again where an older \
                 one does. A luned of another build that it does not replace is refused"
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
        about = "Hand on what comes on stdin to luned's socket, and what comes on the socket to \
                 stdout, until either end hangs up, for a client on another host to ask it \
                 through ssh. luned is started where none runs, and again where an older one \
                 does, a luned of another build that it does not replace is refused, and once \
                 it answers, `luned` and a newline go to stdout before anything it says"
    )]
    Bridge,
    #[command(
        about = "Have luned watch another machine through ssh, as the sidebar's `n m` does: add \
                 it to lune's hosts file, and say how the first ssh to it goes, where luned \
                 runs. luned's ssh asks you nothing, so a machine whose host key ssh has yet to \
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
        about = "Start a worktree, and say its folder. A folder opens a worktree there, in \
                 the project it is in, or in a project made of it, named after it. A branch has \
                 a worktree made of it, as `wt add` does, in the clone wt made that this runs \
                 in. An issue has a branch and a worktree made for it, as a pasted link has, and \
                 an agent started there on it. A pull request has a worktree made of it. Run in \
                 a terminal of lune's, it takes the lune on that terminal to the worktree"
    )]
    Start {
        #[arg(
            value_name = "WHAT",
            help = "A folder, a branch, an issue as `#<number>` or by its link, or the link of a \
                    pull request, on GitHub, with or without its `https://`. A folder of the \
                    name goes before a branch of it"
        )]
        what: String,
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
            value_parser = agreed::agreed,
            help = "Delete them, while what would go is still the plan said as `PLAN`"
        )]
        yes: Option<Agreed>,
    },
    #[command(
        about = "Rebase the branch of a worktree onto upstream, as the menu's `g r` does: onto \
                 the default branch of the repository's `upstream`, `github` or `origin` \
                 remote, fetched first, or else its own main or master. It pushes nothing. A \
                 rebase that stops on conflicts is left stopped, an agent is started on them in \
                 a view of the worktree, and it exits 4, as it does when it cannot tell how \
                 the rebase ended. Where it rebased nothing, it exits 1"
    )]
    Rebase {
        #[arg(value_name = "DIR", help = "A directory of the worktree. Without one, this one")]
        dir: Option<PathBuf>,
    },
    #[command(
        about = "Reset lune: end every lune, luned and lune-mux, on this machine and on \
                 each machine of lune's hosts file, and the shells and agents in lune-mux's \
                 terminals with them, and forget every project and worktree, so that the next \
                 `lune` starts with none. lune's hosts file, its logs and the rest of its \
                 state stay. A machine ssh does not reach is skipped, and keeps its own. Without \
                 `--yes`, say what would end on each machine, and the plan to tell `--yes`, and \
                 end nothing. Run in a terminal of lune's, it ends with lune"
    )]
    Terminate {
        #[arg(
            long,
            value_name = "PLAN",
            value_parser = agreed::agreed,
            conflicts_with_all = ["here", "ending"],
            help = "End it all, while what would end is still the plan said as `PLAN`"
        )]
        yes: Option<Agreed>,
        #[arg(
            long,
            hide = true,
            help = "On this machine alone, and for the lune that asks it through ssh: say \
                    what would end here, as a key and then in words, and end nothing"
        )]
        here: bool,
        #[arg(
            long,
            hide = true,
            value_name = "KEY",
            value_parser = terminate::key,
            requires = "here",
            help = "End it all here at once, while what would end here is still what `--here` \
                    said as `KEY`, and say what ended; refused in a terminal of lune's"
        )]
        ending: Option<terminate::Key>,
    },
    #[command(
        about = "Read text aloud on the machine you sit at, whichever machine this runs on: queue \
                 it, after what waits to be read, and return once luned has it, without waiting \
                 for it to be read. It fails at once where no luned runs, and exits 5"
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
        Command::Info => ask(folder, daemon::of_any_build(folder), info),
        Command::Agents => ask(folder, daemon::running(folder), agents),
        Command::Watch { after } => {
            let current = async {
                let current = daemon::current(folder).await.map_err(Unreached::Failed)?;
                Ok(current.daemon)
            };
            ask(folder, current, |daemon| watch(daemon, *after))
        },
        Command::Bridge => bridge::run(folder),
        Command::Ssh { destination } => ssh(destination, folder),
        Command::Prune { repo, yes } => prune::run(repo.as_deref(), *yes, folder),
        Command::Rebase { dir } => rebase::run(dir.as_deref(), folder),
        Command::Say { text, now, stop } => say(text.as_deref(), *now, *stop, folder),
        Command::Terminate { yes, here, ending } => terminate::run(*yes, *here, *ending, folder),
        Command::Start { what } => start::run(what, folder),
    }
}

fn ask<R, F, Asked>(folder: &Path, reaching: R, asking: F) -> ExitCode
where
    R: Future<Output = Result<Daemon, Unreached>>,
    F: FnOnce(Daemon) -> Asked,
    Asked: Future<Output = Result<(), Box<dyn Error>>>,
{
    let tasks = match tasks() {
        Ok(tasks) => tasks,
        Err(why) => {
            eprintln!("lune: {why}");
            return ExitCode::FAILURE;
        },
    };
    let daemon = match tasks.block_on(reaching) {
        Ok(daemon) => daemon,
        Err(unreached) => {
            eprintln!("lune: {unreached}");
            return unreached.exit_code();
        },
    };
    if let Err(error) = tasks.block_on(asking(daemon)) {
        eprintln!("lune: {}: {}", folder.join(SOCKET).display(), causes(&*error));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn say(text: Option<&str>, now: bool, stop: bool, folder: &Path) -> ExitCode {
    let does = if stop {
        Does::Stop(())
    } else {
        let text = match text.map_or_else(from_stdin, |text| Ok(text.to_owned())) {
            Ok(text) => text,
            Err(why) => {
                eprintln!("lune: {why}");
                return ExitCode::FAILURE;
            },
        };
        let utterance = Utterance { text };
        if now { Does::Now(utterance) } else { Does::InTurn(utterance) }
    };
    let say = Some(Say { does: Some(does) });
    ask(folder, daemon::running(folder), |mut daemon| async move {
        daemon.say(SayRequest { say }).await?;
        Ok(())
    })
}

fn from_stdin() -> Result<String, String> {
    let limit = BYTES_MAX as u64 + 1;
    let mut read = Vec::new();
    let got = io::stdin().take(limit).read_to_end(&mut read);
    got.map_err(|error| format!("stdin does not read: {error}"))?;
    Ok(String::from_utf8_lossy(&read).into_owned())
}

fn ssh(destination: &Destination, folder: &Path) -> ExitCode {
    match machines::add(destination) {
        Ok(Added::Named) => println!("{destination}: added to lune's hosts file"),
        Ok(Added::Already) => println!("{destination}: lune's hosts file names it already"),
        Err(why) => {
            eprintln!("lune: {why}");
            return ExitCode::FAILURE;
        },
    }
    let tasks = match tasks() {
        Ok(tasks) => tasks,
        Err(why) => {
            eprintln!("lune: {why}");
            return ExitCode::FAILURE;
        },
    };
    let daemon = match tasks.block_on(daemon::running(folder)) {
        Ok(daemon) => daemon,
        Err(why) => {
            println!("{destination}: luned reaches it once it runs: {why}");
            return ExitCode::SUCCESS;
        },
    };
    match tasks.block_on(first_reach(daemon, destination)) {
        Ok(Some(Reached::Host(host))) => {
            match host.projects.and_then(|projects| projects.held) {
                Some(Held::Kept(kept)) => match kept.projects.len() {
                    1 => println!("{destination}: reached, 1 project"),
                    n => println!("{destination}: reached, {n} projects"),
                },
                Some(Held::Unkept(_)) | None => println!("{destination}: reached"),
            }
            ExitCode::SUCCESS
        },
        Ok(Some(Reached::Unreached(unreached))) => {
            eprintln!("lune: {destination}: unreached: {}", unreached.why);
            if let Some(said) = &unreached.ssh_said {
                eprintln!("lune: ssh said \"{said}\"");
            }
            ExitCode::FAILURE
        },
        Ok(Some(Reached::Here(()))) => {
            eprintln!("lune: {destination}: {}", luned::THIS_MACHINE);
            ExitCode::FAILURE
        },
        Ok(Some(Reached::Reaching(())) | None) => {
            println!("{destination}: luned reaches it still; the sidebar says how it went");
            ExitCode::SUCCESS
        },
        Err(error) => {
            eprintln!("lune: {}: {}", folder.join(SOCKET).display(), causes(&*error));
            ExitCode::FAILURE
        },
    }
}

async fn first_reach(
    mut daemon: Daemon,
    destination: &Destination,
) -> Result<Option<Reached>, Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + luned::REACH + Duration::from_secs(5);
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

pub fn tasks() -> Result<Handle, String> {
    luned::tasks::start()?;
    Ok(luned::tasks::shared())
}

pub fn listed_agents(folder: &Path) -> Result<ListAgentsResponse, String> {
    let tasks = tasks()?;
    tasks.block_on(async {
        let mut daemon = daemon::running(folder).await?;
        let listed = daemon.list_agents(()).await;
        listed.map(tonic::Response::into_inner).map_err(|error| causes(&error))
    })
}

pub fn occupied(folder: &Path) -> Result<Occupied, String> {
    Occupied::of(&host(folder)?)
}

pub fn host(folder: &Path) -> Result<luned::api::Host, String> {
    let tasks = tasks()?;
    tasks.block_on(async {
        let mut daemon = daemon::running(folder).await?;
        let asked = daemon.watch(WatchRequest { after: None, kept: None, shown: false }).await;
        Ok(asked.map_err(|error| causes(&error))?.into_inner())
    })
}

async fn info(mut daemon: Daemon) -> Result<(), Box<dyn Error>> {
    let info = daemon.info(()).await?.into_inner();
    let started = info.started_at.and_then(|at| SystemTime::try_from(at).ok());
    let build = release::spelled(&info.version, info.built);
    match started.and_then(|at| at.elapsed().ok()) {
        Some(up) => println!("luned {build} (pid {}), up {}", info.pid, span(up.as_secs())),
        None => println!("luned {build} (pid {})", info.pid),
    }
    if let release::Of::Other(other) = release::of(&info.version, info.built) {
        eprintln!("lune: {}", daemon::unmatched(&other, info.pid));
    }
    Ok(())
}

async fn agents(mut daemon: Daemon) -> Result<(), Box<dyn Error>> {
    let listed = daemon.list_agents(()).await?.into_inner();
    let seated = |agent: &Agent| {
        seat_of(agent).unwrap_or_else(|(seat, why)| {
            eprintln!("lune: luned lists an agent in {why}");
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
        eprintln!("lune: {}", unread(count));
    }
    Ok(())
}

fn seat_of(agent: &Agent) -> Result<String, (String, String)> {
    match &agent.terminal {
        Some(key) => TerminalKey::of(key)
            .map(terminal_seat)
            .map_err(|why| (format!("terminal {} (unread)", key.id), why)),
        None => Ok("-".to_owned()),
    }
}

fn terminal_seat(key: TerminalKey) -> String {
    format!("terminal {}", key.id)
}

fn line(seat: &str, width: usize, agent: &Agent) -> String {
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

pub fn unread(unread: Unread) -> String {
    format!("{unread}, and its log says which")
}

async fn watch(
    mut daemon: Daemon,
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
