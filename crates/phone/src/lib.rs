uniffi::setup_scaffolding!();

mod api;
mod core;
mod model;
mod unanswered;
mod verbs;
pub mod waits;

pub use crate::core::{tailnet_name, Asking, Conversation, Core, Observer, TACKLE};
pub use crate::model::{
    Agent, Far, Held, Holds, Model, Placed, Project, Reached, Seen, Status, TerminalKey, Unopened,
    View, Why, Worktree,
};
pub use crate::unanswered::{Refusals, Unanswered};
pub use crate::verbs::{
    Answer, Asks, Came, Filed, Goes, Issue, IssueRepository, Issues, Kept, Landed, Listed, Marked,
    Order, Outcome, Plan, Pruned, Pull, Pushed, Rebase, Rebased, Screen, Step, Unasked, Waiting,
};
