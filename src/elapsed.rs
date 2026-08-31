//! Two elapsed times that are not the same kind of thing.
//!
//! Both are a `Duration` underneath, and both end up as one short string in a column. What
//! separates them is what the number is measured from. An [`Age`] counts forward from a moment
//! that has passed: a session was created, a snapshot was taken. A [`Held`] is the length of
//! something that is still running: how long an agent has been waiting.
//!
//! The distinction was a comment before it was a type. `2h ago` names an event and `2h` names a
//! span, and an agent that has waited for thirty-five minutes is not an event from thirty-five
//! minutes ago. While both formatters took a bare `Duration`, the only thing preventing the
//! swap was calling the right one — the comment said which, and said it in the module that
//! holds the second formatter, where a reader of the first would not find it. Now the wrong
//! formatter does not exist to be called.

use std::time::Duration;

/// Time since a moment that has passed.
///
/// Also the staleness of a measurement, which is the same fact about a different kind of
/// moment: see [`Held::grown_by`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Age(Duration);

/// How long something that is still running has run.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Held(Duration);

impl Age {
    /// `cfg(test)` because only the tests need it: a real age comes from the host or the clock.
    #[cfg(test)]
    pub const ZERO: Age = Age(Duration::ZERO);

    pub fn new(elapsed: Duration) -> Self {
        Age(elapsed)
    }

    pub fn from_secs(secs: u64) -> Self {
        Age(Duration::from_secs(secs))
    }

    /// Elapsed time in one magnitude. The column shows the sort order, so `2h ago` is more
    /// useful than `2days 3h 14m 2s`.
    ///
    /// The arms are not [`Held::label`]'s arms with a suffix bolted on: the first one drops the
    /// number entirely. `<1m ago` is the honest reading of a value the host truncated to whole
    /// seconds, and it is why the two bodies are not worth unifying.
    pub fn label(&self) -> String {
        let secs = self.0.as_secs();
        match secs {
            0..=59 => "<1m ago".to_string(),
            60..=3599 => format!("{}m ago", secs / 60),
            3600..=86_399 => format!("{}h ago", secs / 3600),
            86_400..=604_799 => format!("{}d ago", secs / 86_400),
            _ => format!("{}w ago", secs / 604_800),
        }
    }
}

impl Held {
    pub fn from_secs(secs: u64) -> Self {
        Held(Duration::from_secs(secs))
    }

    /// The same span, read from now instead of from the snapshot that measured it.
    ///
    /// A status that had held for four seconds when the snapshot was taken has held for four
    /// seconds plus however long ago that was. The offset is an [`Age`] because that is what it
    /// is — the snapshot is the moment that has passed — and an [`Age`] added to a span that is
    /// still running lengthens the span. It does not turn it into an event, which is why this
    /// returns a [`Held`] and why the addition is spelled out here rather than left to `Add`.
    pub fn grown_by(self, age: Age) -> Held {
        Held(self.0 + age.0)
    }

    /// Elapsed time as a duration: `4s`, `35m`, `2h`.
    ///
    /// Not [`Age::label`]. That form is correct for the time a session started. This column
    /// says how long the status has held, and those are different sentences.
    pub fn label(&self) -> String {
        let secs = self.0.as_secs();
        match secs {
            0..=59 => format!("{}s", secs),
            60..=3599 => format!("{}m", secs / 60),
            3600..=86_399 => format!("{}h", secs / 3600),
            86_400..=604_799 => format!("{}d", secs / 86_400),
            _ => format!("{}w", secs / 604_800),
        }
    }
}
