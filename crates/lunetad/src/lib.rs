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
pub mod log;
mod metadata;
pub mod moves;
mod panes;
mod process;
pub mod program;
pub mod release;
mod remote;
mod shells;
pub mod state;
mod watch;
mod zellij;

pub use claude::folder as claude_folder;
pub use client::{connect, listening, stream, through, Unbridged, BRIDGED, REACH};
pub use daemon::{serve, Claim};
pub use folder::Refusal;
pub use git::{local as local_git, REPOSITORY_VARS as GIT_REPOSITORY_VARS};
pub use metadata::folder as zellij_info_folder;
pub use remote::{named_by as hosts_named, sharing, Destination, FILE as HOSTS_FILE, HOSTS_MAX};
pub use zellij::{
    folder as zellij_folder, running as zellij_sessions, SessionName,
    NAME_BYTES_MAX as SESSION_NAME_BYTES_MAX, SPOKEN as ZELLIJ_CONTRACT,
};
