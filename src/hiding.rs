//! Whether the sidebar is hidden, the same in every session.
//!
//! Each session has a dock of its own, which hides its sidebar and brings it
//! back. It keeps what it was last asked for where the docks of the other
//! sessions read it: a file in the cache zellij gives the plugin, the folder
//! the folds are kept in too. A dock takes up only what another session said.
//! What its own said came to it as a key, which hides or shows by turns, and
//! taken up from the file as well, the key would undo itself. So the file says
//! whose it is, by the id of the session's zellij server: one process to a
//! session, the same for every dock in it.

use std::fs;
use std::path::PathBuf;

const FILE: &str = "/cache/hidden";

const HIDDEN: &str = "hidden";

const SHOWN: &str = "shown";

/// What a dock last asked for, and the server of its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Said {
    pub hidden: bool,
    pub by: u32,
}

#[derive(Default)]
pub struct Hiding {
    /// Where it is kept. Without one, hiding is this session's alone: a
    /// test's, or one whose file failed it.
    file: Option<PathBuf>,
}

impl Hiding {
    pub fn shared() -> Self {
        Hiding::at(PathBuf::from(FILE))
    }

    pub fn at(file: PathBuf) -> Self {
        Hiding { file: Some(file) }
    }

    /// What was said last, if anything was. A file halfway written says
    /// nothing.
    pub fn read(&self) -> Option<Said> {
        let text = fs::read_to_string(self.file.as_ref()?).ok()?;
        let (hidden, by) = text.trim().split_once(' ')?;
        let hidden = match hidden {
            HIDDEN => true,
            SHOWN => false,
            _ => return None,
        };
        Some(Said { hidden, by: by.parse().ok()? })
    }

    /// Says it for every session. A file that fails a write is given up, so
    /// that this session does not go on taking up what the others say while
    /// they never hear from it.
    pub fn write(&mut self, said: Said) {
        let Some(file) = &self.file else {
            return;
        };
        let hidden = if said.hidden { HIDDEN } else { SHOWN };
        if let Err(error) = fs::write(file, format!("{hidden} {}\n", said.by)) {
            let file = file.display();
            eprintln!("luneta: the sidebar hides in this session alone: {file}: {error}");
            self.file = None;
        }
    }
}
