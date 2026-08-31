//! The state of a question this plugin asked an outside program.
//!
//! Two screens are filled by a program the plugin does not control: the directory screen by
//! `zoxide`, the agent screen by `claude-ps`. Both can be empty for three different reasons —
//! the answer has not come back, the program is absent, or the program has nothing to say —
//! and a blank list for all three makes a missing program look like a broken feature.
//!
//! This is one type because it was one type twice, once in each of those modules. The screens
//! differ in the program they run and in nothing that belongs here.

/// Why a list built from an outside program is empty.
#[derive(Default)]
pub enum Fetch {
    /// The permission has not come back yet, or the program has not answered.
    #[default]
    Waiting,
    /// The program answered. The list may still be empty, and that is now a fact about the
    /// answer rather than about the asking.
    Ready,
    /// The program could not be run, or did not succeed. Carries what to put on screen: the
    /// most probable failure is that it is not installed, and without the reason that looks
    /// the same as an empty answer.
    Failed(String),
}
