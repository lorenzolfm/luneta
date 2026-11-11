//! luneta's own multiplexer (#331): a server that holds the pseudoterminals of
//! its sessions, and reads what each pane's program writes as a terminal
//! does, so that its clients get the screen and not the bytes.
//!
//! One server holds every own session of a user, and answers over gRPC on a
//! socket in luneta's runtime folder. Each pane has a thread of its own, which
//! owns its pseudoterminal and its screen: Ghostty's emulator, which joins
//! graphemes as Ghostty does, so that an emoji of several code points is one
//! cell here as it is on the terminal luneta runs in.

pub mod api;
pub mod screen;
pub mod folder;
pub mod serve;

mod asked;
mod frame;
mod mux;
mod pane;
mod pty;
