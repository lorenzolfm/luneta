use std::fs;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use lunetad::api::shell::{ended, Doing, Ended, Going, Stopped};
use lunetad::api::{self, Front};
use prost_types::{Duration, Timestamp};
use rustix::fs::{Mode, OFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, LocalModes, OptionalActions};
use serde_json::json;

use crate::host::{eventually, start, Host};
use crate::sessions::{listed, session};

fn say(host: &Host, session: &str, pane: u32, started: u64, line: &str) {
    let dir = host.runtime.path().join("luneta/commands").join(session);
    fs::create_dir_all(&dir).unwrap();
    let new = dir.join(format!("{pane}.{started}.new"));
    fs::write(&new, format!("{line}\n")).unwrap();
    fs::rename(new, dir.join(format!("{pane}.{started}"))).unwrap();
}

fn at_home(id: u32) -> serde_json::Value {
    listed(id, "~", (0, "Tab #1"), Some("/home/you"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn shells(host: &Host, name: &str) -> Option<Vec<Option<api::Shell>>> {
    let panes = session(&host.host(), name)?.panes.clone()?.panes;
    Some(panes.into_iter().map(|pane| pane.shell).collect())
}

fn command(shell: &api::Shell) -> &str {
    let command = match shell.doing.as_ref().unwrap() {
        Doing::Going(going) => &going.command,
        Doing::Ended(ended) => &ended.command,
        Doing::Stopped(stopped) => &stopped.command,
        Doing::Ready(()) => return "",
    };
    command.as_deref().expect("the hook says each command")
}

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

    fn take(&self, lines: bool, echo: bool) {
        let mut modes = termios::tcgetattr(&self.tty).unwrap();
        modes.local_modes.set(LocalModes::ICANON, lines);
        modes.local_modes.set(LocalModes::ECHO, echo);
        termios::tcsetattr(&self.tty, OptionalActions::Now, &modes).unwrap();
    }
}

#[test]
fn a_pane_has_what_its_shell_said_and_a_command_what_its_terminal_says() {
    let host = Host::new();
    let panes: Vec<_> = (1..=8).map(at_home).collect();
    host.zellij.list("work", &json!(panes));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    let terminal = Terminal::open();
    let started = now();
    let tty = terminal.path.display();
    say(&host, "work", 1, started, &format!("going {started} {tty} cargo build"));
    say(&host, "work", 2, started, &format!("ended {started} 101 41873 cargo test"));
    say(&host, "work", 3, started, &format!("ended {started} fg 7120 make"));
    say(&host, "work", 4, started, &format!("stopped {started} nvim"));
    say(&host, "work", 5, started, &format!("going {started} /dev/luneta-gone sleep 30"));
    let writing = host.runtime.path().join(format!("luneta/commands/work/6.{started}.new"));
    fs::write(writing, format!("ended {started} 0 2000 true\n")).unwrap();
    say(&host, "work", 7, started, &format!("went {started} make"));
    say(&host, "work", 8, started, "ready");

    let said = eventually("the shells of work's panes have said", || {
        let shells = shells(&host, "work")?;
        (shells[..4].iter().all(Option::is_some) && shells[7].is_some()).then_some(shells)
    });
    let at = Some(Timestamp { seconds: i64::try_from(started).unwrap(), nanos: 0 });
    let doing = |doing| Some(api::Shell { doing: Some(doing) });
    let going = |front: Front| {
        let command = Some("cargo build".to_owned());
        doing(Doing::Going(Going { command, since: at, front: front.into() }))
    };
    let took = |seconds, millis: i32| Some(Duration { seconds, nanos: millis * 1_000_000 });
    assert_eq!(
        said,
        [
            going(Front::Working),
            doing(Doing::Ended(Ended {
                command: Some("cargo test".to_owned()),
                at,
                ending: Some(ended::Ending::Status(101)),
                took: took(41, 873),
            })),
            doing(Doing::Ended(Ended {
                command: Some("make".to_owned()),
                at,
                ending: Some(ended::Ending::Resumed(())),
                took: took(7, 120),
            })),
            doing(Doing::Stopped(Stopped { command: Some("nvim".to_owned()), at })),
            None,
            None,
            None,
            doing(Doing::Ready(())),
        ]
    );
    let unread = host.runtime.path().join(format!("luneta/commands/work/7.{started}"));
    let log = host.home.path().join(".local/state/luneta/luneta.log");
    let unread = format!("{}: a line this lunetad cannot read", unread.display());
    eventually("the log says which file lunetad cannot read", || {
        fs::read_to_string(&log).ok()?.contains(&unread).then_some(())
    });

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

#[test]
fn a_session_is_asked_about_again_as_a_shell_in_it_runs_a_command() {
    let host = Host::new();
    host.zellij.list("work", &json!([at_home(1)]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    let started = now();
    say(&host, "work", 1, started, &format!("ended {started} 0 2 true"));
    eventually("work's shell has said", || shells(&host, "work")?[0].clone());

    host.zellij.asked_again("work");
    host.zellij.list("work", &json!([listed(1, "~/work", (0, "Tab #1"), Some("/home/you/work"))]));
    say(&host, "work", 1, started, &format!("ended {} 0 3 cd work", now()));
    let said = std::time::Instant::now();
    eventually("work's pane is in work", || {
        let panes = session(&host.host(), "work")?.panes.clone()?.panes;
        (panes[0].cwd.as_deref() == Some("/home/you/work")).then_some(())
    });
    assert!(said.elapsed() < std::time::Duration::from_secs(1), "{:?}", said.elapsed());
}
