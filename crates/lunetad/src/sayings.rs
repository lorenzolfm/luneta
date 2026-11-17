use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use crate::api;
use crate::api::say::Does;
use crate::mux::{SessionName, Sessions};
use crate::seats::Where;

pub const BYTES_MAX: usize = 16 * 1024;

pub const STALE: Duration = Duration::from_mins(2);

const KEPT: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct Speaker(pub api::Zellij);

impl Speaker {
    #[must_use]
    pub fn called(&self) -> &str {
        self.0.session.as_deref().unwrap_or(&self.0.named)
    }

    #[must_use]
    pub fn seat(&self) -> Option<(Where<'_>, u32)> {
        Some((Where::Zellij(self.0.session.as_deref()?), self.0.pane))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asker {
    pub named: SessionName,
    pub pane: u32,
}

impl Asker {
    pub fn of(zellij: Option<api::Zellij>) -> Result<Option<Asker>, String> {
        let Some(zellij) = zellij else {
            return Ok(None);
        };
        let named =
            SessionName::running(&zellij.named).map_err(|why| format!("speaker: {why}"))?;
        Ok(Some(Asker { named, pane: zellij.pane }))
    }

    fn placed(self, sessions: &Sessions) -> Speaker {
        let Asker { named, pane } = self;
        let session = sessions.lead(named.as_str(), None).map(ToString::to_string);
        Speaker(api::Zellij { named: named.to_string(), session, pane })
    }
}

#[must_use]
pub fn asked(text: String, asker: Option<Asker>) -> api::Utterance {
    let speaker = asker.map(|Asker { named, pane }| api::Zellij {
        named: named.to_string(),
        session: None,
        pane,
    });
    api::Utterance { text, speaker }
}

fn utterance(text: String, speaker: Option<Speaker>) -> api::Utterance {
    api::Utterance { text, speaker: speaker.map(|Speaker(zellij)| zellij) }
}

#[derive(Debug)]
pub enum ToSay {
    InTurn(Spoken),
    Now(Spoken),
    Stop,
}

#[derive(Debug)]
pub struct Spoken {
    text: String,
    asker: Option<Asker>,
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
        let api::Utterance { text, speaker } = utterance;
        if text.trim().is_empty() {
            return Err("it says nothing".to_owned());
        }
        let asker = Asker::of(speaker)?;
        bounded(&text, asker.as_ref().map(|asker| asker.named.as_str()))?;
        Ok(Spoken { text, asker })
    }
}

fn bounded(text: &str, called: Option<&str>) -> Result<(), String> {
    let bytes = text.len() + called.map_or(0, str::len);
    if bytes > BYTES_MAX {
        let most = format!("{BYTES_MAX} is the most to say at once");
        return Err(format!("it is {bytes} bytes long, and {most}"));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct Sayings {
    kept: VecDeque<(SystemTime, api::Saying)>,
    numbered: u64,
}

impl Sayings {
    pub fn take(
        &mut self,
        said: ToSay,
        sessions: &Sessions,
        at: SystemTime,
    ) -> Result<(), String> {
        let placed = |spoken| placed(spoken, sessions);
        let does = match said {
            ToSay::InTurn(spoken) => Does::InTurn(placed(spoken)?),
            ToSay::Now(spoken) => Does::Now(placed(spoken)?),
            ToSay::Stop => Does::Stop(()),
        };
        self.numbered += 1;
        let say = Some(api::Say { does: Some(does) });
        let saying = api::Saying { pid: std::process::id(), number: self.numbered, say };
        self.kept.push_back((at, saying));
        if self.kept.len() > KEPT {
            self.kept.pop_front();
        }
        Ok(())
    }

    #[must_use]
    pub fn fresh(&self, now: SystemTime) -> Vec<api::Saying> {
        let fresh = |at: &SystemTime| now.duration_since(*at).map_or(true, |age| age <= STALE);
        self.kept.iter().filter(|(at, _)| fresh(at)).map(|(_, saying)| saying.clone()).collect()
    }
}

fn placed(spoken: Spoken, sessions: &Sessions) -> Result<api::Utterance, String> {
    let Spoken { text, asker } = spoken;
    assert_ne!(text.trim(), "");
    assert!(text.len() <= BYTES_MAX);
    let speaker = asker.map(|asker| asker.placed(sessions));
    bounded(&text, speaker.as_ref().map(Speaker::called))?;
    Ok(utterance(text, speaker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn does(text: &str, speaker: Option<api::Zellij>) -> Does {
        Does::InTurn(api::Utterance { text: text.to_owned(), speaker })
    }

    fn utterance(sayings: &Sayings) -> api::Utterance {
        let does = sayings.fresh(SystemTime::UNIX_EPOCH).pop().unwrap().say.unwrap().does;
        let Some(Does::InTurn(utterance)) = does else { panic!("{does:?}") };
        utterance
    }

    fn said(text: &str) -> ToSay {
        ToSay::parse(does(text, None)).unwrap()
    }

    fn from(named: &str, session: Option<&str>) -> api::Zellij {
        api::Zellij { named: named.to_owned(), session: session.map(str::to_owned), pane: 3 }
    }

    fn texts(sayings: &[api::Saying]) -> Vec<&str> {
        let does = sayings.iter().filter_map(|saying| saying.say.as_ref()?.does.as_ref());
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
            sayings.take(said(&count.to_string()), &Sessions::default(), at).unwrap();
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
        let sessions = Sessions::default();
        sayings.take(said("early"), &sessions, at).unwrap();
        sayings.take(said("late"), &sessions, at + Duration::from_mins(1)).unwrap();
        assert_eq!(texts(&sayings.fresh(at + STALE)), ["early", "late"]);
        assert_eq!(texts(&sayings.fresh(at + STALE + Duration::from_secs(1))), ["late"]);
        assert_eq!(texts(&sayings.fresh(at + Duration::from_hours(2))), [""; 0]);
        assert_eq!(texts(&sayings.fresh(SystemTime::UNIX_EPOCH)), ["early", "late"]);
    }

    #[test]
    fn what_a_speaker_was_asked_with_has_no_say_in_its_session() {
        let asked = ToSay::parse(does("hello", Some(from("work", Some("made up"))))).unwrap();
        let mut sayings = Sayings::default();
        sayings.take(asked, &Sessions::default(), SystemTime::UNIX_EPOCH).unwrap();
        let placed = utterance(&sayings).speaker.unwrap();
        assert_eq!(placed.session, None);
        assert_eq!(placed.named, "work");
    }

    #[test]
    fn what_says_nothing_or_too_much_is_not_taken() {
        assert!(ToSay::parse(does(" \n\t", None)).is_err());
        assert!(ToSay::parse(does(&"a".repeat(BYTES_MAX + 1), None)).is_err());
        assert!(ToSay::parse(does(&"a".repeat(BYTES_MAX), None)).is_ok());
        assert!(ToSay::parse(Does::Stop(())).is_ok());
    }

    #[test]
    fn the_name_of_a_session_counts_and_has_to_be_a_name() {
        let parsed = |text: &str, named: &str| ToSay::parse(does(text, Some(from(named, None))));
        let text = "a".repeat(BYTES_MAX - 4);
        assert!(parsed(&text, "name").is_ok());
        assert!(parsed(&text, "names").is_err());
        assert!(parsed("hi", &"a".repeat(BYTES_MAX)).is_err());
        for no_name in ["", " ", "..", "a/b"] {
            assert!(parsed("hi", no_name).is_err(), "{no_name:?}");
        }
    }
}
