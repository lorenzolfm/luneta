//! lunetad, the daemon that holds what luneta knows of a host, and what its
//! CLI needs to reach it.
//!
//! One daemon runs for each user on a host. It listens on a Unix socket in
//! luneta's runtime folder, where the shell hook keeps its files too, which it
//! reads, and answers in the protocol that `proto/luneta/v1` defines.

mod agent;
pub mod api;
pub mod asks;
pub mod carries;
pub mod checks;
mod claude;
pub mod cli;
mod client;
pub mod closes;
mod core;
mod daemon;
mod directory;
pub mod ended;
pub mod eza;
mod fetches;
pub mod folder;
mod git;
mod github;
pub mod images;
pub mod issues;
mod keeps;
pub mod links;
pub mod log;
pub mod moves;
pub mod names;
pub mod opens;
mod panes;
mod pi;
pub mod printed;
mod process;
pub mod program;
pub mod prunes;
pub mod release;
mod remote;
pub mod renames;
pub mod sayings;
pub mod screens;
pub mod seats;
mod shells;
pub mod skills;
pub mod state;
mod topics;
pub mod transcripts;
pub mod turns;
mod watch;
mod zellij;
pub mod zoxide;

pub use agent::Harness;
pub use claude::{folder as claude_folder, one_answer};
pub use client::{connect, listening, stream, through, Unbridged, BRIDGED, REACH};
pub use daemon::{serve, Claim};
pub use directory::Directory;
pub use folder::Refusal;
pub use git::{local as local_git, REPOSITORY_VARS as GIT_REPOSITORY_VARS};
pub use panes::newborn;
pub use remote::{named_by as hosts_named, sharing, Destination, FILE as HOSTS_FILE, HOSTS_MAX};
pub use zellij::{
    folder as zellij_folder, running as zellij_sessions, sockets as zellij_sockets, Id as SocketId,
    SessionName, NAME_BYTES_MAX as SESSION_NAME_BYTES_MAX,
};
