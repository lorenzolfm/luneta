//! What zoxide knows of the machine it runs on: the directories you go to,
//! as it ranks them, for luneta's picker, and for a clone of a pasted link's
//! repository. luneta asks it of its own machine, and a daemon of its
//! machine for a luneta on another, whose link was pasted on a row of this
//! one.

use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::{names, program, Directory, SessionName};

/// How long zoxide has to list the directories.
const ANSWER: Duration = Duration::from_secs(2);

/// A directory zoxide knows, and the session named for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dir {
    pub path: Directory,
    pub name: SessionName,
}

/// The directories zoxide knows, best first, each with the session named
/// for it, as `names::session_for` names it: for its repository too where it
/// is a worktree of one. One no session can be named for, as `/`, is left
/// out.
pub fn visited() -> Result<Vec<Dir>, String> {
    let mut zoxide = Command::new("zoxide");
    zoxide.args(["query", "--list"]);
    let ran = program::run(&mut zoxide, ANSWER).map_err(|error| unrun(&error))?;
    let listed = ran.answer().map_err(|why| format!("zoxide: {why}"))?;
    Ok(dirs_in(&listed))
}

/// The directories zoxide knows, best first, each with the name a session
/// luneta makes there takes now: the one `visited` gives it, with a `-2`
/// where a session has that name, as `names::free_for` names one. Or why
/// zoxide did not list them, or why luneta cannot tell which names zellij
/// has.
pub fn visited_free() -> Result<Vec<Dir>, String> {
    let visited = visited()?;
    let sessions = names::named()?;
    let names: Vec<&str> = sessions.iter().map(SessionName::as_str).collect();
    let free = |dir: Dir| Dir { name: dir.name.free(&names), path: dir.path };
    Ok(visited.into_iter().map(free).collect())
}

/// The directories of what `zoxide query --list` printed, a path a line. A
/// line that is no absolute path is no directory of zoxide's, and would be
/// read from the asker's own. One that is no UTF-8 is of a directory no
/// session can be named for, as `session_for` has it, and made UTF-8 it
/// would name another. A path with a newline in it reads as two: zoxide
/// lists no other way.
fn dirs_in(listed: &[u8]) -> Vec<Dir> {
    let dir = |line: &[u8]| {
        let path = Directory::parse(std::str::from_utf8(line).ok()?.to_owned()).ok()?;
        let name = names::session_for(Path::new(path.as_str()))?;
        Some(Dir { path, name })
    };
    listed.split(|byte| *byte == b'\n').filter_map(dir).collect()
}

/// Why zoxide could not be run.
fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zoxide is not on the PATH".to_owned();
    }
    format!("zoxide: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of zoxide's that is no absolute path, or the path of a
    /// directory no session can be named for, as `/`, or as one that is no
    /// UTF-8, is no directory of the list.
    #[test]
    fn a_line_that_names_no_directory_a_session_can_be_made_in_is_left_out() {
        let dir = |path: &str, name: &str| Dir {
            path: Directory::parse(path.to_owned()).unwrap(),
            name: SessionName::parse(name).unwrap(),
        };
        let listed = b"/home/you/luneta\n/\n\nluneta\n/home/you/caf\xe9\n/home/you/notes/\n";
        let dirs = [dir("/home/you/luneta", "luneta"), dir("/home/you/notes/", "notes")];
        assert_eq!(dirs_in(listed), dirs);
    }
}
