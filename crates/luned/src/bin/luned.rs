use std::fs;
use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::process::ExitCode;

use clap::Parser;
use luned::log::{self, Process};
use luned::projects::{self, Unheld};
use luned::{folder, panics, release, tasks, Claim, Refusal};
use rustix::io::FdFlags;

#[derive(Parser)]
#[command(
    version = release::SAID.as_str(),
    about = "The lune daemon of this host, for this user"
)]
struct Args {
    #[arg(
        long,
        help = "Run in a session of its own, away from the terminal of whatever started it, as \
                lune starts it: so that the terminal closing does not end it. What it was \
                handed open past its stdio, it closes"
    )]
    detached: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let unkept = log::start(Process::Luned).err();
    let hooked = panics::abort_on_panic();
    if args.detached
        && let Err(error) = close_inherited()
    {
        log::say(&format!("what it was handed open is not closed: {error}"));
    }
    if args.detached
        && let Err(error) = rustix::process::setsid()
    {
        log::say(&format!("no session of its own: {error}"));
        return ExitCode::FAILURE;
    }
    if let Err(why) = tasks::start() {
        log::say(&why);
        return ExitCode::FAILURE;
    }
    let folder = match folder::find() {
        Ok(folder) => folder,
        Err(unfound) => {
            log::say(&format!("{unfound}, to listen in"));
            return ExitCode::FAILURE;
        },
    };
    let claim = match Claim::take(&folder) {
        Ok(claim) => claim,
        Err(refusal) => {
            log::say(&refusal.to_string());
            return match refusal {
                Refusal::Running(_) => ExitCode::from(folder::ANOTHER_RUNS),
                Refusal::NotYours(_) | Refusal::Open(_) | Refusal::Io(..) => ExitCode::FAILURE,
            };
        },
    };
    let kept = match projects::take(claim.folder()) {
        Ok(lock) => Ok(lock),
        Err(Unheld::Unkept(why)) => Err(why),
        Err(Unheld::Elsewhere(elsewhere)) => {
            log::say(&elsewhere.to_string());
            return ExitCode::FAILURE;
        },
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            log::say(&format!("no runtime: {error}"));
            return ExitCode::FAILURE;
        },
    };
    let socket = claim.socket().to_owned();
    if let Err(error) = runtime.block_on(luned::serve(hooked, claim, kept, unkept)) {
        log::say(&format!("{}: {error}", socket.display()));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[expect(
    unsafe_code,
    reason = "a file luned was handed by its number has no safe close. It is sound here: \
              luned has no thread but this one yet, as asserted, and the listing that named \
              each file is still open, so that nothing has closed one since. One without \
              close-on-exec came through the exec, as std opens all of its own with it, so \
              that nothing of luned's holds it, to use it once it is closed"
)]
fn close_inherited() -> io::Result<()> {
    if cfg!(target_os = "linux") {
        let threads = fs::read_dir("/proc/self/task")?.count();
        assert_eq!(threads, 1, "luned closes what it was handed only while it has one thread");
    }
    let open = if cfg!(target_os = "linux") { "/proc/self/fd" } else { "/dev/fd" };
    let mut listing = fs::read_dir(open)?;
    let mut fds: Vec<RawFd> = Vec::new();
    for entry in listing.by_ref() {
        if let Ok(fd) = entry?.file_name().to_string_lossy().parse() {
            fds.push(fd);
        }
    }
    for fd in fds.into_iter().filter(|fd| *fd > 2) {
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
