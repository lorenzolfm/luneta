//! luneta attached to an own session, in zellij's place beside the sidebar: a
//! luneta on a terminal of the test's, with the real daemon and the real
//! `luneta-mux` it starts, whose pane runs your shell. What luneta draws is
//! read back as a terminal would show it.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use base64::Engine as _;
use lunetad::api::{kill_request, KillRequest, NewOwnRequest};

use crate::host::{eventually, Host, Running};
use crate::own::{own, Owned};
use crate::sessions::session;
use crate::starts::{directory, luneta, on_terminal, runs, spawn_on, terminal, Directory};

/// What a shell is typed, in any shell: it prints `own-42`, which what is
/// typed does not show.
const TYPED: &str = "printf 'own-%s\\n' 42\r";

/// What it prints.
const PRINTED: &str = "own-42";

/// A luneta on a terminal of the test's, and all it wrote there.
struct Window {
    typed: File,
    said: Arc<Mutex<Vec<u8>>>,
    luneta: Running,
}

impl Window {
    /// luneta, run in `dir` with `args`, once it draws its sidebar.
    fn open(host: &Host, dir: &Directory, args: &[&str]) -> Window {
        let (controller, tty) = terminal();
        let typed = File::from(controller.try_clone().unwrap());
        let said = Arc::new(Mutex::new(Vec::new()));
        let reading = Arc::clone(&said);
        // Read all along, so that nothing waits on a write.
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
        let mut command = luneta(host.vars(), &dir.path);
        command.args(args);
        let luneta = spawn_on(&tty, &mut command);
        drop(tty);
        let window = Window { typed, said, luneta };
        eventually("luneta draws its sidebar", || window.shows("─ luneta").then_some(()));
        window
    }

    fn send(&mut self, sent: &str) {
        self.typed.write_all(sent.as_bytes()).unwrap();
    }

    /// The lines luneta's terminal shows now, of 120 columns.
    fn screen(&self) -> Vec<String> {
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

    fn shows(&self, text: &str) -> bool {
        self.screen().iter().any(|line| line.contains(text))
    }

    /// Where `text` shows on the screen, as its row and the column it starts
    /// at, both from 0, once it does.
    fn spot_of(&self, text: &str) -> (usize, usize) {
        eventually(&format!("luneta shows {text:?}"), || {
            let screen = self.screen();
            screen.iter().enumerate().find_map(|(row, line)| {
                let at = line.find(text)?;
                Some((row, line[..at].chars().count()))
            })
        })
    }

    /// What luneta has asked its terminal to copy, each in its turn.
    fn copied(&self) -> Vec<String> {
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

/// How many clients the own session `name` has, as the host lists it.
fn clients(owned: &Owned, name: &str) -> Option<usize> {
    Some(session(&owned.host.host(), name)?.clients.clone()?.clients.len())
}

/// What a terminal sends for the left button at the cell at `row` and
/// `column`, from 0: `M` and 0 for a press, 32 for a drag, and `m` for a
/// release.
fn mouse(code: u8, (row, column): (usize, usize), last: char) -> String {
    format!("\x1b[<{code};{};{}{last}", column + 1, row + 1)
}

/// `luneta --own` attaches to the own session named for its directory,
/// beside the sidebar, which lists it: what is typed goes to the pane's
/// shell, and what the shell prints shows. A drag over it copies it, as the
/// shell takes no mouse. The pane runs `sh`: a build's sandbox has no login
/// shell of its user's to make one with.
#[test]
fn luneta_own_attaches_to_its_session_and_shows_what_its_shell_prints() {
    let owned = Owned::new();
    let request = NewOwnRequest {
        on: None,
        name: "work".to_owned(),
        cwd: owned.home(),
        command: vec!["sh".to_owned()],
    };
    owned.ask(|mut daemon| async move { daemon.new_own(request).await }).unwrap();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &["--own"]);
    eventually("luneta is the session's client", || (clients(&owned, "work")? == 1).then_some(()));
    assert!(own(&owned.listed("work")).is_some());
    eventually("the sidebar lists it", || {
        let screen = window.screen();
        screen.iter().any(|line| line.contains(" work ") && line.contains('◇')).then_some(())
    });

    window.send(TYPED);
    let (row, column) = window.spot_of(PRINTED);
    let end = (row, column + PRINTED.len() - 1);
    let swept = [mouse(0, (row, column), 'M'), mouse(32, end, 'M'), mouse(0, end, 'm')];
    window.send(&swept.concat());
    eventually("the drag copies what it went over", || {
        window.copied().contains(&PRINTED.to_owned()).then_some(())
    });
}

/// A session of zellij's has the name `luneta --own` would take, and luneta
/// makes no own session of it: the two kinds share their names.
#[test]
fn luneta_own_refuses_a_name_a_session_of_zellij_s_has() {
    let owned = Owned::new();
    let _project = runs(&owned.host, "project");
    let dir = directory("project");
    let mut command = luneta(owned.host.vars(), &dir.path);
    command.arg("--own");
    let ended = on_terminal(command);
    assert!(!ended.status.success(), "{}", ended.said);
    assert!(ended.said.contains("project is a session of zellij's"), "{}", ended.said);
    let host = owned.host.host();
    let named = host.sessions.iter().filter(|session| session.name == "project");
    assert!(named.clone().all(|session| own(session).is_none()), "{:?}", host.sessions);
    // As the daemon has asked zellij for them by now, or has yet to.
    assert_eq!(clients(&owned, "project").unwrap_or(0), 0);
}

/// As the own session luneta is in ends, the newest session of this machine
/// takes its place, of either kind: an own one newer than zellij's.
#[test]
fn the_newest_session_takes_the_place_of_an_own_one_that_ended() {
    let owned = Owned::new();
    let _zellij = runs(&owned.host, "older");
    owned.make("first").unwrap();
    let dir = directory("anywhere");
    let mut window = Window::open(&owned.host, &dir, &["--own", "first"]);
    eventually("luneta is in first", || (clients(&owned, "first")? == 1).then_some(()));
    owned.make("newer").unwrap();
    let first = own(&owned.listed("first")).unwrap();
    let request = KillRequest {
        on: None,
        session: "first".to_owned(),
        whose: Some(kill_request::Whose::Own(first)),
    };
    owned.ask(|mut daemon| async move { daemon.kill(request).await }).unwrap();
    eventually("luneta goes on in newer", || (clients(&owned, "newer")? == 1).then_some(()));
    assert!(owned.host.zellij.attached().is_empty(), "{:?}", owned.host.zellij.attached());
    assert!(window.luneta.ended_within(Duration::ZERO).is_none(), "luneta goes on");
}

/// luneta goes from a session of zellij's to an own one, as the picker picks
/// it, and back: its own client takes the place of zellij's, and zellij's the
/// place of its own, which the own session lets go of.
#[test]
fn luneta_goes_from_zellij_s_session_to_an_own_one_and_back() {
    let owned = Owned::new();
    owned.host.zellij.listen();
    let _project = runs(&owned.host, "project");
    owned.make("mine").unwrap();
    owned.listed("mine");
    let dir = directory("project");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("the sidebar lists mine", || window.shows("mine").then_some(()));
    // `Ctrl j`, as a terminal that speaks kitty's keys sends it.
    window.send("\x1b[106;5umine\r");
    eventually("luneta is mine's client", || (clients(&owned, "mine")? == 1).then_some(()));
    window.send("\x1b[106;5uproject\r");
    eventually("zellij's client attaches to project again", || {
        (owned.host.zellij.attached() == ["attach project", "attach project"]).then_some(())
    });
    eventually("mine lets go of luneta", || (clients(&owned, "mine")? == 0).then_some(()));
}

/// `d s` on the own session luneta is in takes luneta to the newest other
/// session, an own one here, on the stream it has, and kills the session once
/// luneta is there.
#[test]
fn a_delete_of_the_own_session_luneta_is_in_goes_on_in_the_newest_and_kills_it() {
    let owned = Owned::new();
    owned.make("first").unwrap();
    let dir = directory("anywhere");
    let mut window = Window::open(&owned.host, &dir, &["--own", "first"]);
    eventually("luneta is in first", || (clients(&owned, "first")? == 1).then_some(()));
    owned.make("newer").unwrap();
    eventually("the sidebar lists newer", || window.shows("newer").then_some(()));
    // `Ctrl a`, which gives the sidebar the keys, on the row of luneta's pane.
    window.send("\x1b[97;5uds");
    eventually("luneta goes on in newer", || (clients(&owned, "newer")? == 1).then_some(()));
    eventually("first is gone", || session(&owned.host.host(), "first").is_none().then_some(()));
    assert!(window.luneta.ended_within(Duration::ZERO).is_none(), "luneta goes on");
}
