use std::os::unix::net::UnixListener;

use crate::host::eventually;
use crate::own::Owned;
use crate::starts::{directory, goes_to_zellij_s, runs, Directory, Window};

const TITLE_START: (u16, u16) = (2, 0);
const TITLE_END: (u16, u16) = (7, 0);
const OVER_ZELLIJ: (u16, u16) = (60, 5);
const FURTHER_ON: (u16, u16) = (62, 6);

const ESC: &str = "\x1b[27u";
const SWAP: &str = "\x1b[97;5u";

const END: &str = "k";

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

struct InZellij {
    window: Window,
    owned: Owned,
    _session: UnixListener,
    _directory: Directory,
}

impl InZellij {
    fn open() -> InZellij {
        let owned = Owned::new();
        owned.host.zellij.listen();
        let session = runs(&owned.host, "project");
        owned.make("mine").unwrap();
        let dir = directory("anywhere");
        let mut window = Window::open(&owned.host, &dir, &["mine"]);
        goes_to_zellij_s(&owned, &mut window, "mine", "project");
        let mut in_zellij = InZellij { window, owned, _session: session, _directory: dir };
        eventually("zellij's client hears the mouse", || {
            in_zellij.window.send(&format!("{}{}", press(OVER_ZELLIJ), release(OVER_ZELLIJ)));
            in_zellij.owned.host.zellij.heard().contains("\x1b[<0;").then_some(())
        });
        in_zellij.heard("");
        in_zellij
    }

    fn copies(&self) -> usize {
        self.window.copied().len()
    }

    fn heard(&mut self, sent: &str) -> String {
        let zellij = &self.owned.host.zellij;
        let before = zellij.heard().len();
        self.window.send(&format!("{sent}{END}"));
        eventually("zellij's client hears the key after it", || {
            let heard = zellij.heard();
            (heard.len() > before && heard.ends_with(END)).then(|| heard[before..].to_owned())
        })
    }
}

#[test]
fn zellij_hears_nothing_of_a_sweep_on_the_sidebar() {
    let mut in_zellij = InZellij::open();

    let along = [press(TITLE_START), drag(TITLE_END), release(TITLE_END)].concat();
    assert_eq!(in_zellij.heard(&along), END);
    assert_eq!(in_zellij.window.copied(), ["luneta"]);

    let over = [press(TITLE_START), drag(TITLE_END), drag(OVER_ZELLIJ), release(OVER_ZELLIJ)];
    assert_eq!(in_zellij.heard(&over.concat()), END);
    assert_eq!(in_zellij.copies(), 2);

    let keys = [press(TITLE_START), drag(TITLE_END), "x".to_owned(), ESC.to_owned()].concat();
    assert_eq!(in_zellij.heard(&keys), END);
    assert_eq!(in_zellij.copies(), 2);
}

#[test]
fn a_press_let_go_of_with_the_button_down_is_no_press_of_zellij_s() {
    let mut in_zellij = InZellij::open();
    for (case, before, lets_go) in [("Esc", "", ESC), ("the keys leaving", SWAP, SWAP)] {
        let swept = [press(TITLE_START), drag(TITLE_END), drag(OVER_ZELLIJ)].concat();
        let rest = [drag(FURTHER_ON), release(FURTHER_ON)].concat();
        let sent = format!("{before}{swept}{lets_go}{rest}");
        assert_eq!(in_zellij.heard(&sent), END, "{case}");
    }
    assert_eq!(in_zellij.copies(), 0);
}

#[test]
fn zellij_hears_a_button_it_heard_go_down_to_its_release() {
    let mut in_zellij = InZellij::open();
    let right = [report(2, OVER_ZELLIJ, 'M'), report(2, OVER_ZELLIJ, 'm')].concat();
    let sent = [press(OVER_ZELLIJ), right, drag(FURTHER_ON), release(FURTHER_ON)].concat();
    let heard = in_zellij.heard(&sent);
    assert_eq!(heard.matches("\x1b[<32;").count(), 1, "{heard:?}");
    assert_eq!(heard.matches('m').count(), 2, "{heard:?}");
}

#[test]
fn a_window_that_loses_the_focus_lets_go_of_a_press() {
    let mut in_zellij = InZellij::open();
    let lost = [press(TITLE_START), "\x1b[O".to_owned(), "x".to_owned()].concat();
    let back = ["\x1b[I".to_owned(), drag(OVER_ZELLIJ), release(OVER_ZELLIJ)].concat();
    let heard = in_zellij.heard(&format!("{lost}{back}"));
    assert_eq!(heard, format!("x{END}"));
}
