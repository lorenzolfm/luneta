use std::fmt;
use std::num::NonZeroU32;

use crate::v1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unread(pub NonZeroU32);

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lunetad cannot read {} of Claude Code's session files", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unplaced {
    unread: NonZeroU32,
    unplaced: NonZeroU32,
}

impl Unplaced {
    fn new(unread: NonZeroU32, unplaced: NonZeroU32) -> Self {
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

    pub fn unread_seats(&self) -> Result<Vec<(&str, u32)>, Unplaced> {
        seats(self.unread, &self.unread_files)
    }
}

impl v1::ListAgentsResponse {
    #[must_use]
    pub fn unread_count(&self) -> Option<Unread> {
        NonZeroU32::new(self.unread).map(Unread)
    }

    pub fn unread_seats(&self) -> Result<Vec<(&str, u32)>, Unplaced> {
        seats(self.unread, &self.unread_files)
    }
}

pub fn seats(unread: u32, files: &[v1::UnreadFile]) -> Result<Vec<(&str, u32)>, Unplaced> {
    let listed = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let unread = unread.max(listed);
    let mut unplaced = unread - listed;
    let mut seats = Vec::with_capacity(files.len());
    for file in files {
        let Some(zellij) = &file.zellij else {
            continue;
        };
        match zellij.session.as_deref() {
            Some(session) => seats.push((session, zellij.pane)),
            None => unplaced += 1,
        }
    }
    let Some(unplaced) = NonZeroU32::new(unplaced) else {
        return Ok(seats);
    };
    let unread = NonZeroU32::new(unread).expect("a file it cannot place is one it cannot read");
    Err(Unplaced::new(unread, unplaced))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(pid: u32, zellij: Option<(&str, Option<&str>, u32)>) -> v1::UnreadFile {
        let zellij = zellij.map(|(named, session, pane)| v1::Zellij {
            named: named.to_owned(),
            session: session.map(str::to_owned),
            pane,
        });
        v1::UnreadFile { pid, zellij }
    }

    fn unplaced(unread: u32, unplaced: u32) -> Unplaced {
        let count = |count| NonZeroU32::new(count).unwrap();
        Unplaced::new(count(unread), count(unplaced))
    }

    #[test]
    fn a_file_is_where_its_process_runs_unless_lunetad_cannot_follow_it() {
        let placed = [file(10, Some(("work", Some("play"), 3))), file(11, None)];
        assert_eq!(seats(2, &placed), Ok(vec![("play", 3)]));
        assert_eq!(seats(0, &[]), Ok(Vec::new()));

        let lost = [file(10, Some(("work", Some("work"), 3))), file(12, Some(("work", None, 4)))];
        assert_eq!(seats(2, &lost), Err(unplaced(2, 1)));
        assert_eq!(seats(3, &[]), Err(unplaced(3, 3)));
        assert_eq!(seats(3, &lost), Err(unplaced(3, 2)));
        assert_eq!(seats(1, &lost), Err(unplaced(2, 1)));
    }

    #[test]
    fn it_says_how_many_files_it_cannot_read_and_place() {
        let one = NonZeroU32::new(1).unwrap();
        let said = "lunetad cannot read 1 of Claude Code's session files";
        assert_eq!(Unread(one).to_string(), said);
        assert_eq!(unplaced(1, 1).to_string(), format!("{said}, nor tell where its agent runs"));
        let two = "lunetad cannot read 2 of Claude Code's session files, nor tell where";
        assert_eq!(unplaced(2, 2).to_string(), format!("{two} their agents run"));
        assert_eq!(unplaced(2, 1).to_string(), format!("{two} 1 of their agents runs"));
        let three = "lunetad cannot read 3 of Claude Code's session files, nor tell where";
        assert_eq!(unplaced(3, 2).to_string(), format!("{three} 2 of their agents run"));
    }
}
