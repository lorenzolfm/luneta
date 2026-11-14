use std::time::Duration;

pub const SOON: Duration = Duration::from_secs(30);

pub const TOO_MANY: u32 = 6;

#[derive(Debug, PartialEq, Eq)]
pub enum Dropped {
    TakeBack,
    GiveUp,
}

#[derive(Debug, Default)]
pub struct Drops {
    soon: u32,
}

impl Drops {
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
