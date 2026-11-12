//! `luneta-mux`: luneta's own multiplexer, one server for every own session of
//! the user. It listens in luneta's runtime folder until its last session
//! ends, and ends with `folder::ANOTHER_RUNS` where another server holds the
//! folder already.

use std::process::ExitCode;

use luneta_mux::{folder, serve};

fn main() -> ExitCode {
    if let Err(error) = rustix::process::setsid()
        && error != rustix::io::Errno::PERM
    {
        eprintln!("luneta-mux: no session of its own: {error}");
        return ExitCode::FAILURE;
    }
    let Some(path) = folder::find() else {
        eprintln!("luneta-mux: neither XDG_RUNTIME_DIR nor TMPDIR is set");
        return ExitCode::FAILURE;
    };
    let claim = match folder::claim(&path) {
        Ok(claim) => claim,
        Err(refusal @ folder::Refusal::Running(_)) => {
            eprintln!("luneta-mux: {refusal}");
            return ExitCode::from(folder::ANOTHER_RUNS);
        },
        Err(refusal) => {
            eprintln!("luneta-mux: {refusal}");
            return ExitCode::FAILURE;
        },
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("luneta-mux: {error}");
            return ExitCode::FAILURE;
        },
    };
    match runtime.block_on(serve::serve(claim)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("luneta-mux: {error}");
            ExitCode::FAILURE
        },
    }
}
