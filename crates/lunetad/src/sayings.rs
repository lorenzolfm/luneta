use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use crate::api;
use crate::api::say::Does;
use crate::mux::{OwnKey, OwnPane, SessionName, Sessions};
use crate::seats::Where;

pub const BYTES_MAX: usize = 16 * 1024;

pub const STALE: Duration = Duration::from_mins(2);

const KEPT: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub enum Speaker {
    Zellij(api::Zellij),
    Own { pane: OwnPane, session: Option<String> },
}

impl Speaker {
    pub fn of(
        zellij: Option<api::Zellij>,
        own: Option<api::OwnSpeaker>,
    ) -> Result<Option<Speaker>, String> {
        match (zellij, own) {
            (None, None) => Ok(None),
            (Some(zellij), None) => Ok(Some(Speaker::Zellij(zellij))),
            (None, Some(own)) => {
                let pane = own.pane.ok_or_else(|| "own speaker: no pane".to_owned())?;
                let pane = OwnPane::of(&pane).map_err(|why| format!("own speaker: {why}"))?;
                Ok(Some(Speaker::Own { pane, session: own.session }))
            },
            (Some(_), Some(_)) => Err("it comes from a pane of zellij's and an own one".to_owned()),
        }
    }

    #[must_use]
    pub fn called(&self) -> Option<&str> {
        match self {
            Speaker::Zellij(zellij) => Some(zellij.session.as_deref().unwrap_or(&zellij.named)),
            Speaker::Own { session, .. } => session.as_deref(),
        }
    }

    #[must_use]
    pub fn seat(&self) -> Option<(Where<'_>, u32)> {
        match self {
            Speaker::Zellij(zellij) => {
                Some((Where::Zellij(zellij.session.as_deref()?), zellij.pane))
            },
            Speaker::Own { pane, .. } => Some((Where::Own(pane.session), pane.id.get())),
        }
    }
}

#[must_use]
pub fn utterance(text: String, speaker: Option<Speaker>) -> api::Utterance {
    let (speaker, own_speaker) = match speaker {
        None => (None, None),
        Some(Speaker::Zellij(zellij)) => (Some(zellij), None),
        Some(Speaker::Own { pane, session }) => {
            (None, Some(api::OwnSpeaker { pane: Some(pane.into()), session }))
        },
    };
    api::Utterance { text, speaker, own_speaker }
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
    speaker: Option<Speaker>,
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
        let api::Utterance { text, speaker, own_speaker } = utterance;
        if text.trim().is_empty() {
            return Err("it says nothing".to_owned());
        }
        let speaker = Speaker::of(speaker, own_speaker)?;
        let named = match &speaker {
            Some(Speaker::Zellij(zellij)) => {
                SessionName::running(&zellij.named).map_err(|why| format!("speaker: {why}"))?;
                Some(zellij.named.as_str())
            },
            Some(Speaker::Own { .. }) | None => None,
        };
        bounded(&text, named)?;
        Ok(Spoken { text, speaker })
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
        own_named: impl Fn(OwnKey) -> Option<String>,
    ) -> Result<(), String> {
        let placed = |spoken| placed(spoken, sessions, &own_named);
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

fn placed(
    spoken: Spoken,
    sessions: &Sessions,
    own_named: &impl Fn(OwnKey) -> Option<String>,
) -> Result<api::Utterance, String> {
    let Spoken { text, speaker } = spoken;
    assert_ne!(text.trim(), "");
    assert!(text.len() <= BYTES_MAX);
    let speaker = speaker.map(|speaker| match speaker {
        Speaker::Zellij(zellij) => {
            let session = sessions.lead(&zellij.named, None).map(ToString::to_string);
            Speaker::Zellij(api::Zellij { session, ..zellij })
        },
        Speaker::Own { pane, .. } => Speaker::Own { pane, session: own_named(pane.session) },
    });
    bounded(&text, speaker.as_ref().and_then(Speaker::called))?;
    Ok(utterance(text, speaker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::{OwnId, OwnMade};

    fn does(text: &str, speaker: Option<api::Zellij>) -> Does {
        Does::InTurn(api::Utterance { text: text.to_owned(), speaker, own_speaker: None })
    }

    fn nameless(_: OwnKey) -> Option<String> {
        None
    }

    fn own_pane(id: u64, pane: u32) -> OwnPane {
        let session = OwnKey { id: OwnId::new(id).unwrap(), made: OwnMade::from_nanos(60) };
        OwnPane::new(session, pane).unwrap()
    }

    fn from_own(pane: api::Own, session: Option<&str>) -> api::OwnSpeaker {
        api::OwnSpeaker { pane: Some(pane), session: session.map(str::to_owned) }
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
            sayings.take(said(&count.to_string()), &Sessions::default(), at, nameless).unwrap();
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
        sayings.take(said("early"), &sessions, at, nameless).unwrap();
        sayings.take(said("late"), &sessions, at + Duration::from_mins(1), nameless).unwrap();
        assert_eq!(texts(&sayings.fresh(at + STALE)), ["early", "late"]);
        assert_eq!(texts(&sayings.fresh(at + STALE + Duration::from_secs(1))), ["late"]);
        assert_eq!(texts(&sayings.fresh(at + Duration::from_hours(2))), [""; 0]);
        assert_eq!(texts(&sayings.fresh(SystemTime::UNIX_EPOCH)), ["early", "late"]);
    }

    #[test]
    fn what_a_speaker_was_asked_with_has_no_say_in_its_session() {
        let speaker = Some(Speaker::Zellij(from("work", Some("made up"))));
        let spoken = Spoken { text: "hello".to_owned(), speaker };
        let placed = placed(spoken, &Sessions::default(), &nameless).unwrap().speaker.unwrap();
        assert_eq!(placed.session, None);
        assert_eq!(placed.named, "work");
    }

    #[test]
    fn an_own_speaker_s_session_is_named_as_it_is_now() {
        let pane = own_pane(3, 12);
        let asked = || {
            let own_speaker = Some(from_own(pane.into(), Some("made up")));
            let utterance = api::Utterance { text: "hello".to_owned(), speaker: None, own_speaker };
            ToSay::parse(Does::InTurn(utterance)).unwrap()
        };
        let mut sayings = Sayings::default();
        let named = |key: OwnKey| (key == pane.session).then(|| "work".to_owned());
        sayings.take(asked(), &Sessions::default(), SystemTime::UNIX_EPOCH, named).unwrap();
        assert_eq!(utterance(&sayings).own_speaker, Some(from_own(pane.into(), Some("work"))));
        sayings.take(asked(), &Sessions::default(), SystemTime::UNIX_EPOCH, nameless).unwrap();
        assert_eq!(utterance(&sayings).own_speaker, Some(from_own(pane.into(), None)));
    }

    #[test]
    fn an_own_speaker_names_a_pane_and_comes_alone() {
        let parsed = |speaker, own_speaker| {
            let utterance = api::Utterance { text: "hi".to_owned(), speaker, own_speaker };
            ToSay::parse(Does::InTurn(utterance))
        };
        let pane: api::Own = own_pane(3, 12).into();
        assert!(parsed(None, Some(from_own(pane, None))).is_ok());
        let zero = api::Own { pane: 0, ..pane };
        let unkeyed = api::Own { session: None, ..pane };
        for unread in [from_own(zero, None), from_own(unkeyed, None)] {
            assert!(parsed(None, Some(unread.clone())).is_err(), "{unread:?}");
        }
        let no_pane = Some(api::OwnSpeaker { pane: None, session: None });
        assert!(parsed(None, no_pane).is_err());
        assert!(parsed(Some(from("work", None)), Some(from_own(pane, None))).is_err());
    }

    #[test]
    fn the_name_an_own_speaker_is_said_by_counts() {
        let pane = own_pane(3, 12);
        let asked = || {
            let own_speaker = Some(from_own(pane.into(), None));
            let text = "a".repeat(BYTES_MAX - 4);
            let utterance = api::Utterance { text, speaker: None, own_speaker };
            ToSay::parse(Does::InTurn(utterance)).unwrap()
        };
        let mut sayings = Sayings::default();
        let at = SystemTime::UNIX_EPOCH;
        let named = |name: &'static str| move |_: OwnKey| Some(name.to_owned());
        assert!(sayings.take(asked(), &Sessions::default(), at, named("work")).is_ok());
        let refused = sayings.take(asked(), &Sessions::default(), at, named("works"));
        assert!(refused.unwrap_err().contains("the most to say at once"));
        assert!(sayings.take(asked(), &Sessions::default(), at, nameless).is_ok());
        assert_eq!(sayings.fresh(at).len(), 2);
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
