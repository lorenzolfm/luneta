use std::time::{Instant, SystemTime};

#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub wall: SystemTime,
    pub at: Instant,
}

impl Now {
    pub fn read() -> Now {
        Now { wall: SystemTime::now(), at: Instant::now() }
    }
}
