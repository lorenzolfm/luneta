//! luneta attached to an own session, in zellij's place beside the sidebar: a
//! luneta on a terminal of the test's, with the real daemon and the real
//! `luneta-mux` it starts, whose pane runs your shell. What luneta draws is
//! read back as a terminal would show it.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

use crate::host::{eventually, Host, Running};
use crate::own::Owned;
use crate::sessions::session;
use crate::starts::{directory, luneta, runs, spawn_on, terminal, Directory};

/// A luneta on a terminal of the test's, and all it wrote there.
struct Window {
    typed: File,
    said: Arc<Mutex<Vec<u8>>>,
    _luneta: Running,
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
        let window = Window { typed, said, _luneta: luneta };
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

}

/// How many clients the own session `name` has, as the host lists it.
fn clients(owned: &Owned, name: &str) -> Option<usize> {
    Some(session(&owned.host.host(), name)?.clients.clone()?.clients.len())
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
