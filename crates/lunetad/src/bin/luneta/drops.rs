//! How many of the clients zellij drops luneta takes back to their sessions.
//! zellij drops a client that falls behind, and its session runs on, so that
//! one more is worth starting, however many came before: load comes in bursts,
//! and a client costs little next to luneta's ending. Only clients dropped as
//! soon as they come are going nowhere, and those are what luneta counts. One
//! that keeps falling behind for as long as `SOON` is taken back for good.

use std::time::Duration;

/// A client that lasts less than this before zellij drops it counts as going
/// nowhere. Well under the 55 seconds it took zellij to give up on one that read
/// nothing, measured with a pane writing all it could.
pub const SOON: Duration = Duration::from_secs(30);

/// How many clients in a row zellij drops `SOON` for luneta to give up.
pub const TOO_MANY: u32 = 6;

/// What luneta does as zellij dropped its client.
#[derive(Debug, PartialEq, Eq)]
pub enum Dropped {
    /// It takes it back to its session.
    TakeBack,
    /// It ends: `TOO_MANY` were dropped `SOON`, in a row.
    GiveUp,
}

/// How many clients in a row zellij dropped `SOON`.
#[derive(Debug, Default)]
pub struct Drops {
    soon: u32,
}

impl Drops {
    /// What luneta does as zellij dropped its client, which `lasted` that long:
    /// one that lasted starts the count again.
    pub fn dropped(&mut self, lasted: Duration) -> Dropped {
        self.soon = if lasted < SOON { self.soon + 1 } else { 0 };
        if self.soon < TOO_MANY { Dropped::TakeBack } else { Dropped::GiveUp }
    }

    pub fn gave_way(&mut self, lasted: Duration) {
        if lasted >= SOON {
            self.soon = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// luneta gives up on clients dropped as soon as they come, but never on
    /// those that lasted a while in between, however many: load comes in bursts,
    /// and each of those was worth taking back.
    #[test]
    fn only_clients_dropped_soon_in_a_row_are_given_up_on() {
        let quick = SOON.checked_sub(Duration::from_millis(1)).unwrap();
        let mut drops = Drops::default();
        for _ in 1..TOO_MANY {
            assert_eq!(drops.dropped(quick), Dropped::TakeBack);
        }
        assert_eq!(drops.dropped(SOON), Dropped::TakeBack);
        for _ in 1..TOO_MANY {
            assert_eq!(drops.dropped(quick), Dropped::TakeBack);
        }
        assert_eq!(drops.dropped(quick), Dropped::GiveUp);
        let mut slow = Drops::default();
        for _ in 0..100 {
            assert_eq!(slow.dropped(SOON), Dropped::TakeBack);
        }
    }

    #[test]
    fn only_a_client_that_lasted_starts_the_count_again_as_it_gives_way() {
        let quick = SOON.checked_sub(Duration::from_millis(1)).unwrap();
        let almost_too_many = || {
            let mut drops = Drops::default();
            for _ in 1..TOO_MANY {
                assert_eq!(drops.dropped(quick), Dropped::TakeBack);
            }
            drops
        };
        let mut brief = almost_too_many();
        brief.gave_way(quick);
        assert_eq!(brief.dropped(quick), Dropped::GiveUp);
        let mut lasted = almost_too_many();
        lasted.gave_way(SOON);
        for _ in 1..TOO_MANY {
            assert_eq!(lasted.dropped(quick), Dropped::TakeBack);
        }
        assert_eq!(lasted.dropped(quick), Dropped::GiveUp);
    }
}
