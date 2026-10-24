//! lunetad, the luneta daemon of a host. It listens where `luneta` looks for
//! it, which the variables of the environment say, and takes no arguments.

use std::process::ExitCode;

use clap::Parser;
use lunetad::{folder, Claim};

/// The luneta daemon of this host, for this user.
#[derive(Parser)]
#[command(version)]
struct Args {}

fn main() -> ExitCode {
    Args::parse();
    let Some(folder) = folder::find() else {
        eprintln!("lunetad: neither XDG_RUNTIME_DIR nor TMPDIR is set, to listen in");
        return ExitCode::FAILURE;
    };
    let claim = match Claim::take(&folder) {
        Ok(claim) => claim,
        Err(refusal) => {
            eprintln!("lunetad: {refusal}");
            return ExitCode::FAILURE;
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
