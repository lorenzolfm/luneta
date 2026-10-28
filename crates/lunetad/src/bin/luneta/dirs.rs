//! What luneta asks of zoxide and of eza, for the picker's directories: the
//! directories you go to, as zoxide ranks them, and what is in one, as eza
//! lists it. The plugin ran both as zellij's commands; luneta runs them
//! itself.

use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use lunetad::program;

use crate::zellij::{self, SessionName};

/// How long zoxide has to list the directories, and eza what is in one.
const ANSWER: Duration = Duration::from_secs(2);

/// What eza is asked to list a directory as: an entry a line, the hidden
/// ones too, directories first, each in its colour, after its icon, and with
/// a `/` after a directory.
const LIST: [&str; 7] = [
    "--oneline",
    "--almost-all",
    "--group-directories-first",
    "--classify=always",
    "--color=always",
    "--icons=always",
    "--no-quotes",
];

/// A directory zoxide knows, and the session named for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dir {
    pub path: String,
    pub name: SessionName,
}

/// The directories zoxide knows, best first, each with the session named
/// for it, as `zellij::session_for` names it: for its repository too where it
/// is a worktree of one. One no session can be named for, as `/`, is left
/// out.
pub fn visited() -> Result<Vec<Dir>, String> {
    let mut zoxide = Command::new("zoxide");
    zoxide.args(["query", "--list"]);
    let ran = program::run(&mut zoxide, ANSWER).map_err(|error| unrun("zoxide", &error))?;
    let listed = ran.answer().map_err(|why| format!("zoxide: {why}"))?;
    Ok(dirs_in(&String::from_utf8_lossy(&listed)))
}

/// The directories of what `zoxide query --list` printed, a path a line. A
/// line that is no absolute path is no directory of zoxide's, and would be
/// read from luneta's own.
fn dirs_in(listed: &str) -> Vec<Dir> {
    let dir = |path: &str| {
        if !path.starts_with('/') {
            return None;
        }
        let name = zellij::session_for(Path::new(path))?;
        Some(Dir { path: path.to_owned(), name })
    };
    listed.lines().filter_map(dir).collect()
}

/// What is in the directory `path`, as eza lists it, an entry a line, in
/// colour; or why eza did not list it. eza says why it cannot read a
/// directory on stderr, and may end with success all the same.
pub fn listed(path: &str) -> Result<String, String> {
    let mut eza = Command::new("eza");
    eza.args(LIST).arg("--").arg(path);
    let ran = program::run(&mut eza, ANSWER).map_err(|error| unrun("eza", &error))?;
    let program::Ran::Ended { status, stdout, stderr } = ran else {
        return Err("eza: it did not answer in time".to_owned());
    };
    let listed = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr);
    if !status.success() || (listed.trim().is_empty() && !stderr.trim().is_empty()) {
        return Err(refusal(path, &stderr).unwrap_or_else(|| format!("eza: {status}")));
    }
    Ok(listed)
}

/// Why eza did not list `path`, from what it said on stderr: its first line,
/// less the path, and less the error's number, as `"/tmp/x": No such file or
/// directory (os error 2)` says `No such file or directory`.
fn refusal(path: &str, stderr: &str) -> Option<String> {
    let line = stderr.lines().find(|line| !line.trim().is_empty())?;
    let line = line.replace(path, "");
    let line = before(&line, " - code: ");
    let line = before(line, " (os error ");
    let line = line.trim_matches(|c: char| c.is_whitespace() || "\":-".contains(c));
    (!line.is_empty()).then(|| line.to_owned())
}

/// `text` up to the last `marker` in it, or all of it.
fn before<'a>(text: &'a str, marker: &str) -> &'a str {
    text.rfind(marker).map_or(text, |at| &text[..at])
}

/// Why `program` could not be run.
fn unrun(program: &str, error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return format!("{program} is not on the PATH");
    }
    format!("{program}: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of zoxide's that is no absolute path, or the path of a
    /// directory no session can be named for, as `/`, is no directory of the
    /// list.
    #[test]
    fn a_line_that_names_no_directory_a_session_can_be_made_in_is_left_out() {
        let dir = |path: &str, name: &str| Dir {
            path: path.to_owned(),
            name: SessionName::parse(name).unwrap(),
        };
        let listed = "/home/you/luneta\n/\n\nluneta\n/home/you/notes/\n";
        let dirs = [dir("/home/you/luneta", "luneta"), dir("/home/you/notes/", "notes")];
        assert_eq!(dirs_in(listed), dirs);
    }

    /// What eza says as it fails, less the path and the error's number.
    #[test]
    fn a_refusal_keeps_the_reason_and_not_the_path() {
        let said = |stderr: &str| refusal("/tmp/x", stderr);
        let missing = said("\"/tmp/x\": No such file or directory (os error 2)\n");
        assert_eq!(missing.as_deref(), Some("No such file or directory"));
        let denied = said("Permission denied: /tmp/x - code: 13\n");
        assert_eq!(denied.as_deref(), Some("Permission denied"));
        assert_eq!(said("\n"), None);
    }
}
