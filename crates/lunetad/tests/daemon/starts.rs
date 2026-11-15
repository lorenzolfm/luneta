use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use base64::Engine as _;
use lunetad::api::NewOwnResponse;
use lunetad::opens;
use lunetad::program::alone;
use rustix::fs::{Mode, OFlags};
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};
use serde_json::json;
use tempfile::TempDir;

use crate::demand::{answering, moved, stand_in_watched_as, stop, Detached};
use crate::host::{eventually, script, Host, Running, Vars, LUNETA};
use crate::own::{own, Owned};
use crate::sessions::session;

const ENDS: Duration = Duration::from_secs(30);

const RUNS: Duration = Duration::from_secs(5);

pub struct Ended {
    pub status: ExitStatus,
    pub said: String,
}

pub fn terminal() -> (OwnedFd, OwnedFd) {
    let controller = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&controller, FdFlags::CLOEXEC).unwrap();
    pty::grantpt(&controller).unwrap();
    pty::unlockpt(&controller).unwrap();
    let path = pty::ptsname(&controller, Vec::new()).unwrap().into_string().unwrap();
    let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
    let tty: OwnedFd = rustix::fs::open(&path, flags, Mode::empty()).unwrap();
    let size = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    termios::tcsetwinsize(&tty, size).unwrap();
    (controller, tty)
}

pub fn spawn_on(tty: &OwnedFd, command: &mut Command) -> Running {
    let stdio = || Stdio::from(tty.try_clone().unwrap());
    Running::spawn(alone(command).stdin(stdio()).stdout(stdio()).stderr(stdio()))
}

pub fn on_terminal(command: Command) -> Ended {
    on_terminal_with(command, |_| {})
}

fn on_terminal_with(mut command: Command, meanwhile: impl FnOnce(&Running)) -> Ended {
    let (controller, tty) = terminal();
    let reader = thread::spawn(move || {
        let mut said = Vec::new();
        let _ = File::from(controller).read_to_end(&mut said);
        said
    });
    let mut luneta = spawn_on(&tty, &mut command);
    drop(command);
    drop(tty);
    meanwhile(&luneta);
    let status = luneta.ended_within(ENDS).expect("luneta did not end");
    let said = reader.join().unwrap();
    Ended { status, said: String::from_utf8_lossy(&said).into_owned() }
}

pub struct Directory {
    _parent: TempDir,
    pub path: PathBuf,
}

pub fn directory(name: &str) -> Directory {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join(name);
    fs::create_dir(&path).unwrap();
    Directory { _parent: parent, path }
}

pub fn luneta(vars: Vars, dir: &Path) -> Command {
    luneta_at(LUNETA, vars, dir)
}

fn luneta_at(program: &str, vars: Vars, dir: &Path) -> Command {
    let mut command = vars.command(program);
    command.current_dir(dir).env_remove("SSH_CONNECTION").env_remove("SSH_TTY");
    command
}

pub fn runs(host: &Host, name: &str) -> std::os::unix::net::UnixListener {
    host.zellij.list(name, &json!([]));
    host.serve(name)
}

pub struct Window {
    typed: File,
    said: Arc<Mutex<Vec<u8>>>,
    pub luneta: Running,
}

impl Window {
    pub fn open(host: &Host, dir: &Directory, args: &[&str]) -> Window {
        let mut command = luneta(host.vars(), &dir.path);
        command.args(args);
        Window::running(command)
    }

    pub fn running(mut command: Command) -> Window {
        let (controller, tty) = terminal();
        let typed = File::from(controller.try_clone().unwrap());
        let said = Arc::new(Mutex::new(Vec::new()));
        let reading = Arc::clone(&said);
        thread::spawn(move || {
            let mut controller = File::from(controller);
            let mut read = [0; 4096];
            loop {
                match controller.read(&mut read) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => reading.lock().unwrap().extend_from_slice(&read[..count]),
                }
            }
        });
        let luneta = spawn_on(&tty, &mut command);
        drop(tty);
        let window = Window { typed, said, luneta };
        eventually("luneta draws its sidebar", || window.shows("─ luneta").then_some(()));
        window
    }

    pub fn send(&mut self, sent: &str) {
        self.typed.write_all(sent.as_bytes()).unwrap();
    }

    pub fn screen(&self) -> Vec<String> {
        let said = self.said.lock().unwrap().clone();
        let mut screen = Term::new(Config::default(), &TermSize::new(120, 40), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, &said);
        let grid = screen.grid();
        let line = |row: usize| -> String {
            let row = Line(i32::try_from(row).unwrap());
            (0..screen.columns()).map(|column| grid[Point::new(row, Column(column))].c).collect()
        };
        (0..screen.screen_lines()).map(line).collect()
    }

    pub fn shows(&self, text: &str) -> bool {
        self.screen().iter().any(|line| line.contains(text))
    }

    pub fn spot_of(&self, text: &str) -> (usize, usize) {
        eventually(&format!("luneta shows {text:?}"), || {
            let screen = self.screen();
            screen.iter().enumerate().find_map(|(row, line)| {
                let at = line.find(text)?;
                Some((row, line[..at].chars().count()))
            })
        })
    }

    pub fn copied(&self) -> Vec<String> {
        let said = String::from_utf8_lossy(&self.said.lock().unwrap()).into_owned();
        let copies = said.split("\x1b]52;c;").skip(1);
        let decoded = copies.map(|copy| {
            let end = copy.find('\x07').unwrap();
            let text = base64::engine::general_purpose::STANDARD.decode(&copy[..end]).unwrap();
            String::from_utf8(text).unwrap()
        });
        decoded.collect()
    }
}

pub fn clients(owned: &Owned, name: &str) -> Option<usize> {
    Some(session(&owned.host.host(), name)?.clients.clone()?.clients.len())
}

pub fn mouse(code: u8, (row, column): (usize, usize), last: char) -> String {
    format!("\x1b[<{code};{};{}{last}", column + 1, row + 1)
}

fn log_of(host: &Host) -> String {
    fs::read_to_string(host.home.path().join(".local/state/luneta/luneta.log")).unwrap()
}

fn in_pane_of(command: &mut Command, made: &NewOwnResponse) {
    let at = made.made_at.unwrap();
    let nanos = u64::try_from(at.seconds).unwrap() * 1_000_000_000;
    let nanos = nanos + u64::try_from(at.nanos).unwrap();
    command
        .env("LUNETA_MUX_SESSION", made.session.to_string())
        .env("LUNETA_MUX_MADE", nanos.to_string())
        .env("LUNETA_MUX_PANE", made.pane.to_string());
}

fn makes_shells() -> bool {
    Path::new(&opens::your_shell()).is_file()
}

#[test]
fn a_bare_luneta_joins_the_session_of_its_directory_or_else_the_newest() {
    let owned = Owned::new();
    owned.make("project").unwrap();
    owned.make("newest").unwrap();

    let window = Window::open(&owned.host, &directory("project"), &[]);
    eventually("luneta is in project", || (clients(&owned, "project")? == 1).then_some(()));
    drop(window);
    let _window = Window::open(&owned.host, &directory("elsewhere"), &[]);
    eventually("luneta is in newest", || (clients(&owned, "newest")? == 1).then_some(()));
}

#[test]
fn a_bare_luneta_makes_the_session_of_its_directory_only_where_none_runs() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let _window = Window::open(&owned.host, &directory("project"), &[]);
    eventually("luneta is in project", || (clients(&owned, "project")? == 1).then_some(()));
    assert!(own(&owned.listed("project")).is_some());
}

#[test]
fn a_session_named_is_made_though_another_runs() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    owned.make("other").unwrap();
    let _window = Window::open(&owned.host, &directory("project"), &["named"]);
    eventually("luneta is in named", || (clients(&owned, "named")? == 1).then_some(()));
    assert_eq!(clients(&owned, "other"), Some(0));
}

#[test]
fn a_bare_luneta_never_joins_the_session_its_terminal_is_a_pane_of() {
    let owned = Owned::new();
    let project = owned.make("project").unwrap();
    let dir = directory("project");
    let mut alone = luneta(owned.host.vars(), &dir.path);
    in_pane_of(&mut alone, &project);
    let output = alone.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    let held = "luneta runs in project, and goes nowhere it shows itself";
    assert!(refusal.starts_with(&format!("luneta: {held}")), "{refusal}");
    let log = log_of(&owned.host);
    assert!(log.contains(&format!("ends: {held}")), "{log}");
    assert_eq!(clients(&owned, "project").unwrap_or(0), 0);

    owned.make("other").unwrap();
    let mut other = luneta(owned.host.vars(), &dir.path);
    in_pane_of(&mut other, &project);
    let _window = Window::running(other);
    eventually("luneta is in other", || (clients(&owned, "other")? == 1).then_some(()));
}

#[test]
fn the_log_says_luneta_ended_by_a_signal() {
    let owned = Owned::new();
    owned.make("work").unwrap();
    let dir = directory("work");
    let command = luneta(owned.host.vars(), &dir.path);
    let ended = on_terminal_with(command, |luneta| {
        eventually("luneta is in work", || (clients(&owned, "work")? == 1).then_some(()));
        stop(luneta.pid());
    });
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    let log = log_of(&owned.host);
    assert!(log.contains("ends: ended by signal 15"), "{log}");
}

#[test]
fn a_daemon_that_goes_away_is_started_again_in_moments() {
    let mut owned = Owned::new();
    owned.make("work").unwrap();
    let _window = Window::open(&owned.host, &directory("work"), &[]);
    eventually("luneta is in work", || (clients(&owned, "work")? == 1).then_some(()));
    thread::sleep(Duration::from_secs(3));
    let was = answering(&owned.host).unwrap();
    let went = Instant::now();
    owned.stop_daemon();
    let pid = eventually("a daemon answers again", || {
        answering(&owned.host).filter(|answers| *answers != was)
    });
    let took = went.elapsed();
    let _daemon = Detached { host: &owned.host, pid };
    assert!(took < Duration::from_secs(1), "it took {took:?}");
}

#[test]
fn an_answer_of_another_daemon_than_the_one_reached_is_dropped() {
    const OTHER: u32 = 1;
    let mut owned = Owned::new();
    owned.make("work").unwrap();
    owned.stop_daemon();
    let notes = owned.host.home.path().join("notes");
    let stand_in = stand_in_watched_as(&owned.host, OTHER, &notes);

    let dir = directory("work");
    let command = luneta(owned.host.vars(), &dir.path);
    let ended = on_terminal_with(command, |luneta| {
        thread::sleep(RUNS);
        stop(luneta.pid());
    });
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);

    let log = log_of(&owned.host);
    let note = format!(
        "lunetad (pid {OTHER}) answered in the place of the one luneta reached (pid {})",
        stand_in.pid()
    );
    assert!(log.contains(&note), "{log}");

    let notes = fs::read_to_string(&notes).unwrap();
    let asked = |what| notes.lines().filter(|line| *line == what).count();
    let (infos, watches) = (asked("info"), asked("watch"));
    assert!(infos >= 3, "it reached for a daemon {infos} times: {notes}");
    assert!(infos.abs_diff(watches) <= 1, "{infos} of Info to {watches} of Watch: {notes}");
}

#[test]
fn a_lunetad_that_does_not_start_is_started_once_a_second_at_the_most() {
    let mut owned = Owned::new();
    owned.make("work").unwrap();
    owned.stop_daemon();
    let elsewhere = moved();
    let started = elsewhere.path().join("started");
    let note = format!("echo started >> '{}'\nexit 1\n", started.display());
    script(&elsewhere.path().join("lunetad"), &note);
    let program = elsewhere.path().join("luneta").display().to_string();

    let began = Instant::now();
    let dir = directory("work");
    let command = luneta_at(&program, owned.host.vars(), &dir.path);
    let ended = on_terminal_with(command, |luneta| {
        thread::sleep(RUNS);
        stop(luneta.pid());
    });
    let took = began.elapsed();
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    let starts = fs::read_to_string(&started).unwrap().lines().count();
    assert!(starts >= 2, "it was started {starts} times in {took:?}");
    let most = usize::try_from(took.as_secs()).unwrap() + 1;
    assert!(starts <= most, "it was started {starts} times in {took:?}");
}
