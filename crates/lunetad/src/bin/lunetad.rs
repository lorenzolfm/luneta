//! lunetad, the luneta daemon of a host. It listens where `luneta` looks for
//! it, which the variables of the environment say, until zellij has had no
//! session for a while. `luneta` starts it, detached, when none answers.

use std::process::ExitCode;

use clap::Parser;
use lunetad::{folder, Claim, Refusal};

/// The luneta daemon of this host, for this user.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Run in a session of its own, away from the terminal of whatever
    /// started it, as luneta starts it: so that the terminal closing does
    /// not end it.
    #[arg(long)]
    detached: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    // A shell that runs it as a job has made it the leader of a group of its
    // own, which cannot leave for a session.
    if args.detached
        && let Err(error) = rustix::process::setsid()
    {
        eprintln!("lunetad: no session of its own: {error}");
        return ExitCode::FAILURE;
    }
    let Some(folder) = folder::find() else {
        eprintln!("lunetad: neither XDG_RUNTIME_DIR nor TMPDIR is set, to listen in");
        return ExitCode::FAILURE;
    };
    let claim = match Claim::take(&folder) {
        Ok(claim) => claim,
        Err(refusal) => {
            eprintln!("lunetad: {refusal}");
            return match refusal {
                Refusal::Running(_) => ExitCode::from(folder::ANOTHER_RUNS),
                Refusal::NotYours(_) | Refusal::Open(_) | Refusal::Io(..) => ExitCode::FAILURE,
            };
        },
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("lunetad: no runtime: {error}");
            return ExitCode::FAILURE;
        },
    };
    let socket = claim.socket().to_owned();
    if let Err(error) = runtime.block_on(lunetad::serve(claim)) {
        eprintln!("lunetad: {}: {error}", socket.display());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
