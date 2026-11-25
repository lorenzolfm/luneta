use std::sync::mpsc::Sender;
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use luned::Destination;

use crate::clipboard;
use crate::host::Event;
use crate::machines::Machine;
use crate::reach::{self, Luned};

pub const KEY: (KeyCode, KeyModifiers) = (KeyCode::Char('v'), KeyModifiers::CONTROL);

#[derive(Debug)]
pub enum Went {
    NoImage { key: KeyEvent, on: Destination },
    Kept { on: Destination, path: String },
    Failed(String),
}

#[derive(Default)]
pub struct Going(bool);

impl Going {
    pub fn send(&mut self, on: Destination, key: KeyEvent, events: &Sender<Event>) -> bool {
        if self.0 || key.kind != KeyEventKind::Press {
            return false;
        }
        self.0 = true;
        let events = events.clone();
        thread::spawn(move || {
            let went = went(on, key, &events);
            let _ = events.send(Event::Image(went));
        });
        true
    }

    pub fn heard(&mut self) {
        self.0 = false;
    }
}

fn went(on: Destination, key: KeyEvent, events: &Sender<Event>) -> Went {
    let png = match clipboard::image() {
        Ok(Some(png)) => png,
        Ok(None) => return Went::NoImage { key, on },
        Err(why) => return Went::Failed(why),
    };
    let slow = || {
        let _ = events.send(Event::ImageSlow(on.clone()));
    };
    let luned = Luned::on(&Machine::Remote(on.clone()), reach::ANSWER);
    let kept = luned.and_then(|mut luned| luned.keep_image(png, slow));
    match kept {
        Ok(path) => Went::Kept { on, path },
        Err(why) => Went::Failed(why),
    }
}
