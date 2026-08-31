//! The state of a question this plugin asked an outside program.
//!
//! Two screens are filled by a program the plugin does not control: the directory screen by
//! `zoxide`, the agent screen by `claude-ps`. Both can be empty for three different reasons —
//! the answer has not come back, the program is absent, or the program has nothing to say —
//! and a blank list for all three makes a missing program look like a broken feature.
//!
//! This is one type because it was one type twice, once in each of those modules. The screens
//! differ in the program they run, and in what an answer holds, which is the parameter.

/// Why a list built from an outside program is empty, and what the answer held when there was
/// one.
///
/// The answer hangs off [`Fetch::Ready`], which is the only state that has one. It is a
/// parameter because it is the one thing the two screens do not share: `zoxide` answers with
/// directories, `claude-ps` with agents and a count of the agents it cannot address.
///
/// Keeping the answer here rather than beside this value is the point of the type. A screen
/// that held both would have to empty its list by hand on every path to [`Fetch::Failed`], and
/// a failure with the answer of the last reply still behind it is not a state either screen
/// can draw.
pub enum Fetch<T> {
    /// The permission has not come back yet, or the program has not answered.
    Waiting,
    /// The program answered, and this is what it said. The answer may still be empty, and that
    /// is a fact about the answer rather than about the asking.
    Ready(T),
    /// The program could not be run, or did not succeed. Carries what to put on screen: the
    /// most probable failure is that it is not installed, and without the reason that looks
    /// the same as an empty answer.
    Failed(String),
}

/// Written out rather than derived, because [`Fetch::Waiting`] is the state before any answer
/// and needs nothing from the answer to name it. A derive would ask every `T` for a default
/// that this value never reads.
impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Fetch::Waiting
    }
}
