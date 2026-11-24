use std::time::Duration;

pub const ANSWER: Duration = Duration::from_secs(10);

pub const AGAIN: Duration = Duration::from_secs(2);

pub const PACE: Duration = Duration::from_millis(250);

pub const TURN: Duration = Duration::from_mins(10);

pub const MAKING: Duration = Duration::from_mins(4);

pub const LISTING: Duration = Duration::from_mins(1);

pub const PLANNING: Duration = Duration::from_mins(5);

pub const DELETING: Duration = Duration::from_mins(35);

pub const REBASING: Duration = Duration::from_mins(21);

pub const FILING: Duration = Duration::from_secs(510);

const _: () = assert!(AGAIN.as_millis() < ANSWER.as_millis());

const _: () = assert!(PACE.as_millis() < AGAIN.as_millis());
