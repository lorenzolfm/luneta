//! lunetad, the daemon that holds what luneta knows of a host, and what its
//! CLI needs to reach it.
//!
//! One daemon runs for each user on a host. It listens on a Unix socket in
//! luneta's runtime folder, where the shell hook keeps its files too, which it
//! reads, and answers in the protocol that `proto/luneta/v1` defines.

mod agent;
pub mod api;
mod claude;
mod client;
pub mod clients;
mod core;
mod daemon;
pub mod folder;
mod git;
mod github;
mod metadata;
mod panes;
mod process;
pub mod program;
pub mod release;
mod shells;
mod watch;
mod zellij;

pub use claude::folder as claude_folder;
pub use client::{connect, listening};
pub use daemon::{serve, Claim};
pub use folder::Refusal;
pub use git::local as local_git;
pub use metadata::folder as zellij_info_folder;
pub use zellij::{folder as zellij_folder, running as zellij_sessions, SPOKEN as ZELLIJ_CONTRACT};
