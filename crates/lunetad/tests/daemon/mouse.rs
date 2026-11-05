//! What zellij hears of the mouse and the keys as a press goes down on the
//! sidebar: a luneta on a terminal of the test's, which sends what a terminal
//! sends, beside a client of zellij's that notes all it hears.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::host::{eventually, Host, Running};
use crate::starts::{directory, luneta, runs, spawn_on, terminal, Directory};

/// Cells of the sidebar's title, `─ luneta ─`: its first letter and its last,
/// further apart than a click's slack.
const TITLE_START: (u16, u16) = (2, 0);
const TITLE_END: (u16, u16) = (7, 0);
/// Cells of zellij's, past the sidebar.
const OVER_ZELLIJ: (u16, u16) = (60, 5);
const FURTHER_ON: (u16, u16) = (62, 6);

/// What a terminal sends for `Esc`, and for `Ctrl a`, which gives the keys to
/// the sidebar and back.
const ESC: &str = "\x1b[27u";
const SWAP: &str = "\x1b[97;5u";

/// A key, which a client hearing it tells that it heard all sent before it.
const END: &str = "k";

/// What a terminal sends for `code` at a cell, ending in `last`: `m` for a release.
fn report(code: u8, (x, y): (u16, u16), last: char) -> String {
    format!("\x1b[<{code};{};{}{last}", x + 1, y + 1)
}

fn press(at: (u16, u16)) -> String {
    report(0, at, 'M')
}

fn drag(at: (u16, u16)) -> String {
    report(32, at, 'M')
}

fn release(at: (u16, u16)) -> String {
    report(0, at, 'm')
}

/// A luneta on a terminal of the test's, drawn, beside a client of zellij's
/// that hears the mouse.
struct Window {
    typed: File,
    said: Arc<Mutex<Vec<u8>>>,
    _luneta: Running,
    _session: UnixListener,
    _directory: Directory,
}

impl Window {
    fn open(host: &Host) -> Window {
        host.zellij.listen();
        let session = runs(host, "project");
        let dir = directory("project");
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
        let running = spawn_on(&tty, &mut command);
        drop(tty);
        let mut window =
            Window { typed, said, _luneta: running, _session: session, _directory: dir };
        eventually("luneta draws its sidebar", || window.said().contains("─ luneta").then_some(()));
        // A click over zellij, again until its client has taken the mouse.
        eventually("zellij's client hears the mouse", || {
            window.send(&format!("{}{}", press(OVER_ZELLIJ), release(OVER_ZELLIJ)));
            host.zellij.heard().contains("\x1b[<0;").then_some(())
        });
        window.heard(host, "");
        window
    }

    fn send(&mut self, sent: &str) {
        self.typed.write_all(sent.as_bytes()).unwrap();
    }

    fn said(&self) -> String {
        String::from_utf8_lossy(&self.said.lock().unwrap()).into_owned()
    }

    /// How many times luneta has asked its terminal to copy something.
    fn copies(&self) -> usize {
        self.said().matches("\x1b]52;c;").count()
    }

    /// What zellij's client hears of `sent`, and of `END` after it.
    fn heard(&mut self, host: &Host, sent: &str) -> String {
        let before = host.zellij.heard().len();
        self.send(&format!("{sent}{END}"));
        eventually("zellij's client hears the key after it", || {
            let heard = host.zellij.heard();
            (heard.len() > before && heard.ends_with(END)).then(|| heard[before..].to_owned())
        })
    }
}

/// A sweep is copied on luneta's terminal. zellij hears nothing of it, as it
/// goes over zellij and lets go there too, nor of a key pressed along the way:
/// the `Esc` that would let go of it can interrupt an agent in a pane.
#[test]
fn zellij_hears_nothing_of_a_sweep_on_the_sidebar() {
    let host = Host::new();
    let mut window = Window::open(&host);

    let along = [press(TITLE_START), drag(TITLE_END), release(TITLE_END)].concat();
    assert_eq!(window.heard(&host, &along), END);
    // `luneta`, as OSC 52 spells it.
    assert!(window.said().contains("\x1b]52;c;bHVuZXRh\x07"));

    let over = [press(TITLE_START), drag(TITLE_END), drag(OVER_ZELLIJ), release(OVER_ZELLIJ)];
    assert_eq!(window.heard(&host, &over.concat()), END);
    assert_eq!(window.copies(), 2);

    let keys = [press(TITLE_START), drag(TITLE_END), "x".to_owned(), ESC.to_owned()].concat();
    assert_eq!(window.heard(&host, &keys), END);
    assert_eq!(window.copies(), 2);
}

/// A press the sidebar let go of with the button still down, by `Esc` or by the
/// keys leaving it, is no press of zellij's as the hand goes on over it and lets
/// go there: zellij takes the first drag it hears of a button it did not hear go
/// down for a press there, and would click in the pane, focus it, or select what
/// the release then copies, over what `Esc` was to leave on the clipboard.
#[test]
fn a_press_let_go_of_with_the_button_down_is_no_press_of_zellij_s() {
    let host = Host::new();
    let mut window = Window::open(&host);
    for (case, before, lets_go) in [("Esc", "", ESC), ("the keys leaving", SWAP, SWAP)] {
        let swept = [press(TITLE_START), drag(TITLE_END), drag(OVER_ZELLIJ)].concat();
        let rest = [drag(FURTHER_ON), release(FURTHER_ON)].concat();
        let sent = format!("{before}{swept}{lets_go}{rest}");
        assert_eq!(window.heard(&host, &sent), END, "{case}");
    }
    assert_eq!(window.copies(), 0);
}

/// zellij hears all a button does that it heard go down, though another button
/// came up meanwhile: left with the first held, a program in a pane would drag
/// on after it let go.
#[test]
fn zellij_hears_a_button_it_heard_go_down_to_its_release() {
    let host = Host::new();
    let mut window = Window::open(&host);
    let right = [report(2, OVER_ZELLIJ, 'M'), report(2, OVER_ZELLIJ, 'm')].concat();
    let sent = [press(OVER_ZELLIJ), right, drag(FURTHER_ON), release(FURTHER_ON)].concat();
    let heard = window.heard(&host, &sent);
    assert_eq!(heard.matches("\x1b[<32;").count(), 1, "{heard:?}");
    assert_eq!(heard.matches('m').count(), 2, "{heard:?}");
}

/// A window losing the focus lets go of a press on the sidebar, whose release
/// may go elsewhere and never come: the keys go to zellij again, with the
/// pointer still, and zellij hears nothing of the rest of the press.
#[test]
fn a_window_that_loses_the_focus_lets_go_of_a_press() {
    let host = Host::new();
    let mut window = Window::open(&host);
    let lost = [press(TITLE_START), "\x1b[O".to_owned(), "x".to_owned()].concat();
    let back = ["\x1b[I".to_owned(), drag(OVER_ZELLIJ), release(OVER_ZELLIJ)].concat();
    let heard = window.heard(&host, &format!("{lost}{back}"));
    assert_eq!(heard, format!("x{END}"));
}
