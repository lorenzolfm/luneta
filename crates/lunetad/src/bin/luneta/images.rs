//! `Ctrl v` for an agent on another machine. Claude Code reads the clipboard
//! of the machine it runs on, and your screenshot is on the one you sit at:
//! luneta reads it there, has the daemon of the agent's machine keep it in a
//! file, as `lunetad::images` does, and pastes the file's path on the agent's
//! prompt, which Claude Code takes for an image at once.

use std::sync::mpsc::Sender;
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use lunetad::Destination;

use crate::away::{self, Away};
use crate::clipboard;
use crate::host::Event;

/// The key that sends the image on the clipboard: Claude Code's own for an
/// image, which the pane has where there is none.
pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('v'), KeyModifiers::CONTROL);

/// What came of the image `KEY` was to paste.
#[derive(Debug)]
pub enum Went {
    /// The clipboard holds none, and this key goes on to the pane.
    NoImage(KeyEvent),
    /// The daemon of the machine `on` keeps it in the file `path`.
    Kept { on: Destination, path: String },
    /// Why it was not kept.
    Failed(String),
}

/// Whether an image `KEY` sent has yet to be heard of. One goes at a time, so
/// a key held down, or pressed again on a slow line, sends no more.
#[derive(Default)]
pub struct Going(bool);

impl Going {
    /// Sends the image on the clipboard to the daemon of the machine `on`,
    /// for `key` pressed, while none goes: on a thread of its own, as neither
    /// the clipboard's tools nor a slow line are for luneta's screen to wait
    /// on. `events` hears what comes of it.
    pub fn send(&mut self, on: Destination, key: KeyEvent, events: &Sender<Event>) {
        if self.0 || key.kind != KeyEventKind::Press {
            return;
        }
        self.0 = true;
        let events = events.clone();
        thread::spawn(move || {
            let went = went(on, key, &events);
            // luneta is on its way out.
            let _ = events.send(Event::Image(went));
        });
    }

    /// What came of the one that went has been heard: another may go.
    pub fn heard(&mut self) {
        self.0 = false;
    }
}

/// What comes of the image on the clipboard, as `key` sends it to the daemon
/// of the machine `on`, which `events` hears of once that takes a while.
fn went(on: Destination, key: KeyEvent, events: &Sender<Event>) -> Went {
    let png = match clipboard::image() {
        Ok(Some(png)) => png,
        Ok(None) => return Went::NoImage(key),
        Err(why) => return Went::Failed(why),
    };
    let slow = || {
        // luneta is on its way out.
        let _ = events.send(Event::ImageSlow(on.clone()));
    };
    let kept = Away::reach(&on, away::ANSWER).and_then(|mut away| away.keep_image(png, slow));
    match kept {
        Ok(path) => Went::Kept { on, path },
        Err(why) => Went::Failed(why),
    }
}
