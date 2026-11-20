use std::process::ExitCode;

use luneta_mux::{folder, panics, serve};

fn main() -> ExitCode {
    panics::abort_on_panic();
    if let Err(error) = rustix::process::setsid()
        && error != rustix::io::Errno::PERM
    {
        eprintln!("luneta-mux: no session of its own: {error}");
        return ExitCode::FAILURE;
    }
    let mut arguments = std::env::args_os().skip(1);
    let (Some(path), None) = (arguments.next(), arguments.next()) else {
        eprintln!("luneta-mux: give it the one folder to listen in, as lunetad does");
        return ExitCode::FAILURE;
    };
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        eprintln!("luneta-mux: {} is no folder to listen in, as it is not absolute", path.display());
        return ExitCode::FAILURE;
    }
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
