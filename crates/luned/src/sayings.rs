use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use crate::api;
use crate::api::say::Does;

pub const BYTES_MAX: usize = 16 * 1024;

pub const STALE: Duration = Duration::from_mins(2);

const KEPT: usize = 16;

#[derive(Debug)]
pub enum ToSay {
    InTurn(Spoken),
    Now(Spoken),
    Stop,
}

#[derive(Debug)]
pub struct Spoken {
    text: String,
}

impl ToSay {
    pub fn parse(does: Does) -> Result<ToSay, String> {
        match does {
            Does::InTurn(utterance) => Ok(ToSay::InTurn(Spoken::parse(utterance)?)),
            Does::Now(utterance) => Ok(ToSay::Now(Spoken::parse(utterance)?)),
            Does::Stop(()) => Ok(ToSay::Stop),
        }
    }
}

impl Spoken {
    fn parse(utterance: api::Utterance) -> Result<Spoken, String> {
        let api::Utterance { text } = utterance;
        if text.trim().is_empty() {
            return Err("it says nothing".to_owned());
        }
        let bytes = text.len();
        if bytes > BYTES_MAX {
            let most = format!("{BYTES_MAX} is the most to say at once");
            return Err(format!("it is {bytes} bytes long, and {most}"));
        }
        Ok(Spoken { text })
    }
}

#[derive(Debug, Default)]
pub struct Sayings {
    kept: VecDeque<(SystemTime, api::Saying)>,
    numbered: u64,
}

impl Sayings {
    pub fn take(&mut self, said: ToSay, at: SystemTime) {
        let does = match said {
            ToSay::InTurn(spoken) => Does::InTurn(spoken.said()),
            ToSay::Now(spoken) => Does::Now(spoken.said()),
            ToSay::Stop => Does::Stop(()),
        };
        self.numbered += 1;
        let say = Some(api::Say { does: Some(does) });
        let saying = api::Saying {
            pid: std::process::id(),
            number: self.numbered,
            say,
        };
        self.kept.push_back((at, saying));
        if self.kept.len() > KEPT {
            self.kept.pop_front();
        }
        assert!(self.kept.len() <= KEPT);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    pub fn prune(&mut self, now: SystemTime) {
        self.kept.retain(|(at, _)| is_fresh(*at, now));
        assert!(self.kept.len() <= KEPT);
    }

    #[must_use]
    pub fn fresh(&self, now: SystemTime) -> Vec<api::Saying> {
        self.kept
            .iter()
            .filter(|(at, _)| is_fresh(*at, now))
            .map(|(_, saying)| saying.clone())
            .collect()
    }
}

fn is_fresh(at: SystemTime, now: SystemTime) -> bool {
    now.duration_since(at).map_or(true, |age| age <= STALE)
}

impl Spoken {
    fn said(self) -> api::Utterance {
        let Spoken { text } = self;
        assert_ne!(text.trim(), "");
        assert!(text.len() <= BYTES_MAX);
        api::Utterance { text }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn does(text: &str) -> Does {
        Does::InTurn(api::Utterance {
            text: text.to_owned(),
        })
    }

    fn said(text: &str) -> ToSay {
        ToSay::parse(does(text)).unwrap()
    }

    fn texts(sayings: &[api::Saying]) -> Vec<&str> {
        let does = sayings
            .iter()
            .filter_map(|saying| saying.say.as_ref()?.does.as_ref());
        does.map(|does| match does {
            Does::InTurn(utterance) | Does::Now(utterance) => utterance.text.as_str(),
            Does::Stop(()) => "stop",
        })
        .collect()
    }

    #[test]
    fn the_list_keeps_the_last_taken_in_order_each_numbered_later() {
        let at = SystemTime::UNIX_EPOCH;
        let mut sayings = Sayings::default();
        for count in 0..KEPT + 2 {
            sayings.take(said(&count.to_string()), at);
        }
        let fresh = sayings.fresh(at);
        assert_eq!(fresh.len(), KEPT);
        assert_eq!(texts(&fresh)[0], "2");
        assert_eq!(texts(&fresh)[KEPT - 1], (KEPT + 1).to_string());
        assert!(fresh.windows(2).all(|two| two[0].number < two[1].number));
    }

    #[test]
    fn what_was_said_too_long_ago_goes() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_hours(1);
        let mut sayings = Sayings::default();
        sayings.take(said("early"), at);
        sayings.take(said("late"), at + Duration::from_mins(1));
        assert_eq!(texts(&sayings.fresh(at + STALE)), ["early", "late"]);
        assert_eq!(
            texts(&sayings.fresh(at + STALE + Duration::from_secs(1))),
            ["late"]
        );
        assert_eq!(texts(&sayings.fresh(at + Duration::from_hours(2))), [""; 0]);
        assert_eq!(
            texts(&sayings.fresh(SystemTime::UNIX_EPOCH)),
            ["early", "late"]
        );

        sayings.prune(SystemTime::UNIX_EPOCH);
        assert!(!sayings.is_empty());
        assert_eq!(texts(&sayings.fresh(at + STALE)), ["early", "late"]);

        sayings.prune(at + STALE + Duration::from_secs(1));
        assert!(!sayings.is_empty());
        assert_eq!(
            texts(&sayings.fresh(at + STALE + Duration::from_secs(1))),
            ["late"]
        );

        sayings.prune(at + Duration::from_hours(2));
        assert!(sayings.is_empty());
        assert_eq!(texts(&sayings.fresh(at + Duration::from_hours(2))), [""; 0]);
    }

    #[test]
    fn what_says_nothing_or_too_much_is_not_taken() {
        assert!(ToSay::parse(does(" \n\t")).is_err());
        assert!(ToSay::parse(does(&"a".repeat(BYTES_MAX + 1))).is_err());
        assert!(ToSay::parse(does(&"a".repeat(BYTES_MAX))).is_ok());
        assert!(ToSay::parse(Does::Stop(())).is_ok());
    }
}
