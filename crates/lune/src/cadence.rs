use std::time::{Duration, Instant, SystemTime};

use crate::clock::Moment;
use crate::sidebar;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cadence {
    spinning: bool,
    clock_next: Option<SystemTime>,
    steady_next: Option<Instant>,
}

impl Cadence {
    pub fn spin(&mut self) {
        self.spinning = true;
    }

    #[must_use]
    pub fn spinning(&self) -> bool {
        self.spinning
    }

    pub fn age(&mut self, since: SystemTime, wall: SystemTime) {
        if let Some(at) = sidebar::next_age(since, wall) {
            self.clock_next = Some(self.clock_next.map_or(at, |next| next.min(at)));
        }
    }

    pub fn clock_at(&mut self, at: SystemTime, wall: SystemTime) {
        assert!(
            at > wall,
            "a scheduled wall tick lands strictly after wall time"
        );
        self.clock_next = Some(self.clock_next.map_or(at, |next| next.min(at)));
    }

    pub fn every_second(&mut self, since: Instant, steady: Instant) {
        let seconds = steady.saturating_duration_since(since).as_secs();
        if let Some(at) = since.checked_add(Duration::from_secs(seconds.saturating_add(1))) {
            assert!(
                at > steady,
                "the next steady second lands strictly after steady time"
            );
            self.steady_next = Some(self.steady_next.map_or(at, |next| next.min(at)));
        }
    }

    pub fn merge(&mut self, other: Cadence) {
        self.spinning |= other.spinning;
        if let Some(at) = other.clock_next {
            self.clock_next = Some(self.clock_next.map_or(at, |next| next.min(at)));
        }
        if let Some(at) = other.steady_next {
            self.steady_next = Some(self.steady_next.map_or(at, |next| next.min(at)));
        }
    }

    #[must_use]
    pub fn wait(&self, drawn: Moment, now: Moment) -> Option<Duration> {
        let mut wait: Option<Duration> = None;
        let mut take = |left: Duration| wait = Some(wait.map_or(left, |best| best.min(left)));
        if self.spinning {
            take(
                if now.system < drawn.system
                    || sidebar::frame(now.system) != sidebar::frame(drawn.system)
                {
                    Duration::ZERO
                } else {
                    sidebar::next_spin(now.system)
                },
            );
        }
        if let Some(at) = self.clock_next {
            take(if now.system < drawn.system {
                Duration::ZERO
            } else {
                at.duration_since(now.system).unwrap_or(Duration::ZERO)
            });
        }
        if let Some(at) = self.steady_next {
            take(at.saturating_duration_since(now.steady));
        }
        wait
    }

    #[must_use]
    pub fn due(&self, drawn: Moment, now: Moment) -> bool {
        let wall_active = self.spinning || self.clock_next.is_some();
        (wall_active && now.system < drawn.system)
            || (self.spinning && sidebar::frame(now.system) != sidebar::frame(drawn.system))
            || self.clock_next.is_some_and(|at| now.system >= at)
            || self.steady_next.is_some_and(|at| now.steady >= at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_second_schedules_strictly_after_steady_clock() {
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let drawn = Moment {
            steady: start + Duration::from_millis(1250),
            system,
        };
        let mut cadence = Cadence::default();
        assert!(!cadence.spinning());
        cadence.every_second(start, drawn.steady);
        assert!(!cadence.spinning());

        assert_eq!(cadence.wait(drawn, drawn), Some(Duration::from_millis(750)));
        assert!(!cadence.due(drawn, drawn));

        let before = Moment {
            steady: start + Duration::from_millis(1999),
            system,
        };
        assert!(!cadence.due(drawn, before));

        let at = Moment {
            steady: start + Duration::from_secs(2),
            system,
        };
        assert!(cadence.due(drawn, at));
        assert_eq!(cadence.wait(drawn, at), Some(Duration::ZERO));
    }

    #[test]
    fn age_and_spin_wake_immediately_when_wall_clock_steps_backward() {
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let drawn = Moment {
            steady: start,
            system,
        };
        let mut cadence = Cadence::default();
        cadence.age(system - Duration::from_secs(5), system);
        assert!(!cadence.spinning());

        assert_eq!(cadence.wait(drawn, drawn), Some(Duration::from_secs(1)));
        assert!(!cadence.due(drawn, drawn));

        let back = Moment {
            steady: start + Duration::from_millis(16),
            system: system - Duration::from_secs(2),
        };
        assert_eq!(cadence.wait(drawn, back), Some(Duration::ZERO));
        assert!(cadence.due(drawn, back));
    }

    #[test]
    fn spinner_wait_returns_zero_once_frame_has_advanced_since_draw() {
        let start = Instant::now();
        let drawn = Moment {
            steady: start,
            system: SystemTime::UNIX_EPOCH + Duration::from_millis(160_099),
        };
        let mut cadence = Cadence::default();
        assert!(!cadence.spinning());
        cadence.spin();
        assert!(cadence.spinning());

        assert_eq!(cadence.wait(drawn, drawn), Some(Duration::from_millis(1)));
        assert!(!cadence.due(drawn, drawn));

        let next_frame = Moment {
            steady: start + Duration::from_millis(2),
            system: SystemTime::UNIX_EPOCH + Duration::from_millis(160_101),
        };
        assert!(cadence.due(drawn, next_frame));
        assert_eq!(cadence.wait(drawn, next_frame), Some(Duration::ZERO));

        let mid = Moment {
            steady: start,
            system: SystemTime::UNIX_EPOCH + Duration::from_millis(160_050),
        };
        let same_frame = Moment {
            steady: start + Duration::from_millis(30),
            system: SystemTime::UNIX_EPOCH + Duration::from_millis(160_080),
        };
        assert!(!cadence.due(mid, same_frame));
        assert_eq!(
            cadence.wait(mid, same_frame),
            Some(Duration::from_millis(20))
        );
    }

    #[test]
    fn spinning_reflects_spin_and_merge_without_leaking_from_clocks() {
        let mut idle = Cadence::default();
        assert!(!idle.spinning());

        let mut merged = Cadence::default();
        merged.merge(idle);
        assert!(!merged.spinning());

        let mut turning = Cadence::default();
        turning.spin();
        assert!(turning.spinning());

        idle.merge(turning);
        assert!(idle.spinning());

        turning.merge(Cadence::default());
        assert!(turning.spinning());
    }
}
