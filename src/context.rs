//! The context luneta runs each command with, which the command answers
//! with: the kind of the command, by which its answer finds its way back.

use crate::{agents, brief, checkouts, close, daemon, dirs, menu, panes, places, skills};

/// What names the kind of a command luneta runs, in the context the command
/// answers with: its value says where the answer goes.
pub const CONTEXT_KEY: &str = "luneta";

/// Each kind of command luneta runs, by the value its context names it with.
/// A kind that is not here may share its value with one that is.
const CONTEXT_VALUES: [&str; 19] = [
    agents::CONTEXT_VALUE,
    agents::CLOSE_VALUE,
    brief::CONTEXT_VALUE,
    checkouts::CONTEXT_VALUE,
    close::CLIENTS_VALUE,
    daemon::CONTEXT_VALUE,
    dirs::CONTEXT_VALUE,
    dirs::PREVIEW_VALUE,
    dirs::WORKTREES_VALUE,
    menu::BRANCH_VALUE,
    menu::START_VALUE,
    menu::OPENING_VALUE,
    menu::CLOSING_VALUE,
    menu::NAMING_VALUE,
    menu::ARRIVING_VALUE,
    menu::TAB_VALUE,
    panes::CONTEXT_VALUE,
    places::CONTEXT_VALUE,
    skills::CONTEXT_VALUE,
];

/// Whether zellij could not start the command that answered at all, as when
/// it is not on the server's `PATH`. zellij says so with exit code 2 and the
/// error from the OS as Rust spells it, `No such file or directory (os error
/// 2)`: git never spells an error so, and luneta, which does, fails with 1.
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
