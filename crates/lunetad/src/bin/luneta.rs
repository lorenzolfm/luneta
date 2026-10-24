//! luneta, the CLI that talks to the daemon of this host.

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use lunetad::api::{Agent, InfoRequest, ListAgentsRequest, Status};
use lunetad::folder::{self, SOCKET};

/// Talk to lunetad, the luneta daemon of this host.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Say which lunetad runs, and for how long it has.
    Info,
    /// List the agents that run, and the pane each is in.
    Agents,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(folder) = folder::find() else {
        eprintln!("luneta: neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in");
        return ExitCode::FAILURE;
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("luneta: no runtime: {error}");
            return ExitCode::FAILURE;
        },
    };
    let socket = folder.join(SOCKET);
    let done = match cli.command {
        Command::Info => runtime.block_on(info(&socket)),
        Command::Agents => runtime.block_on(agents(&socket)),
    };
    if let Err(error) = done {
        eprintln!("luneta: {}: {}", socket.display(), causes(&*error));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn info(socket: &Path) -> Result<(), Box<dyn Error>> {
    let mut daemon = lunetad::connect(socket).await?;
    let info = daemon.info(InfoRequest {}).await?.into_inner();
    let started = info.started_at.and_then(|at| SystemTime::try_from(at).ok());
    match started.and_then(|at| at.elapsed().ok()) {
        Some(up) => println!("lunetad {} (pid {}), up {}", info.version, info.pid, span(up.as_secs())),
        None => println!("lunetad {} (pid {})", info.version, info.pid),
    }
    Ok(())
}

/// One line to an agent, by its session and pane: the pane, its status, its pid,
/// and its name or else where it works. An agent outside zellij has `-` for a
/// pane. One whose session was renamed says what it was named, and one whose
/// session the daemon cannot find has `?` for one.
async fn agents(socket: &Path) -> Result<(), Box<dyn Error>> {
    let mut daemon = lunetad::connect(socket).await?;
    let agents = daemon.list_agents(ListAgentsRequest {}).await?.into_inner().agents;
    let pane = |agent: &Agent| match &agent.zellij {
        None => "-".to_owned(),
        Some(zellij) if zellij.session.is_empty() => {
            format!("?:{} ({} is gone)", zellij.pane, zellij.named)
        },
        Some(zellij) if zellij.session != zellij.named => {
            format!("{}:{} (named {})", zellij.session, zellij.pane, zellij.named)
        },
        Some(zellij) => format!("{}:{}", zellij.session, zellij.pane),
    };
    let mut rows: Vec<(String, &Agent)> = agents.iter().map(|agent| (pane(agent), agent)).collect();
    rows.sort_by(|(a, _), (b, _)| a.cmp(b));
    let width = rows.iter().map(|(pane, _)| pane.len()).max().unwrap_or(0);
    for (pane, agent) in rows {
        let status = match agent.status() {
            Status::Busy => "busy",
            Status::Idle => "idle",
            Status::Waiting => "waiting",
            Status::Unspecified => "?",
        };
        let label = agent.name.as_ref().map_or(agent.cwd.as_str(), |name| name.text.as_str());
        println!("{pane:<width$}  {status:<7}  {:>7}  {label}", agent.pid);
    }
    Ok(())
}

/// An error and what it came of, on one line. tonic's own says only
/// "transport error", and why is further down. An error that says what it
/// came of already is not said twice.
fn causes(error: &dyn Error) -> String {
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
