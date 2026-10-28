use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Age(Duration);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Held(Duration);

/// A moment, as the host's clock tells it: how long after the epoch.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Moment(Duration);

impl Moment {
    /// A clock that cannot be read puts every moment at the epoch, where
    /// nothing comes after anything.
    pub fn now() -> Self {
        Moment(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default())
    }

    pub const fn from_millis(millis: u64) -> Self {
        Moment(Duration::from_millis(millis))
    }

    #[cfg(test)]
    pub fn millis(self) -> u64 {
        u64::try_from(self.0.as_millis()).unwrap_or(u64::MAX)
    }

    /// The moment `held` before this one.
    #[cfg(test)]
    pub fn back(self, held: Held) -> Moment {
        Moment(self.0.saturating_sub(held.0))
    }

    /// How long after `earlier` this moment is: no time at all when it is
    /// not after it.
    pub fn since(self, earlier: Moment) -> Held {
        Held(self.0.saturating_sub(earlier.0))
    }
}

impl Age {
    #[cfg(test)]
    pub const ZERO: Age = Age(Duration::ZERO);

    pub fn new(elapsed: Duration) -> Self {
        Age(elapsed)
    }

    #[cfg(test)]
    pub fn from_secs(secs: u64) -> Self {
        Age(Duration::from_secs(secs))
    }

    pub fn label(&self) -> String {
        let secs = self.0.as_secs();
        match secs {
            0..=59 => "<1m ago".to_owned(),
            60..=3599 => format!("{}m ago", secs / 60),
            3600..=86_399 => format!("{}h ago", secs / 3600),
            86_400..=604_799 => format!("{}d ago", secs / 86_400),
            _ => format!("{}w ago", secs / 604_800),
        }
    }
}

impl Held {
    pub const fn from_secs(secs: u64) -> Self {
        Held(Duration::from_secs(secs))
    }

    #[cfg(test)]
    pub const fn from_millis(millis: u64) -> Self {
        Held(Duration::from_millis(millis))
    }

    pub fn label(&self) -> String {
        let secs = self.0.as_secs();
        match secs {
            0..=59 => format!("{secs}s"),
            60..=3599 => format!("{}m", secs / 60),
            3600..=86_399 => format!("{}h", secs / 3600),
            86_400..=604_799 => format!("{}d", secs / 86_400),
            _ => format!("{}w", secs / 604_800),
        }
    }
}
