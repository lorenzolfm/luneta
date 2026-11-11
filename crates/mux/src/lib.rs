//! luneta's own multiplexer (#331): a server that holds the pseudoterminals of
//! its sessions, and reads what each pane's program writes as a terminal
//! does, so that its clients get the screen and not the bytes.
//!
//! For now only the screen a pane's program draws on: Ghostty's emulator,
//! which joins graphemes as Ghostty does, so that an emoji of several code
//! points is one cell here as it is on the terminal luneta runs in.

pub mod screen;
