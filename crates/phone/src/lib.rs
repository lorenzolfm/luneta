uniffi::setup_scaffolding!();

mod api;
mod core;
mod model;
mod unanswered;
mod verbs;
pub mod waits;

pub use crate::core::{Asking, Core, Observer, TACKLE};
pub use crate::model::{Agent, Held, Machine, Model, Project, Reached, Seen, Status, TerminalKey, View, Why, Worktree};
pub use crate::unanswered::Unanswered;
pub use crate::verbs::{
    Answer, Asks, Came, Filed, Goes, Issue, IssueRepository, Issues, Kept, Landed, Listed, Marked,
    Order, Outcome, Plan, Pruned, Pull, Pushed, Rebase, Rebased, Step, Unasked, Waiting,
};
