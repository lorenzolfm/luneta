use std::fs;
use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use luned::links::{self, Of, Taken};
use luned::log::{self, Process};
use luned::projects::{self, Unheld};
use luned::{folder, release, Claim, Refusal};
use rustix::io::FdFlags;

#[derive(Parser)]
#[command(
    version = release::SAID.as_str(),
    about = "The lune daemon of this host, for this user"
)]
#[command(args_conflicts_with_subcommands = true)]
struct Args {
    #[arg(
        long,
        help = "Run in a session of its own, away from the terminal of whatever started it, as \
                lune starts it: so that the terminal closing does not end it. What it was \
                handed open past its stdio, it closes"
    )]
    detached: bool,
    #[command(subcommand)]
    helper: Option<Helper>,
}

#[derive(Subcommand)]
enum Helper {
    #[command(
        hide = true,
        about = "Make a worktree of a branch, or of a pull request by its link, in a repository \
                 `wt clone` made, as `wt add` does, and print where it is, or where one of the \
                 branch is already. luned makes the worktree of a link pasted in lune's sidebar \
                 so, with wt's code, and no `wt` on the `PATH`, in a process of its own that it \
                 can end. Why it made none comes last on stderr, after `luned: `"
    )]
    Worktree {
        #[arg(long, value_name = "DIR", help = "A directory of the repository")]
        repo: String,
        #[arg(
            long,
            help = "When no worktree has the branch, and a worktree of the repository on another \
                    branch, or on none, is where wt would make one, make none and print that \
                    worktree after `moved off: `"
        )]
        moved_off: bool,
        #[arg(
            value_parser = Of::parse,
            value_name = "BRANCH",
            help = "The branch, or the link of a pull request on GitHub, with or without its \
                    `https://`"
        )]
        of: Of,
    },
    #[command(
        hide = true,
        about = "Bring a worktree to its upstream branch, as `wt follow` does. Why it did not \
                 comes last on stderr, after `luned: `"
    )]
    Follow {
        #[arg(value_name = "DIR")]
        dir: String,
    },
    #[command(
        hide = true,
        about = "Fetch the upstream branch of a branch of the repository a directory is in, as \
                 `wt follow` fetches it, and move no branch. luned has it fetched so, for the \
                 count of how far behind it a branch is. What wt found of it is one word on \
                 stdout; why it did not fetch it comes last on stderr, after `luned: `"
    )]
    FetchUpstream {
        #[arg(value_name = "DIR")]
        dir: String,
        #[arg(value_name = "BRANCH")]
        branch: String,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Some(helper) = args.helper {
        return helped(helper);
    }
    let unkept = log::start(Process::Luned).err();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::panicked(panic);
        default(panic);
    }));
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
    if let Err(error) = runtime.block_on(luned::serve(claim, kept, unkept)) {
        log::say(&format!("{}: {error}", socket.display()));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn helped(helper: Helper) -> ExitCode {
    let said = match helper {
        Helper::Worktree { repo, moved_off, of } => worktree(&repo, &of, moved_off),
        Helper::Follow { dir } => links::followed(&dir).map(|()| None),
        Helper::FetchUpstream { dir, branch } => links::fetched_upstream(&dir, &branch)
            .map(|found| Some(links::upstream_word(found).to_owned())),
    };
    match said {
        Ok(said) => {
            if let Some(said) = said {
                println!("{said}");
            }
            ExitCode::SUCCESS
        },
        Err(why) => {
            eprintln!("{}{why}", links::WHY);
            ExitCode::FAILURE
        },
    }
}

fn worktree(repo: &str, of: &Of, moved_off: bool) -> Result<Option<String>, String> {
    let made = if moved_off {
        links::made_or_moved_off(repo, of)?
    } else {
        Taken::OnBranch(links::made(repo, of)?)
    };
    Ok(Some(match made {
        Taken::OnBranch(folder) => folder.display().to_string(),
        Taken::MovedOff(folder) => format!("{}{}", links::MOVED_OFF, folder.display()),
    }))
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
