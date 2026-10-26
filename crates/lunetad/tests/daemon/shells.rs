//! What the shells with luneta's hook say, as `Watch` gives it on each pane:
//! the line in each shell's file, and what the terminal of a command going
//! says it does.

use std::fs;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use lunetad::api::shell::{Doing, Ended, Going, Stopped};
use lunetad::api::{self, Front};
use prost_types::{Duration, Timestamp};
use rustix::fs::{Mode, OFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, LocalModes, OptionalActions};
use serde_json::json;

use crate::host::{eventually, start, Host};
use crate::sessions::{listed, session};

/// Has the shell of `pane` of `session` that started in the second `started`
/// say `line`, as the hook does: written beside its file, and moved into
/// place. The daemon makes luneta's folder, for itself alone, so it runs
/// first.
fn say(host: &Host, session: &str, pane: u32, started: u64, line: &str) {
    let dir = host.runtime.path().join("luneta/commands").join(session);
    fs::create_dir_all(&dir).unwrap();
    let new = dir.join(format!("{pane}.{started}.new"));
    fs::write(&new, format!("{line}\n")).unwrap();
    fs::rename(new, dir.join(format!("{pane}.{started}"))).unwrap();
}

/// A pane of a shell at its prompt, in the first tab, as zellij lists it.
fn at_home(id: u32) -> serde_json::Value {
    listed(id, "~", (0, "Tab #1"), Some("/home/you"))
}

/// The second it is, as the hook counts it.
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// What the shell of each pane of `name` says, in the order zellij lists
/// them, once the daemon has them.
fn shells(host: &Host, name: &str) -> Option<Vec<Option<api::Shell>>> {
    let panes = session(&host.host(), name)?.panes.clone()?.panes;
    Some(panes.into_iter().map(|pane| pane.shell).collect())
}

fn command(shell: &api::Shell) -> &str {
    match shell.doing.as_ref().unwrap() {
        Doing::Going(going) => &going.command,
        Doing::Ended(ended) => &ended.command,
        Doing::Stopped(stopped) => &stopped.command,
    }
}

/// A terminal of the test's own, which no process runs in front of: what the
/// command going on it does is what its modes say.
struct Terminal {
    _controller: OwnedFd,
    tty: OwnedFd,
    path: PathBuf,
}

impl Terminal {
    fn open() -> Self {
        let controller = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        pty::grantpt(&controller).unwrap();
        pty::unlockpt(&controller).unwrap();
        let path = pty::ptsname(&controller, Vec::new()).unwrap().into_string().unwrap();
        let tty = rustix::fs::open(&path, OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).unwrap();
        Terminal { _controller: controller, tty, path: PathBuf::from(path) }
    }

    /// Has it take keys a line at a time, or a key at a time, with the echo
    /// on or off.
    fn take(&self, lines: bool, echo: bool) {
        let mut modes = termios::tcgetattr(&self.tty).unwrap();
        modes.local_modes.set(LocalModes::ICANON, lines);
        modes.local_modes.set(LocalModes::ECHO, echo);
        termios::tcsetattr(&self.tty, OptionalActions::Now, &modes).unwrap();
    }
}

/// Each pane has what its shell said last: a command it runs, which its
/// terminal says works, asks, or has the terminal to itself, as the terminal
/// comes to say with no word from the shell; a command that ended, and how;
/// a job stopped. A command going on a terminal that is gone is no pane's,
/// and a file the hook is still writing says nothing yet.
#[test]
fn a_pane_has_what_its_shell_said_and_a_command_what_its_terminal_says() {
    let host = Host::new();
    let panes: Vec<_> = (1..=6).map(at_home).collect();
    host.zellij.list("work", &json!(panes));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    let terminal = Terminal::open();
    let started = now();
    let tty = terminal.path.display();
    say(&host, "work", 1, started, &format!("going {started} {tty} cargo build"));
    say(&host, "work", 2, started, &format!("ended {started} 101 41873 cargo test"));
    say(&host, "work", 3, started, &format!("ended {started} - 7120 make"));
    say(&host, "work", 4, started, &format!("stopped {started} nvim"));
    say(&host, "work", 5, started, &format!("going {started} /dev/luneta-gone sleep 30"));
    let writing = host.runtime.path().join(format!("luneta/commands/work/6.{started}.new"));
    fs::write(writing, format!("ended {started} 0 2000 true\n")).unwrap();

    let said = eventually("the shells of work's panes have said", || {
        let shells = shells(&host, "work")?;
        shells[..4].iter().all(Option::is_some).then_some(shells)
    });
    let at = Some(Timestamp { seconds: i64::try_from(started).unwrap(), nanos: 0 });
    let doing = |doing| Some(api::Shell { doing: Some(doing) });
    let going = |front: Front| {
        let command = "cargo build".to_owned();
        doing(Doing::Going(Going { command, since: at, front: front.into() }))
    };
    let took = |seconds, millis: i32| Some(Duration { seconds, nanos: millis * 1_000_000 });
    assert_eq!(
        said,
        [
            going(Front::Working),
            doing(Doing::Ended(Ended {
                command: "cargo test".to_owned(),
                at,
                status: Some(101),
                took: took(41, 873),
            })),
            doing(Doing::Ended(Ended {
                command: "make".to_owned(),
                at,
                status: None,
                took: took(7, 120),
            })),
            doing(Doing::Stopped(Stopped { command: "nvim".to_owned(), at })),
            None,
            None,
        ]
    );

    let modes = [
        (false, false, Front::Interactive),
        (true, false, Front::Asking),
        (true, true, Front::Working),
    ];
    for (lines, echo, front) in modes {
        terminal.take(lines, echo);
        eventually("the terminal says what the command does", || {
            (shells(&host, "work")?[0] == going(front)).then_some(())
        });
    }
}

/// A shell is in the session it started in, by the name that session had: a
/// rename takes it along, and one that started before its session was made
/// is another session's of that name. The hook counts whole seconds, so the
/// first shells of a session started in the second it was made, by its
/// count. A shell started in a pane's shell is the pane's, until it exits.
#[test]
fn a_pane_s_shell_is_the_newest_that_started_in_its_session_by_any_name() {
    let host = Host::new();
    let panes: Vec<_> = (1..=3).map(at_home).collect();
    host.zellij.list("work", &json!(panes));
    host.zellij.list("play", &json!(panes));
    let _work = host.serve("work");
    let made = fs::metadata(host.contract().join("work")).unwrap().modified().unwrap();
    let made = made.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let _daemon = start(host.vars()).unwrap();
    let ended = |command: &str| format!("ended {made} 0 2000 {command}");
    say(&host, "work", 1, made, &ended("make"));
    say(&host, "work", 2, made, &ended("cargo test"));
    say(&host, "work", 2, made + 1, &ended("nested"));
    say(&host, "work", 3, made - 100, &ended("gone by"));

    let commands = |name| -> Option<Vec<Option<String>>> {
        let shells = shells(&host, name)?;
        Some(shells.iter().map(|shell| shell.as_ref().map(|it| command(it).to_owned())).collect())
    };
    let expected = [Some("make".to_owned()), Some("nested".to_owned()), None];
    eventually("work's shells have said", || (commands("work")? == expected).then_some(()));

    fs::rename(host.contract().join("work"), host.contract().join("play")).unwrap();
    eventually("work is called play", || (commands("play")? == expected).then_some(()));
    fs::remove_file(host.runtime.path().join(format!("luneta/commands/work/2.{}", made + 1)))
        .unwrap();
    eventually("the nested shell exits", || {
        (commands("play")?[1].as_deref() == Some("cargo test")).then_some(())
    });
}

/// A session made under the name of one renamed, while that one runs, has the
/// shells started in it since: they say the name it has, as the old panes of
/// the one renamed do. The one renamed keeps the shells that started in it
/// before.
#[test]
fn a_session_that_takes_a_renamed_one_s_name_has_the_shells_started_in_it() {
    let host = Host::new();
    let panes: Vec<_> = (1..=2).map(at_home).collect();
    host.zellij.list("work", &json!(panes));
    host.zellij.list("play", &json!(panes));
    let _old = host.serve("work");
    let made = fs::metadata(host.contract().join("work")).unwrap().modified().unwrap();
    let made = made.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let _daemon = start(host.vars()).unwrap();
    say(&host, "work", 1, made, &format!("ended {made} 0 2000 make"));
    let commands = |name| -> Option<Vec<Option<String>>> {
        let shells = shells(&host, name)?;
        Some(shells.iter().map(|shell| shell.as_ref().map(|it| command(it).to_owned())).collect())
    };
    let old = [Some("make".to_owned()), None];
    eventually("work's shell has said", || (commands("work")? == old).then_some(()));
    fs::rename(host.contract().join("work"), host.contract().join("play")).unwrap();
    eventually("work is called play", || (commands("play")? == old).then_some(()));

    // Past the second the old shell started in, as the hook counts it, so
    // that it could not be the new session's.
    while now() <= made + 1 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _new = host.serve("work");
    let started = now();
    say(&host, "work", 2, started, &format!("ended {started} 0 2000 cargo test"));
    let new = [None, Some("cargo test".to_owned())];
    eventually("the new work has its shell", || (commands("work")? == new).then_some(()));
    assert_eq!(commands("play"), Some(old.to_vec()));
}
