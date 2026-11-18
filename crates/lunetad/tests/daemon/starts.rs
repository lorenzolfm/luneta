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
use lunetad::api::projects::Held;
use lunetad::api::terminal::Found;
use lunetad::api;
use lunetad::opens;
use lunetad::program::alone;
use rustix::fs::{Mode, OFlags};
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};
use tempfile::TempDir;

use crate::demand::{answering, moved, stand_in_watched_as, stop, Detached};
use crate::host::{eventually, script, Host, Running, Vars, LUNETA};
use crate::own::Owned;
use crate::projects::{project, workstream};

const ENDS: Duration = Duration::from_secs(30);

const RUNS: Duration = Duration::from_secs(5);

pub const UNSHOWN: &str = "No workstream to show yet.";

const CD_FEAT: &str = "cd ../feat && printf '\\033]7;file://localhost%s\\007' \"$PWD\"\r";

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

pub fn work_in(owned: &Owned, name: &str) -> Directory {
    let mut made = directory(name);
    project(&owned.host, Path::new(&real(&made.path)), None);
    made.path.push("main");
    fs::create_dir(&made.path).unwrap();
    made
}

pub fn luneta(vars: Vars, dir: &Path) -> Command {
    luneta_at(LUNETA, vars, dir)
}

fn luneta_at(program: &str, vars: Vars, dir: &Path) -> Command {
    let mut command = vars.command(program);
    command.current_dir(dir).env_remove("SSH_CONNECTION").env_remove("SSH_TTY");
    command
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

    pub fn opening(host: &Host, dir: &Directory, args: &[&str]) -> Window {
        let mut command = luneta(host.vars(), &dir.path);
        command.args(args);
        Window::started(command)
    }

    pub fn running(command: Command) -> Window {
        let window = Window::started(command);
        eventually("luneta draws its sidebar", || window.shows("─ luneta").then_some(()));
        window
    }

    fn started(mut command: Command) -> Window {
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
        Window { typed, said, luneta }
    }

    pub fn send(&mut self, sent: &str) {
        self.typed.write_all(sent.as_bytes()).unwrap();
    }

    fn term(&self) -> Term<VoidListener> {
        let said = self.said.lock().unwrap().clone();
        let mut screen = Term::new(Config::default(), &TermSize::new(120, 40), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, &said);
        screen
    }

    pub fn cursor(&self) -> (usize, usize) {
        let point = self.term().grid().cursor.point;
        (usize::try_from(point.line.0).unwrap(), point.column.0)
    }

    pub fn screen(&self) -> Vec<String> {
        let screen = self.term();
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

pub fn mouse(code: u8, (row, column): (usize, usize), last: char) -> String {
    format!("\x1b[<{code};{};{}{last}", column + 1, row + 1)
}

fn log_of(host: &Host) -> String {
    fs::read_to_string(host.home.path().join(".local/state/luneta/luneta.log")).unwrap()
}

pub fn makes_shells() -> bool {
    Path::new(&opens::your_shell()).is_file()
}

pub fn projects_of(owned: &Owned) -> Vec<api::Project> {
    match owned.host.host().projects.and_then(|projects| projects.held) {
        Some(Held::Kept(kept)) => kept.projects,
        Some(Held::Unkept(_)) | None => Vec::new(),
    }
}

fn real(path: &Path) -> String {
    fs::canonicalize(path).unwrap().to_str().unwrap().to_owned()
}

pub fn shown(owned: &Owned, folder: &Path) -> Option<api::Workstream> {
    let folder = real(folder);
    let workstreams = projects_of(owned).into_iter().flat_map(|project| project.workstreams);
    let mut held = workstreams.filter(|workstream| workstream.folder == folder);
    let found = |terminal: &api::Terminal| matches!(terminal.found, Some(Found::Key(_)));
    held.find(|workstream| workstream.terminals.iter().any(found))
}

#[test]
fn a_bare_luneta_makes_the_project_of_its_directory_and_shows_no_workstream_in_it() {
    let owned = Owned::new();
    let dir = directory("project");
    let window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows no workstream", || window.shows(UNSHOWN).then_some(()));
    let projects = projects_of(&owned);
    let made: Vec<(&str, &str, usize)> = projects
        .iter()
        .map(|project| (project.name.as_str(), project.dir.as_str(), project.workstreams.len()))
        .collect();
    assert_eq!(made, [("project", real(&dir.path).as_str(), 0)]);
}

#[test]
fn a_bare_luneta_in_a_project_s_own_folder_shows_its_workstream_used_last() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = directory("project");
    let made = project(&owned.host, Path::new(&real(&dir.path)), None);
    let folders = ["one", "two"].map(|name| dir.path.join(name));
    for folder in &folders {
        fs::create_dir(folder).unwrap();
        workstream(&owned.host, made, Path::new(&real(folder)));
    }
    let window = Window::running(luneta(owned.host.vars(), &folders[1]));
    eventually("luneta shows two", || shown(&owned, &folders[1]));
    drop(window);
    let _window = Window::open(&owned.host, &dir, &[]);
    thread::sleep(Duration::from_secs(1));
    let shown = eventually("luneta shows two again", || shown(&owned, &folders[1]));
    assert_eq!(shown.terminals.len(), 1, "{:?}", projects_of(&owned));
    assert!(self::shown(&owned, &folders[0]).is_none(), "one, used never, stays empty");
    let workstreams = projects_of(&owned).iter().map(|it| it.workstreams.len()).sum::<usize>();
    assert_eq!(workstreams, 2, "no workstream is made in the project's own folder");
}

#[test]
fn a_bare_luneta_in_a_project_shows_the_workstream_of_its_folder() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = directory("project");
    let below = dir.path.join("below");
    fs::create_dir(&below).unwrap();
    let other = dir.path.join("other");
    fs::create_dir(&other).unwrap();
    let made = project(&owned.host, Path::new(&real(&dir.path)), None);
    workstream(&owned.host, made, Path::new(&real(&other)));
    let in_below = workstream(&owned.host, made, Path::new(&real(&below)));
    let _window = Window::running(luneta(owned.host.vars(), &below));
    let shown = eventually("luneta shows the folder's workstream", || shown(&owned, &below));
    assert_eq!(shown.id, in_below);
    assert!(self::shown(&owned, &other).is_none(), "the project's other stays empty");
    assert_eq!(projects_of(&owned).len(), 1);
}

#[test]
fn a_new_session_on_a_pane_in_a_folder_of_a_project_shows_that_folder_in_the_project() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let feat = dir.path.parent().unwrap().join("feat");
    fs::create_dir(&feat).unwrap();
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    window.send(CD_FEAT);
    let deadline = Instant::now() + Duration::from_secs(10);
    while shown(&owned, &feat).is_none() {
        assert!(Instant::now() < deadline, "n s showed no feat: {:#?}", window.screen());
        window.send("\x1b[97;3uns");
        thread::sleep(Duration::from_secs(1));
    }
    let projects = projects_of(&owned);
    let folders: Vec<(&str, Vec<&str>)> = projects
        .iter()
        .map(|project| {
            let folders = project.workstreams.iter().map(|it| it.folder.as_str()).collect();
            (project.name.as_str(), folders)
        })
        .collect();
    assert_eq!(folders, [("work", vec![real(&dir.path).as_str(), real(&feat).as_str()])]);
}

#[test]
fn a_named_luneta_shows_the_project_of_that_name_wherever_it_starts() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let named = directory("named");
    let made = project(&owned.host, Path::new(&real(&named.path)), None);
    let main = named.path.join("main");
    fs::create_dir(&main).unwrap();
    workstream(&owned.host, made, Path::new(&real(&main)));
    let _window = Window::open(&owned.host, &directory("anywhere"), &["named"]);
    eventually("luneta shows named", || shown(&owned, &main));
    assert_eq!(projects_of(&owned).len(), 1);
}

#[test]
fn a_named_luneta_refuses_a_name_two_projects_share() {
    let owned = Owned::new();
    let (one, other) = (directory("one"), directory("other"));
    project(&owned.host, Path::new(&real(&one.path)), Some("twin"));
    project(&owned.host, Path::new(&real(&other.path)), Some("twin"));
    let output = luneta(owned.host.vars(), &one.path).arg("twin").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    let dirs = format!("{}, {}", real(&one.path), real(&other.path));
    assert!(refusal.contains(&format!("projects in {dirs} are all named twin")), "{refusal}");
}

#[test]
fn a_bare_luneta_says_why_lunetad_keeps_no_more_projects_rather_than_go_to_another() {
    let owned = Owned::new();
    let kept = directory("kept");
    for at in 0..lunetad::projects::PROJECTS_MAX {
        let dir = kept.path.join(format!("p{at}"));
        fs::create_dir(&dir).unwrap();
        project(&owned.host, Path::new(&real(&dir)), None);
    }
    let dir = directory("new");
    let output = luneta(owned.host.vars(), &dir.path).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    assert!(refusal.contains("projects, as many as are kept"), "{refusal}");
}

#[test]
fn a_bare_luneta_where_no_project_may_be_made_shows_the_one_it_saw_last() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let outer = directory("outer");
    let inner = outer.path.join("inner");
    fs::create_dir(&inner).unwrap();
    let made = project(&owned.host, Path::new(&real(&inner)), None);
    let main = inner.join("main");
    fs::create_dir(&main).unwrap();
    workstream(&owned.host, made, Path::new(&real(&main)));
    let _window = Window::open(&owned.host, &outer, &[]);
    eventually("luneta shows inner", || shown(&owned, &main));
    assert_eq!(projects_of(&owned).len(), 1, "no project is made around another");
}

#[test]
fn a_bare_luneta_where_the_directory_names_no_project_asks_for_a_name() {
    let owned = Owned::new();
    let output = luneta(owned.host.vars(), Path::new("/")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    let said = "luneta: no workstream to show: / names no project: name one";
    assert!(refusal.starts_with(said), "{refusal}");
}

#[test]
fn the_log_says_luneta_ended_by_a_signal() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let command = luneta(owned.host.vars(), &dir.path);
    let ended = on_terminal_with(command, |luneta| {
        eventually("luneta shows work", || shown(&owned, &dir.path));
        stop(luneta.pid());
    });
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    let log = log_of(&owned.host);
    assert!(log.contains("ends: ended by signal 15"), "{log}");
}

#[test]
fn a_daemon_that_goes_away_is_started_again_in_moments() {
    let mut owned = Owned::new();
    let dir = work_in(&owned, "work");
    let _window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
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
    let dir = work_in(&owned, "work");
    let command = luneta(owned.host.vars(), &dir.path);
    let notes = owned.host.home.path().join("notes");
    let mut stand_in = None;
    let ended = on_terminal_with(command, |luneta| {
        eventually("luneta shows work", || shown(&owned, &dir.path));
        owned.stop_daemon();
        stand_in = Some(stand_in_watched_as(&owned.host, OTHER, &notes));
        thread::sleep(RUNS);
        stop(luneta.pid());
    });
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    let stand_in = stand_in.unwrap();

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
    let elsewhere = moved();
    let started = elsewhere.path().join("started");
    let note = format!("echo started >> '{}'\nexit 1\n", started.display());
    script(&elsewhere.path().join("lunetad"), &note);
    let program = elsewhere.path().join("luneta").display().to_string();

    let dir = work_in(&owned, "work");
    let command = luneta_at(&program, owned.host.vars(), &dir.path);
    let mut began = None;
    let ended = on_terminal_with(command, |luneta| {
        eventually("luneta shows work", || shown(&owned, &dir.path));
        owned.stop_daemon();
        began = Some(Instant::now());
        thread::sleep(RUNS);
        stop(luneta.pid());
    });
    let took = began.unwrap().elapsed();
    assert_eq!(ended.status.code(), Some(1), "{}", ended.said);
    let starts = fs::read_to_string(&started).unwrap().lines().count();
    assert!(starts >= 2, "it was started {starts} times in {took:?}");
    let most = usize::try_from(took.as_secs()).unwrap() + 1;
    assert!(starts <= most, "it was started {starts} times in {took:?}");
}


