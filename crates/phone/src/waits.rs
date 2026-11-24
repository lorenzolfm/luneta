use std::num::NonZeroU32;
use std::time::Duration;

pub const ANSWER: Duration = Duration::from_secs(10);

pub const AGAIN: Duration = Duration::from_secs(2);

pub const AGAIN_MOST: Duration = Duration::from_secs(32);

pub const PACE: Duration = Duration::from_millis(250);

pub const TURN: Duration = Duration::from_mins(10);

pub const MAKING: Duration = Duration::from_mins(4);

pub const LISTING: Duration = Duration::from_mins(1);

pub const PLANNING: Duration = Duration::from_mins(5);

pub const DELETING: Duration = Duration::from_mins(35);

pub const REBASING: Duration = Duration::from_mins(21);

pub const FILING: Duration = Duration::from_secs(510);

const _: () = assert!(AGAIN.as_millis() < ANSWER.as_millis());

const _: () = assert!(AGAIN.as_millis() <= AGAIN_MOST.as_millis() / 2);

const _: () = assert!(PACE.as_millis() < AGAIN.as_millis());

#[must_use]
pub fn again(failed: NonZeroU32, chance: u64) -> Duration {
    let multiple = 1_u32.checked_shl(failed.get()).unwrap_or(u32::MAX);
    let most_ms = AGAIN.saturating_mul(multiple).min(AGAIN_MOST).as_millis() as u64;
    let least_ms = most_ms / 2;
    let waited = Duration::from_millis(least_ms + chance % (most_ms - least_ms + 1));
    assert!(waited >= AGAIN, "{failed} failed: {waited:?} retries sooner than the core did before backing off");
    assert!(waited <= AGAIN_MOST, "{failed} failed: {waited:?} waits past the cap");
    waited
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(times: u32) -> NonZeroU32 {
        NonZeroU32::new(times).unwrap()
    }

    #[test]
    fn a_failed_watch_waits_twice_as_long_as_the_one_before_up_to_a_cap() {
        let bounds = |times| {
            let longest = (0..=AGAIN_MOST.as_millis() as u64).map(|chance| again(failed(times), chance)).max();
            (again(failed(times), 0), longest.unwrap())
        };
        assert_eq!(bounds(1), (AGAIN, AGAIN * 2));
        assert_eq!(bounds(2), (AGAIN * 2, AGAIN * 4));
        assert_eq!(bounds(3), (AGAIN * 4, AGAIN * 8));
        assert_eq!(bounds(4), (AGAIN_MOST / 2, AGAIN_MOST));
        for times in [5, 31, 32, 33, u32::MAX] {
            assert_eq!(bounds(times), (AGAIN_MOST / 2, AGAIN_MOST), "{times} failed");
        }
    }

    #[test]
    fn a_failed_watch_waits_anywhere_in_its_span_so_phones_do_not_retry_in_step() {
        let spread = AGAIN.as_millis() as u64;
        assert_eq!(again(failed(1), spread), AGAIN * 2);
        assert_eq!(again(failed(1), spread + 1), AGAIN, "the chance wraps around the span");
        assert_eq!(again(failed(1), 250), AGAIN + Duration::from_millis(250));
        for chance in [0, 1, 999, 1_000, 1_001, u64::MAX] {
            for times in 1..=8 {
                let waited = again(failed(times), chance);
                assert!(waited >= AGAIN, "{times}, {chance}: {waited:?}");
                assert!(waited <= AGAIN_MOST, "{times}, {chance}: {waited:?}");
            }
        }
    }
}
