use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Age(Duration);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Held(Duration);

impl Age {
    #[cfg(test)]
    pub const ZERO: Age = Age(Duration::ZERO);

    pub fn new(elapsed: Duration) -> Self {
        Age(elapsed)
    }

    pub fn from_secs(secs: u64) -> Self {
        Age(Duration::from_secs(secs))
    }

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

    pub fn grown_by(self, age: Age) -> Held {
        Held(self.0 + age.0)
    }

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
