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

/// The longest text the daemon takes: seventeen minutes of speech or so, and
/// what each answer of `Watch` carries for as long as it is on the list.
pub const BYTES_MAX: usize = 16 * 1024;

/// How long what was said stays worth reading. A window holds what waits to
/// be read to it too.
pub const STALE: Duration = Duration::from_mins(2);

/// How many the list holds, the last taken.
const KEPT: usize = 16;

/// What the daemon last took, oldest first, each with when.
#[derive(Debug, Default)]
pub struct Sayings {
    kept: VecDeque<(SystemTime, api::Saying)>,
    /// The number of the last taken.
    numbered: u64,
}

impl Sayings {
    /// Takes `does` at `at`, as of the pane it names, by the name of its
    /// session now, where `agents` tell it: a rename leaves zellij's
    /// variables behind.
    pub fn take(&mut self, does: Does, agents: &[api::Agent], at: SystemTime) {
        let does = match does {
            Does::InTurn(utterance) => Does::InTurn(by_name_now(utterance, agents)),
            Does::Now(utterance) => Does::Now(by_name_now(utterance, agents)),
            Does::Stop(stop) => Does::Stop(stop),
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

/// `utterance`, with its speaker's session by the name it has now, where an
/// agent in that pane tells it, or else by the name it had as the pane opened.
fn by_name_now(mut utterance: api::Utterance, agents: &[api::Agent]) -> api::Utterance {
    if let Some(speaker) = &mut utterance.speaker {
        let zellij = agents.iter().filter_map(|agent| agent.zellij.as_ref());
        let found = zellij
            .filter(|zellij| zellij.named == speaker.session && zellij.pane == speaker.pane)
            .find_map(|zellij| zellij.session.clone());
        if let Some(now) = found {
            speaker.session = now;
        }
    }
    utterance
}

/// Whether the daemon takes `does`, or why not: what it reads has words in
/// it, and no more than `BYTES_MAX` of them.
pub fn check(does: &Does) -> Result<(), String> {
    let (Does::InTurn(utterance) | Does::Now(utterance)) = does else {
        return Ok(());
    };
    let bytes = utterance.text.len();
    if utterance.text.trim().is_empty() {
        return Err("it says nothing".to_owned());
    }
    if bytes > BYTES_MAX {
        return Err(format!("it is {bytes} bytes long, and {BYTES_MAX} is the most to say at once"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::utterance::Speaker;

    fn said(text: &str) -> Does {
        Does::InTurn(api::Utterance { text: text.to_owned(), speaker: None })
    }

    fn texts(sayings: &[api::Saying]) -> Vec<&str> {
        let does = sayings.iter().filter_map(|saying| saying.say.as_ref()?.does.as_ref());
        does.map(|does| match does {
            Does::InTurn(utterance) | Does::Now(utterance) => utterance.text.as_str(),
            Does::Stop(_) => "stop",
        })
        .collect()
    }

    #[test]
    fn the_list_keeps_the_last_taken_in_order_each_numbered_later() {
        let at = SystemTime::UNIX_EPOCH;
        let mut sayings = Sayings::default();
        for count in 0..KEPT + 2 {
            sayings.take(said(&count.to_string()), &[], at);
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
        sayings.take(said("early"), &[], at);
        sayings.take(said("late"), &[], at + Duration::from_mins(1));
        assert_eq!(texts(&sayings.fresh(at + STALE)), ["early", "late"]);
        assert_eq!(texts(&sayings.fresh(at + STALE + Duration::from_secs(1))), ["late"]);
        assert_eq!(texts(&sayings.fresh(at + Duration::from_hours(2))), [""; 0]);
        // A clock put back loses nothing.
        assert_eq!(texts(&sayings.fresh(SystemTime::UNIX_EPOCH)), ["early", "late"]);
    }

    /// zellij's variables keep the name a session had as the pane opened.
    #[test]
    fn a_speaker_goes_by_the_name_its_session_has_now() {
        let agent = |named: &str, session: Option<&str>, pane| api::Agent {
            zellij: Some(api::Zellij {
                named: named.to_owned(),
                session: session.map(str::to_owned),
                pane,
            }),
            ..api::Agent::default()
        };
        let agents = [agent("old", Some("new"), 3), agent("other", Some("other"), 4)];
        let from = |session: &str, pane| api::Utterance {
            text: "hello".to_owned(),
            speaker: Some(Speaker { session: session.to_owned(), pane }),
        };
        let told = |utterance| by_name_now(utterance, &agents).speaker.unwrap().session;
        assert_eq!(told(from("old", 3)), "new");
        assert_eq!(told(from("old", 9)), "old", "no agent in that pane says otherwise");
        assert_eq!(told(from("elsewhere", 3)), "elsewhere");
    }

    #[test]
    fn what_says_nothing_or_too_much_is_not_taken() {
        assert!(check(&said(" \n\t")).is_err());
        assert!(check(&said(&"a".repeat(BYTES_MAX + 1))).is_err());
        assert_eq!(check(&said(&"a".repeat(BYTES_MAX))), Ok(()));
        assert_eq!(check(&Does::Stop(api::say::Stop {})), Ok(()));
    }
}
