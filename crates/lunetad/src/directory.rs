//! A directory of a machine's, by its absolute path, as luneta hands one to
//! eza, to zellij, or to the daemon of another machine.

use std::fmt;

/// A directory by its absolute path, on the machine it is of. `parse` is the
/// only way to make one, so a path that reaches eza, zellij or another
/// machine's daemon as one is never read against the reader's own directory,
/// which is no one's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Directory(String);

impl Directory {
    /// `path`, as a directory by its absolute path, or why it is none.
    pub fn parse(path: String) -> Result<Self, String> {
        if path.starts_with('/') {
            return Ok(Directory(path));
        }
        Err(format!("{path:?} is no absolute path"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Directory> for String {
    fn from(directory: Directory) -> Self {
        directory.0
    }
}
