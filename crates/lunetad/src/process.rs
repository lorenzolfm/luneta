//! What the daemon reads of another process: when it started, spelled as Claude
//! Code records it and as a time, and the environment it started with. Each
//! system has its own way to say them, and a backend of its own.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("lunetad reads processes on Linux and macOS, and on nothing else yet");

pub use os::{environ, start, started, Domain};
