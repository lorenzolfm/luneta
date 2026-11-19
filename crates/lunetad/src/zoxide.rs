use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::{names, program, Directory, SessionName};

const ANSWER: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dir {
    pub path: Directory,
    pub name: SessionName,
}

pub fn visited() -> Result<Vec<Dir>, String> {
    let mut zoxide = Command::new("zoxide");
    zoxide.args(["query", "--list"]);
    let ran = program::run(&mut zoxide, ANSWER).map_err(|error| unrun(&error))?;
    let listed = ran.answer().map_err(|why| format!("zoxide: {why}"))?;
    Ok(dirs_in(&listed))
}

fn dirs_in(listed: &[u8]) -> Vec<Dir> {
    let dir = |line: &[u8]| {
        let path = Directory::parse(std::str::from_utf8(line).ok()?.to_owned()).ok()?;
        let name = names::session_for(Path::new(path.as_str()))?;
        Some(Dir { path, name })
    };
    listed.split(|byte| *byte == b'\n').filter_map(dir).collect()
}

fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zoxide is not on the PATH".to_owned();
    }
    format!("zoxide: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
