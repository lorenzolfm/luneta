use std::fmt;
use std::num::NonZeroU32;

use crate::v1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unread(pub NonZeroU32);

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "luned cannot read {} of Claude Code's session files", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unplaced {
    unread: NonZeroU32,
    unplaced: NonZeroU32,
}

impl Unplaced {
    #[must_use]
    pub fn new(unread: NonZeroU32, unplaced: NonZeroU32) -> Self {
        assert!(unplaced <= unread, "{unplaced} files unplaced of {unread} unread");
        Unplaced { unread, unplaced }
    }
}

impl fmt::Display for Unplaced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unplaced { unread, unplaced } = *self;
        let runs = match unplaced.get() {
            1 if unread.get() == 1 => "its agent runs".to_owned(),
            _ if unplaced == unread => "their agents run".to_owned(),
            1 => "1 of their agents runs".to_owned(),
            some => format!("{some} of their agents run"),
        };
        write!(f, "{}, nor tell where {runs}", Unread(unread))
    }
}

impl v1::Host {
    #[must_use]
    pub fn unread_count(&self) -> Option<Unread> {
        NonZeroU32::new(self.unread).map(Unread)
    }
}

impl v1::ListAgentsResponse {
    #[must_use]
    pub fn unread_count(&self) -> Option<Unread> {
        NonZeroU32::new(self.unread).map(Unread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unplaced(unread: u32, unplaced: u32) -> Unplaced {
        let count = |count| NonZeroU32::new(count).unwrap();
        Unplaced::new(count(unread), count(unplaced))
    }

    #[test]
    fn it_says_how_many_files_it_cannot_read_and_place() {
        let one = NonZeroU32::new(1).unwrap();
        let said = "luned cannot read 1 of Claude Code's session files";
        assert_eq!(Unread(one).to_string(), said);
        assert_eq!(unplaced(1, 1).to_string(), format!("{said}, nor tell where its agent runs"));
        let two = "luned cannot read 2 of Claude Code's session files, nor tell where";
        assert_eq!(unplaced(2, 2).to_string(), format!("{two} their agents run"));
        assert_eq!(unplaced(2, 1).to_string(), format!("{two} 1 of their agents runs"));
        let three = "luned cannot read 3 of Claude Code's session files, nor tell where";
        assert_eq!(unplaced(3, 2).to_string(), format!("{three} 2 of their agents run"));
    }
}
