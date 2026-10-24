//! The word the sidebar's menu leaves as it takes you into the sidebar of
//! another session, before it kills the one you are in.
//!
//! zellij runs the sidebar once for each client. A client that comes to a
//! session has a sidebar of its own there: a new one, or the one a client that
//! left had. No pipe reaches the new one before you come, and a pipe to the
//! session reaches the sidebars of the clients already in it too, which would
//! each take the focus for their own. So the menu leaves word where the
//! sidebars of every session read it, a file in the cache zellij gives the
//! plugin, and then switches you. The sidebar the focus lands in, with no key
//! of its session that asked for it, keeps the focus when the word names it.

use std::fs;
use std::path::PathBuf;

use crate::elapsed::{Held, Moment};

const FILE: &str = "/cache/coming";

/// How long the word holds: you come to another session in well under a
/// second, and the sidebar zellij starts for you there loads first.
const HOLDS: Held = Held::from_secs(5);

pub struct Coming {
    file: PathBuf,
}

impl Coming {
    pub fn shared() -> Self {
        Coming::at(PathBuf::from(FILE))
    }

    pub fn at(file: PathBuf) -> Self {
        Coming { file }
    }

    /// Says that you are on your way into `sidebar`, the pane of the sidebar
    /// of `session`, `now`. A word that fails to go out leaves you where the
    /// session had you.
    pub fn say(&self, session: &str, sidebar: u32, now: Moment) {
        if let Err(error) = fs::write(&self.file, format!("{} {sidebar} {session}\n", now.millis())) {
            let file = self.file.display();
            eprintln!("luneta: no word goes to the sidebar of {session}: {file}: {error}");
        }
    }

    /// The session whose sidebar in pane `sidebar` the word names, while it
    /// holds: plugin panes of two sessions can have the same id. A file
    /// halfway written says nothing.
    pub fn heard(&self, sidebar: u32, now: Moment) -> Option<String> {
        let text = fs::read_to_string(&self.file).ok()?;
        let (said, rest) = text.trim_end_matches('\n').split_once(' ')?;
        let (pane, session) = rest.split_once(' ')?;
        let said = Moment::from_millis(said.parse().ok()?);
        let named = pane.parse::<u32>().ok() == Some(sidebar);
        (named && now.since(said) <= HOLDS).then(|| session.to_owned())
    }

    /// Takes the word back, once the sidebar it was for has kept the focus.
    pub fn forget(&self) {
        let _ = fs::remove_file(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word names the sidebar of a session, and holds only so long. A
    /// session's name can have spaces in it.
    #[test]
    fn the_word_names_a_sidebar_for_a_few_seconds() {
        let file = std::env::temp_dir().join(format!("luneta-coming-{}", std::process::id()));
        let _ = fs::remove_file(&file);
        let coming = Coming::at(file.clone());
        let now = Moment::from_millis(1_000_000);
        assert_eq!(coming.heard(3, now), None);

        coming.say("my elo", 3, now);
        assert_eq!(coming.heard(3, now), Some("my elo".to_owned()));
        assert_eq!(coming.heard(4, now), None);
        let later = |millis| Moment::from_millis(now.millis() + millis);
        assert_eq!(coming.heard(3, later(5_000)), Some("my elo".to_owned()));
        assert_eq!(coming.heard(3, later(5_001)), None);

        coming.forget();
        assert_eq!(coming.heard(3, now), None);
        fs::write(&file, "1000000 3").unwrap();
        assert_eq!(coming.heard(3, now), None);
        fs::remove_file(file).unwrap();
    }
}
