//! What `luneta say` gives the daemon, for luneta's window to read aloud: a
//! short list on the host, numbered, which `Watch` and `luneta bridge` take
//! to a window on any machine, as they take the agents, with nothing new
//! between them. So `luneta say` returns once the daemon has it, and nothing
//! waits on the speakers.
//!
//! The list is short, and what was said too long ago goes from it, by the
//! clock of the machine that said it, so that a window that was away for an
//! hour, as a Mac that slept, hears what is still worth hearing, and no
//! clock of another machine has a say in it. A window that keeps up has read
//! each by then.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use crate::api;
use crate::api::say::Does;
use crate::zellij::{SessionName, Sessions};

/// The longest text the daemon takes, with the name of the session it comes
/// from, as that is said too: seventeen minutes of speech or so, and what each
/// answer of `Watch` carries for as long as it is on the list.
pub const BYTES_MAX: usize = 16 * 1024;

/// How long what was said stays worth reading. A window holds what waits to
/// be read to it too.
pub const STALE: Duration = Duration::from_mins(2);

/// How many the list holds, the last taken.
const KEPT: usize = 16;

/// What the daemon takes, as `parse` found it: only that makes one, and only
/// this is put on the list.
#[derive(Debug)]
pub struct ToSay(Does);

impl ToSay {
    /// `does`, or why the daemon does not take it: what it reads has words in
    /// it, comes from a session by a name zellij takes, and is no more than
    /// `BYTES_MAX`, with that name.
    pub fn parse(does: Does) -> Result<ToSay, String> {
        let (Does::InTurn(utterance) | Does::Now(utterance)) = &does else {
            return Ok(ToSay(does));
        };
        if utterance.text.trim().is_empty() {
            return Err("it says nothing".to_owned());
        }
        let mut bytes = utterance.text.len();
        if let Some(speaker) = &utterance.speaker {
            SessionName::running(&speaker.named).map_err(|why| format!("speaker: {why}"))?;
            bytes += speaker.named.len();
        }
        if bytes > BYTES_MAX {
            let most = format!("{BYTES_MAX} is the most to say at once");
            return Err(format!("it is {bytes} bytes long, and {most}"));
        }
        Ok(ToSay(does))
    }
}

/// What the daemon last took, oldest first, each with when.
#[derive(Debug, Default)]
pub struct Sayings {
    kept: VecDeque<(SystemTime, api::Saying)>,
    /// The number of the last taken.
    numbered: u64,
}

impl Sayings {
    /// Takes `said` at `at`, as of the pane it names, in the session `sessions`
    /// say that is now: a rename leaves zellij's variables behind.
    pub fn take(&mut self, said: ToSay, sessions: &Sessions, at: SystemTime) {
        let does = match said.0 {
            Does::InTurn(utterance) => Does::InTurn(placed(utterance, sessions)),
            Does::Now(utterance) => Does::Now(placed(utterance, sessions)),
            Does::Stop(()) => Does::Stop(()),
        };
        self.numbered += 1;
        let say = Some(api::Say { does: Some(does) });
        let saying = api::Saying { pid: std::process::id(), number: self.numbered, say };
        self.kept.push_back((at, saying));
        if self.kept.len() > KEPT {
            self.kept.pop_front();
        }
    }

    /// What is still worth reading at `now`, oldest first. A clock put back
    /// past when one was taken finds it fresh.
    #[must_use]
    pub fn fresh(&self, now: SystemTime) -> Vec<api::Saying> {
        let fresh = |at: &SystemTime| now.duration_since(*at).map_or(true, |age| age <= STALE);
        self.kept.iter().filter(|(at, _)| fresh(at)).map(|(_, saying)| saying.clone()).collect()
    }
}

/// `utterance`, with its speaker's session as `sessions` tell it now: none
/// where no session could be it, or more than one, as a pane's id does not tell
/// the sessions that have held a name apart. What it was asked with has no say.
fn placed(mut utterance: api::Utterance, sessions: &Sessions) -> api::Utterance {
    if let Some(speaker) = &mut utterance.speaker {
        speaker.session = sessions.lead(&speaker.named, None).map(str::to_owned);
    }
    utterance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn does(text: &str, speaker: Option<api::Zellij>) -> Does {
        Does::InTurn(api::Utterance { text: text.to_owned(), speaker })
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
            sayings.take(said(&count.to_string()), &Sessions::default(), at);
        }
        let fresh = sayings.fresh(at);
        assert_eq!(fresh.len(), KEPT);
        assert_eq!(texts(&fresh)[0], "2");
        assert_eq!(texts(&fresh)[KEPT - 1], (KEPT + 1).to_string());
        assert!(fresh.windows(2).all(|two| two[0].number < two[1].number));
    }

    /// A Mac that slept for an hour hears nothing of that hour.
    #[test]
    fn what_was_said_too_long_ago_goes() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_hours(1);
        let mut sayings = Sayings::default();
        let sessions = Sessions::default();
        sayings.take(said("early"), &sessions, at);
        sayings.take(said("late"), &sessions, at + Duration::from_mins(1));
        assert_eq!(texts(&sayings.fresh(at + STALE)), ["early", "late"]);
        assert_eq!(texts(&sayings.fresh(at + STALE + Duration::from_secs(1))), ["late"]);
        assert_eq!(texts(&sayings.fresh(at + Duration::from_hours(2))), [""; 0]);
        // A clock put back loses nothing.
        assert_eq!(texts(&sayings.fresh(SystemTime::UNIX_EPOCH)), ["early", "late"]);
    }

    /// The daemon alone says what session a pane is in now.
    #[test]
    fn what_a_speaker_was_asked_with_has_no_say_in_its_session() {
        let speaker = Some(from("work", Some("made up")));
        let utterance = api::Utterance { text: "hello".to_owned(), speaker };
        let placed = placed(utterance, &Sessions::default()).speaker.unwrap();
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

    /// A session's name is said too, and goes in each answer of `Watch`.
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
