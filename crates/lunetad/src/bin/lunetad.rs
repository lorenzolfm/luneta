//! lunetad, the luneta daemon of a host. It listens where `luneta` looks for
//! it, which the variables of the environment say, until zellij has had no
//! session for a while. `luneta` starts it, detached, when none answers.

use std::fs;
use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::process::ExitCode;
use std::sync::LazyLock;

use clap::Parser;
use lunetad::{folder, release, Claim, Refusal};
use rustix::io::FdFlags;

/// This build, as `--version` says it after the name: luneta asks it of the
/// lunetad it would start, to tell whether that one is newer than the daemon
/// that runs.
static SAID: LazyLock<String> = LazyLock::new(|| release::spelled(release::VERSION, release::BUILT));

/// The luneta daemon of this host, for this user.
#[derive(Parser)]
#[command(version = SAID.as_str())]
struct Args {
    /// Run in a session of its own, away from the terminal of whatever
    /// started it, as luneta starts it: so that the terminal closing does
    /// not end it. What it was handed open past its stdio, it closes.
    #[arg(long)]
    detached: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    // Before anything of lunetad's own is open, which it would close too.
    if args.detached
        && let Err(error) = close_inherited()
    {
        eprintln!("lunetad: what it was handed open is not closed: {error}");
    }
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

/// Closes the files lunetad was handed open past its stdio. luneta hands on
/// whatever was open to it, and what runs luneta may leave more open: zellij's
/// server leaves each pane's terminal open to the commands its plugins run. A
/// daemon that held a pane's terminal would keep the kernel from hanging it
/// up: once a server ended without a word to its panes, as one that crashes
/// does, what ran in them would run on for as long as the daemon did.
///
/// A file comes through an exec only where it lacks close-on-exec, which
/// what std opens has, so that these are all that are closed.
#[expect(unsafe_code, reason = "a file lunetad was handed by its number has no safe close")]
fn close_inherited() -> io::Result<()> {
    // The files of the process that lists them.
    let open = if cfg!(target_os = "linux") { "/proc/self/fd" } else { "/dev/fd" };
    let mut listing = fs::read_dir(open)?;
    let mut fds: Vec<RawFd> = Vec::new();
    for entry in listing.by_ref() {
        if let Ok(fd) = entry?.file_name().to_string_lossy().parse() {
            fds.push(fd);
        }
    }
    for fd in fds.into_iter().filter(|fd| *fd > 2) {
        // SAFETY: lunetad has no thread but this one yet, and `listing`,
        // which listed `fd`, is still open, so that nothing has closed `fd`
        // since. One without close-on-exec came through the exec, and so
        // nothing of lunetad's holds it, to use it once it is closed.
        unsafe {
            let flags = rustix::io::fcntl_getfd(BorrowedFd::borrow_raw(fd))?;
            if !flags.contains(FdFlags::CLOEXEC) {
                rustix::io::close(fd);
            }
        }
    }
    drop(listing);
    Ok(())
}
