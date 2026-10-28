//! The context luneta runs each command with, which the command answers
//! with: the kind of the command, by which its answer finds its way back.

use crate::{agents, close, daemon, dirs, menu, panes, skills};

/// What names the kind of a command luneta runs, in the context the command
/// answers with: its value says where the answer goes.
pub const CONTEXT_KEY: &str = "luneta";

/// What names the directory a command was run for, in its context: what
/// answers for a directory that is not the one asked about now goes nowhere.
pub const DIR_KEY: &str = "luneta_dir";

/// Each kind of command luneta runs, by the value its context names it with.
/// A kind that is not here may share its value with one that is.
const CONTEXT_VALUES: [&str; 10] = [
    agents::CLOSE_VALUE,
    close::CLIENTS_VALUE,
    daemon::CONTEXT_VALUE,
    dirs::CONTEXT_VALUE,
    dirs::PREVIEW_VALUE,
    dirs::WORKTREES_VALUE,
    menu::BRANCH_VALUE,
    menu::NAMING_VALUE,
    panes::CONTEXT_VALUE,
    skills::CONTEXT_VALUE,
];

/// Whether zellij could not start the command that answered at all, as when
/// it is not on the server's `PATH`. zellij says so with exit code 2 and the
/// error from the OS as Rust spells it, `No such file or directory (os error
/// 2)`: luneta, which spells an error so too, fails with 1.
pub fn unstarted(exit_code: Option<i32>, stderr: &[u8]) -> bool {
    exit_code == Some(2) && String::from_utf8_lossy(stderr).contains("(os error ")
}

/// No two kinds of command share a value: the answer to one would go where
/// the other's goes.
const _: () = {
    let mut at = 0;
    while at < CONTEXT_VALUES.len() {
        let mut other = at + 1;
        while other < CONTEXT_VALUES.len() {
            let shared = same(CONTEXT_VALUES[at], CONTEXT_VALUES[other]);
            assert!(!shared, "two kinds of command share a context value");
            other += 1;
        }
        at += 1;
    }
};

/// Whether `one` and `two` are the same text, where `==` cannot say: in a
/// constant.
const fn same(one: &str, two: &str) -> bool {
    let (one, two) = (one.as_bytes(), two.as_bytes());
    if one.len() != two.len() {
        return false;
    }
    let mut at = 0;
    while at < one.len() {
        if one[at] != two[at] {
            return false;
        }
        at += 1;
    }
    true
}
