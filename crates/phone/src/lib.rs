uniffi::setup_scaffolding!();

mod api;
mod core;
mod model;
mod terminal;
mod unanswered;
mod verbs;
pub mod waits;

pub use crate::core::{tailnet_name, Asking, Backoff, Conversation, Core, Observer, TACKLE};
pub use crate::waits::NonZeroDuration;
pub use crate::model::{
    Agent, Far, Found, Held, Holds, Model, Placed, Project, Reached, Seen, Status, TerminalKey,
    Unopened, View, Why, Worktree,
};
pub use crate::terminal::{
    Appearance, Cursor, CursorShape, Framed, Grid, KeyName, Rgb, Row, Run, TerminalView, SAID_HELD,
    SIDE_MAX,
};
pub use crate::unanswered::{Refusals, Unanswered};
pub use crate::verbs::{
    Answer, Asks, Came, Filed, Goes, Issue, IssueRepository, Issues, Kept, Landed, Listed, Marked,
    Order, Outcome, Plan, Pruned, Pull, Pushed, Rebase, Rebased, Screen, Step, Unasked, Waiting,
};
