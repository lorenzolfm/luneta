use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Made(u64);

impl Made {
    #[must_use]
    pub fn of(at: SystemTime) -> Option<Made> {
        u64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok().map(Made)
    }

    #[must_use]
    pub fn from_nanos(nanos: u64) -> Made {
        Made(nanos)
    }

    pub fn read(at: prost_types::Timestamp) -> Result<Made, String> {
        let prost_types::Timestamp { seconds, nanos } = at;
        let part = u64::try_from(nanos).ok().filter(|&part| part < NANOS_PER_SECOND);
        let part = part.ok_or_else(|| format!("{nanos} nanoseconds are no part of a second"))?;
        let whole = u64::try_from(seconds).map_err(|_| "a time before 1970".to_owned())?;
        let made = whole.checked_mul(NANOS_PER_SECOND).and_then(|it| it.checked_add(part));
        let made = made.map(Made).ok_or_else(|| "a time past 2554".to_owned())?;
        assert_eq!(prost_types::Timestamp::from(made), at);
        Ok(made)
    }

    #[must_use]
    pub fn nanos(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn at(self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.0)
    }
}

impl From<Made> for prost_types::Timestamp {
    fn from(made: Made) -> Self {
        let seconds = i64::try_from(made.0 / NANOS_PER_SECOND);
        let seconds = seconds.expect("u64::MAX nanoseconds are fewer than i64::MAX seconds");
        let nanos = i32::try_from(made.0 % NANOS_PER_SECOND);
        let nanos = nanos.expect("the nanoseconds left of a second fit an i32");
        prost_types::Timestamp { seconds, nanos }
    }
}

#[cfg(test)]
mod tests {
    use prost_types::Timestamp;

    use super::*;

    #[test]
    fn a_made_time_reads_back_as_the_timestamp_it_was_said_in_to_the_nanosecond_or_not_at_all() {
        let said = |seconds, nanos| Timestamp { seconds, nanos };
        let last = i64::try_from(u64::MAX / NANOS_PER_SECOND).unwrap();
        for made in [said(1_795_525_750, 483_697_731), said(0, 0), said(0, 999_999_999), said(last, 709_551_615)] {
            assert_eq!(Timestamp::from(Made::read(made).unwrap()), made);
        }
        assert_eq!(Made::read(said(last, 709_551_615)), Ok(Made(u64::MAX)));
        let unmade = [said(-1, 0), said(1, -1), said(1, 1_000_000_000), said(last, 709_551_616), said(last + 1, 0)];
        for unmade in unmade {
            assert!(Made::read(unmade).is_err(), "{unmade:?}");
        }
    }

    #[test]
    fn a_time_is_made_at_the_nanosecond_it_names_since_1970() {
        let at = UNIX_EPOCH + Duration::new(1_795_525_750, 483_697_731);
        let made = Made::of(at).unwrap();
        assert_eq!((made.nanos(), made.at()), (1_795_525_750_483_697_731, at));
        assert_eq!(Made::of(UNIX_EPOCH - Duration::from_nanos(1)), None);
    }
}
