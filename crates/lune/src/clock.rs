use std::time::{Instant, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moment {
    pub system: SystemTime,
    pub steady: Instant,
}

impl Moment {
    #[must_use]
    pub fn now() -> Moment {
        Moment { system: SystemTime::now(), steady: Instant::now() }
    }
}

#[cfg(test)]
mod arithmetic {
    use std::ops::{Add, Sub};
    use std::time::Duration;

    use super::Moment;

    impl Add<Duration> for Moment {
        type Output = Moment;

        fn add(self, by: Duration) -> Moment {
            Moment { system: self.system + by, steady: self.steady + by }
        }
    }

    impl Sub<Duration> for Moment {
        type Output = Moment;

        fn sub(self, by: Duration) -> Moment {
            Moment {
                system: self.system.checked_sub(by).expect("system time does not underflow"),
                steady: self.steady.checked_sub(by).expect("steady time does not underflow"),
            }
        }
    }
}
