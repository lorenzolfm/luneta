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
use lunetad::shells;
use lunetad::program::alone;
use rustix::fs::{Mode, OFlags};
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};
use tempfile::TempDir;

use crate::demand::{answering, moved, stop, Detached};
use crate::host::{eventually, script, Host, Running, Vars, LUNETA};
use crate::own::Owned;
use crate::projects::{project, worktree};
use crate::worktree::{cloned_as_wt, git, worktree_added};

const ENDS: Duration = Duration::from_secs(30);

const RUNS: Duration = Duration::from_secs(5);

pub const UNSHOWN: &str = "No worktree to show yet.";

pub const SPLIT: &str = "\x1b[97;3unv";

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

pub fn cloned(name: &str, worktrees: &[&str]) -> Directory {
    let made = directory(name);
    let parent = made.path.parent().unwrap().to_owned();
    cloned_as_wt(&parent, &parent.join("up"), &made.path);
    for worktree in worktrees {
        worktree_added(&made.path, worktree);
    }
    made
}

pub fn work_in(owned: &Owned, name: &str) -> Directory {
    let mut made = cloned(name, &[]);
    project(&owned.host, Path::new(&real(&made.path)), None);
    made.path.push("main");
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

const FOLDERS: &str = "\x1b[100;5u";

pub fn makes_shells() -> bool {
    Path::new(&shells::your_shell()).is_file()
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

pub fn shown(owned: &Owned, folder: &Path) -> Option<api::Worktree> {
    let folder = real(folder);
    let projects = projects_of(owned);
    let worktrees = projects.iter().flat_map(|project| project.worktrees().unwrap());
    let mut held = worktrees.filter(|worktree| worktree.folder == folder);
    let found = |terminal: &api::Terminal| matches!(terminal.found, Some(Found::Key(_)));
    held.find(|worktree| worktree.terminals.iter().any(found)).cloned()
}

#[test]
fn a_bare_luneta_where_no_project_holds_its_folder_makes_none_and_shows_none() {
    let owned = Owned::new();
    let dir = directory("project");
    let window = Window::open(&owned.host, &dir, &[]);
    let none = || window.shows("no projects").then_some(());
    eventually("luneta's sidebar says it has no project", none);
    thread::sleep(Duration::from_secs(1));
    assert!(projects_of(&owned).is_empty(), "a project is made only when asked for");
}

#[test]
fn a_bare_luneta_in_a_project_s_own_folder_shows_its_worktree_used_last() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = cloned("project", &["one", "two"]);
    project(&owned.host, Path::new(&real(&dir.path)), None);
    let folders = ["one", "two"].map(|name| dir.path.join(name));
    let window = Window::running(luneta(owned.host.vars(), &folders[1]));
    eventually("luneta shows two", || shown(&owned, &folders[1]));
    drop(window);
    let _window = Window::open(&owned.host, &dir, &[]);
    thread::sleep(Duration::from_secs(1));
    let shown = eventually("luneta shows two again", || shown(&owned, &folders[1]));
    assert_eq!(shown.terminals.len(), 1, "{:?}", projects_of(&owned));
    assert!(self::shown(&owned, &folders[0]).is_none(), "one, used never, stays empty");
    let counted = |it: &api::Project| it.worktrees().unwrap().len();
    let worktrees = projects_of(&owned).iter().map(counted).sum::<usize>();
    assert_eq!(worktrees, 1, "none is opened at the clone's root, nor one never opened");
}

#[test]
fn a_bare_luneta_at_a_clone_s_root_shows_its_home_and_asks_the_agent_of_what_is_marked() {
    let owned = Owned::new();
    let dir = cloned("project", &["one", "two"]);
    git(&dir.path.join(".bare"), &["remote", "add", "upstream", "git@github.com:o/n.git"]);
    let root = real(&dir.path);
    project(&owned.host, Path::new(&root), None);
    let issues = serde_json::json!([
        {"number": 507, "title": "Open a project on a home view", "labels": [{"name": "enhancement"}]},
        {"number": 498, "title": "Menu says no project", "labels": []},
    ]);
    let pulls = serde_json::json!([
        {"number": 524, "title": "Say view, not pane", "isDraft": true, "headRefName": "issue-511",
         "author": {"login": "o"}},
    ]);
    owned.host.gh.work(&issues, &pulls);
    let home = owned.host.home.path().display().to_string();
    let claude = format!(
        r#"pwd -P > "{home}/asked-in"
cat > "{home}/words"
echo '{{"type":"system","subtype":"init","session_id":"7c22"}}'
echo '{{"type":"result","subtype":"success","is_error":false,"result":"Started both."}}'
"#
    );
    script(&owned.host.bin().join("claude"), &claude);
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows the project's open issues and pull requests", || {
        let listed = ["#507", "Open a project on a home view", "enhancement", "#524", "issue-511"];
        listed.iter().all(|it| window.shows(it)).then_some(())
    });
    assert!(window.shows("Issues · 2") && window.shows("Pull requests · 1"));
    let projects = projects_of(&owned);
    assert!(projects[0].worktrees().unwrap().is_empty(), "no worktree opens: {projects:?}");
    window.send("\t\t\ttackle these\r");
    eventually("the agent's answer shows in the home", || window.shows("Started both.").then_some(()));
    let asked_in = fs::read_to_string(owned.host.home.path().join("asked-in")).unwrap();
    assert_eq!(asked_in.trim(), root, "the agent works in the project's folder");
    let words = fs::read_to_string(owned.host.home.path().join("words")).unwrap();
    let marked = "[luneta: in the project's work view the user marked issues #498 and #507, and \
                  pull request #524:";
    assert!(words.contains(marked), "{words}");
    assert!(words.ends_with("\n\ntackle these"), "{words}");
}

#[test]
fn a_clone_s_root_picked_from_the_folders_shows_its_home() {
    let owned = Owned::new();
    let dir = cloned("project", &["one", "two"]);
    let root = real(&dir.path);
    script(&owned.host.bin().join("zoxide"), &format!("echo '{root}'\n"));
    let mut window = Window::open(&owned.host, &directory("elsewhere"), &[]);
    let none = || window.shows("no projects").then_some(());
    eventually("luneta's sidebar says it has no project", none);
    window.send(FOLDERS);
    let tmp = dir.path.parent().unwrap().file_name().unwrap().to_str().unwrap().to_owned();
    eventually("the folders list the clone's root", || window.shows(&tmp).then_some(()));
    window.send("\r");
    eventually("luneta shows the project's home", || {
        window.shows("no remote on GitHub, so nothing is listed here").then_some(())
    });
    let projects = projects_of(&owned);
    let opened = projects[0].worktrees().unwrap();
    assert!(opened.is_empty(), "none opens until one is picked: {projects:?}");
}

#[test]
fn a_bare_luneta_in_a_project_shows_the_worktree_of_its_folder() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = cloned("project", &["below", "other"]);
    let (below, other) = (dir.path.join("below"), dir.path.join("other"));
    let made = project(&owned.host, Path::new(&real(&dir.path)), None);
    let in_other = worktree(&owned.host, made, Path::new(&real(&other)));
    let in_below = worktree(&owned.host, made, Path::new(&real(&below)));
    let _window = Window::running(luneta(owned.host.vars(), &below));
    let used = |id: u64| {
        let projects = projects_of(&owned);
        let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
        worktrees.find(|it| it.id == id).and_then(|it| it.used_at)
    };
    eventually("luneta goes to the folder's worktree", || used(in_below).map(drop));
    assert!(used(in_other).is_none(), "the project's other is not gone to");
    assert_eq!(projects_of(&owned).len(), 1);
}

#[test]
fn a_new_worktree_on_a_view_in_a_folder_of_a_project_shows_that_folder_in_the_project() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let feat = dir.path.parent().unwrap().join("feat");
    git(&dir.path.parent().unwrap().join(".bare"), &["worktree", "add", "-q", "../feat", "feat"]);
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    window.send(SPLIT);
    eventually("a second view opens in work", || {
        (shown(&owned, &dir.path)?.terminals.len() == 2).then_some(())
    });
    window.send(CD_FEAT);
    eventually("the view works in feat", || window.shows(" work/feat ").then_some(()));
    window.send("\x1b[97;3unw");
    eventually("luneta shows feat", || shown(&owned, &feat));
    let projects = projects_of(&owned);
    let folders: Vec<(&str, Vec<&str>)> = projects
        .iter()
        .map(|project| {
            let worktrees = project.worktrees().unwrap();
            let folders = worktrees.iter().map(|it| it.folder.as_str()).collect();
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
    let named = cloned("named", &[]);
    let made = project(&owned.host, Path::new(&real(&named.path)), None);
    let main = named.path.join("main");
    worktree(&owned.host, made, Path::new(&real(&main)));
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
fn a_named_luneta_in_a_clone_shows_the_worktree_of_the_branch_no_project_is_named() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let main = work_in(&owned, "luneta");
    let clone = main.path.parent().unwrap().to_owned();
    git(&clone.join(".bare"), &["update-ref", "refs/remotes/origin/main", "main"]);
    let window = Window::open(&owned.host, &main, &["fix/foo"]);
    let fix = clone.join("fix-foo");
    eventually("luneta shows fix-foo, the worktree it made", || shown(&owned, &fix));
    drop(window);
    let head = fs::read_to_string(clone.join(".bare/worktrees/fix-foo/HEAD")).unwrap();
    assert_eq!(head, "ref: refs/heads/fix/foo\n", "on a branch of its own");
    let names: Vec<String> = projects_of(&owned).into_iter().map(|it| it.name).collect();
    assert_eq!(names, ["luneta"], "no project is made of fix/foo");
}

#[test]
fn a_named_luneta_in_a_folder_project_says_the_folder_is_its_one_worktree() {
    let owned = Owned::new();
    let vault = directory("Vault");
    project(&owned.host, Path::new(&real(&vault.path)), None);
    let output = luneta(owned.host.vars(), &vault.path).arg(" foo ").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    let said = ": foo names no project, and Vault is a folder, and its own is its one worktree";
    assert!(refusal.contains(said), "{refusal}");
    let projects = projects_of(&owned);
    assert_eq!(projects.len(), 1, "no project is made of foo: {projects:?}");
}

#[test]
fn a_luneta_whose_terminal_variables_name_no_terminal_opens_nothing() {
    let owned = Owned::new();
    let main = work_in(&owned, "named");
    let mut luneta = luneta(owned.host.vars(), &main.path);
    let output = luneta.env("LUNETA_MUX_TERMINAL", "7").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let refusal = String::from_utf8(output.stderr).unwrap();
    assert!(refusal.contains("name no terminal: LUNETA_MUX_TERMINAL_MADE is unset"), "{refusal}");
    let projects = projects_of(&owned);
    assert_eq!(projects[0].worktrees().unwrap(), [], "luneta opened its folder's worktree");
}

#[test]
fn a_named_luneta_says_why_lunetad_keeps_no_more_projects_rather_than_go_to_another() {
    let owned = Owned::new();
    let kept = directory("kept");
    for at in 0..lunetad::projects::PROJECTS_MAX {
        let dir = kept.path.join(format!("p{at}"));
        fs::create_dir(&dir).unwrap();
        project(&owned.host, Path::new(&real(&dir)), None);
    }
    let dir = directory("new");
    let output = luneta(owned.host.vars(), &dir.path).arg("new").output().unwrap();
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
    cloned_as_wt(&outer.path, &outer.path.join("up"), &inner);
    let made = project(&owned.host, Path::new(&real(&inner)), None);
    let main = inner.join("main");
    worktree(&owned.host, made, Path::new(&real(&main)));
    let _window = Window::open(&owned.host, &outer, &[]);
    eventually("luneta shows inner", || shown(&owned, &main));
    assert_eq!(projects_of(&owned).len(), 1, "no project is made around another");
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
fn q_in_the_sidebar_quits_luneta_and_its_terminal_runs_on_for_the_next_one() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    let ids = |worktree: api::Worktree| -> Vec<u64> {
        worktree.terminals.iter().map(|it| it.id).collect()
    };
    let terminals = ids(eventually("luneta shows work", || shown(&owned, &dir.path)));
    window.send("printf 'be%s\\n' fore-42\r");
    eventually("the terminal runs what was typed", || window.shows("before-42").then_some(()));
    window.send("\x01q");
    let status = window.luneta.ended_within(ENDS).expect("q ended luneta");
    assert_eq!(status.code(), Some(0), "{}", window.screen().join("\n"));
    let log = log_of(&owned.host);
    assert!(log.contains("ends: asked to quit"), "{log}");
    let kept = shown(&owned, &dir.path).expect("the worktree keeps its terminal");
    assert_eq!(ids(kept), terminals);
    let again = Window::open(&owned.host, &dir, &[]);
    eventually("the next luneta shows the same terminal", || {
        again.shows("before-42").then_some(())
    });
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


