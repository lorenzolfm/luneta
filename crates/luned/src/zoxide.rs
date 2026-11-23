use std::io;
use std::process::Command;
use std::time::Duration;

use crate::{program, Directory};

const ANSWER: Duration = Duration::from_secs(2);

pub fn visited() -> Result<Vec<Directory>, String> {
    let mut zoxide = Command::new("zoxide");
    zoxide.args(["query", "--list"]);
    let ran = program::run(&mut zoxide, ANSWER).map_err(|error| unrun(&error))?;
    let listed = ran.answer().map_err(|why| format!("zoxide: {why}"))?;
    Ok(dirs_in(&listed))
}

fn dirs_in(listed: &[u8]) -> Vec<Directory> {
    let dir = |line: &[u8]| Directory::parse(std::str::from_utf8(line).ok()?.to_owned()).ok();
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
    fn a_line_that_names_no_absolute_utf_8_path_is_left_out() {
        let dir = |path: &str| Directory::parse(path.to_owned()).unwrap();
        let listed = b"/home/you/lune\n/\n\nlune\n/home/you/caf\xe9\n/home/you/-dash/\n";
        let dirs = [dir("/home/you/lune"), dir("/"), dir("/home/you/-dash/")];
        assert_eq!(dirs_in(listed), dirs);
    }
}
