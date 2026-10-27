//! What is new in each pane since you were last in it, an agent that finished
//! or a command its shell finished, and the panes you marked unread. The
//! sidebar dots their rows until you go there.
//!
//! You were in a pane once luneta's zellij stayed on it a moment, `DWELL`, so
//! that a pane you only pass through on the way to another keeps its dot. Each
//! luneta sees you only on the pane its own zellij is on, and only while its
//! window has the focus: one you are not looking at sees nothing for you. It
//! keeps what you saw in a `Ledger`, the same for every luneta on the host: up
//! to when you saw each pane. What finished there by then, you saw.
//!
//! What finished is dated as the daemon says: by Claude Code, as an agent
//! comes to `idle`, and by the shell's hook, as a command ends. Both date it
//! by the host's clock, which luneta dates what you saw by too.
//!
//! A pane marked unread keeps its dot whatever is new in it, until you go
//! there: a visit that started after the mark was made. So a pane you mark as
//! you stay in it keeps its dot until you leave it and come back.
//!
//! What was kept of a session goes once the daemon has not listed it for a
//! while, `GONE`: a session renamed as the daemon reads zellij's folder is
//! left out of one list, and comes back in the next.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use notify::RecommendedWatcher;

use crate::host::Event;
use crate::ledger::{Key, Ledger, Session};
use crate::state;

/// How long luneta's zellij stays on a pane before you were there.
const DWELL: Duration = Duration::from_secs(1);

/// How long the daemon goes without listing a session before what was kept
/// of it goes. A rename made while the daemon reads zellij's folder leaves the
/// session out of one list, and so does a socket it fails to read once, and a
/// luneta can hear of a session after another kept something of it. The
/// daemon lists it again within its sweep, of two seconds.
const GONE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct News {
    /// Up to when you saw each pane.
    seen: Ledger,
    /// When you marked each pane unread.
    unread: Ledger,
    /// The pane luneta's zellij is on, as the last `dwell` found it, and since
    /// when.
    dwelling: Option<(Key, SystemTime)>,
    /// Since when the daemon has not listed each session something is kept
    /// of, by the lists `listed` heard.
    missing: BTreeMap<Session, SystemTime>,
}

impl News {
    /// The news every luneta on the host shares, as it stands.
    pub fn shared() -> Self {
        let kept = |name| match state::folder(name) {
            Some(dir) => Ledger::within(dir),
            None => Ledger::unshared(
                "neither XDG_STATE_HOME nor HOME is set, to keep them in".to_owned(),
            ),
        };
        News { seen: kept("seen"), unread: kept("unread"), ..News::default() }
    }

    /// Why the news is this luneta's alone, where a folder failed it.
    pub fn lost(&self) -> Option<&str> {
        self.seen.lost().or(self.unread.lost())
    }

    /// Takes up what the other lunetas saw and marked since, and says whether
    /// that changed anything.
    pub fn reload(&mut self) -> bool {
        let seen = self.seen.reload();
        self.unread.reload() || seen
    }

    /// Whether what finished last in the pane of `key`, at `finished`, if
    /// anything did, is new since you were last there.
    pub fn is_new(&self, key: Key, finished: Option<SystemTime>) -> bool {
        finished.is_some_and(|finished| self.seen.get(key).is_none_or(|seen| seen < finished))
    }

    pub fn is_unread(&self, key: Key) -> bool {
        self.unread.get(key).is_some()
    }

    /// Marks the pane of `key` unread at `now`, or takes the mark away from
    /// one that has it.
    pub fn toggle_unread(&mut self, key: Key, now: SystemTime) {
        let marked = (!self.is_unread(key)).then_some(now);
        self.unread.set(key, marked);
    }

    /// luneta's zellij is on the pane of `here` at `now`, while you look at
    /// it, or else on none, and what finished last in each pane did at the
    /// moment `finished` says. You were in the pane it is on once it has
    /// stayed there for `DWELL`, and so in the one it was on before, if it
    /// stayed there that long, until now: what finished in either, you saw,
    /// and a mark made before you came, you saw too. Says whether that
    /// changed anything.
    pub fn dwell(
        &mut self,
        here: Option<Key>,
        finished: impl Fn(Key) -> Option<SystemTime>,
        now: SystemTime,
    ) -> bool {
        let (left, dwelling) = match (self.dwelling.take(), here) {
            (Some((was, since)), Some(here)) if was == here => (None, Some((was, since))),
            (was, here) => (was, here.map(|here| (here, now))),
        };
        let mut changed = false;
        for (key, since) in left.into_iter().chain(dwelling) {
            // A clock set back since counts no time at all.
            if now.duration_since(since).unwrap_or(Duration::ZERO) >= DWELL {
                changed |= self.visit(key, since, finished(key), now);
            }
        }
        self.dwelling = dwelling;
        changed
    }

    /// You were in the pane of `key` from `since` until `now`, and what
    /// finished there last did at `finished`. Says whether that changed
    /// anything.
    fn visit(
        &mut self,
        key: Key,
        since: SystemTime,
        finished: Option<SystemTime>,
        now: SystemTime,
    ) -> bool {
        let mut changed = false;
        if self.is_new(key, finished) {
            // The hook dates a command's end to the last moment of its
            // second, which can be a moment from now.
            self.seen.set(key, finished.map(|finished| finished.max(now)));
            changed = true;
        }
        if self.unread.get(key).is_some_and(|marked| marked < since) {
            self.unread.set(key, None);
            changed = true;
        }
        changed
    }

    /// The daemon lists the sessions of `live` at `now`, or none where it
    /// lists one it cannot tell from another, which any could be. What was
    /// kept of a session it has not listed for `GONE` goes, for every luneta.
    pub fn listed(&mut self, live: Option<&BTreeSet<Session>>, now: SystemTime) {
        let Some(live) = live else {
            self.missing.clear();
            return;
        };
        let unlisted: BTreeSet<Session> = self
            .seen
            .sessions()
            .chain(self.unread.sessions())
            .filter(|session| !live.contains(session))
            .collect();
        self.missing.retain(|session, _| unlisted.contains(session));
        for session in unlisted {
            self.missing.entry(session).or_insert(now);
        }
        // A clock set back since counts no time at all.
        let gone: BTreeSet<Session> = self
            .missing
            .iter()
            .filter(|(_, since)| now.duration_since(**since).unwrap_or(Duration::ZERO) >= GONE)
            .map(|(session, _)| *session)
            .collect();
        if gone.is_empty() {
            return;
        }
        self.seen.forget(&gone);
        self.unread.forget(&gone);
        self.missing.retain(|session, _| !gone.contains(session));
    }

    /// The folders the news is shared through, while it is.
    fn dirs(&self) -> Vec<&Path> {
        [self.seen.dir(), self.unread.dir()].into_iter().flatten().collect()
    }
}

/// Tells luneta's loop each time another luneta, or this one, keeps something
/// new of what you saw or marked, for as long as the watcher it returns is
/// kept.
pub fn watch(news: &News, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch(&news.dirs(), events, || Event::News)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn ms(millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
    }

    fn session(started: u64) -> Session {
        Session { started: at(started), socket: std::num::NonZeroU64::MIN }
    }

    fn pane(pane: u32) -> Key {
        Key { session: session(10), pane }
    }

    /// The agent in pane 1 finished at 100, and the command in pane 2 ended
    /// at 101.2, which its hook dates to the last moment of its second.
    fn finished(key: Key) -> Option<SystemTime> {
        match key.pane {
            1 => Some(at(100)),
            2 => Some(ms(101_999)),
            _ => None,
        }
    }

    fn new(news: &News) -> [bool; 2] {
        [1, 2].map(|id| news.is_new(pane(id), finished(pane(id))))
    }

    /// What finished is new until luneta's zellij stays on its pane for a
    /// second, though the hook dates its end after you leave. A pane it
    /// passes through keeps its dot, and one that finishes again is new
    /// again.
    #[test]
    fn what_finished_is_new_until_you_stay_in_its_pane() {
        let mut news = News::default();
        // Through pane 1 on the way to pane 2, which it leaves at 101.6.
        news.dwell(Some(pane(1)), finished, ms(100_000));
        news.dwell(Some(pane(2)), finished, ms(100_500));
        assert!(!news.dwell(Some(pane(2)), finished, ms(101_000)));
        assert!(news.dwell(Some(pane(2)), finished, ms(101_500)));
        news.dwell(None, finished, ms(101_600));
        assert_eq!(new(&news), [true, false]);

        let later = |key: Key| finished(key).map(|at| at + Duration::from_secs(10));
        assert!([1, 2].iter().all(|id| news.is_new(pane(*id), later(pane(*id)))));
    }

    /// A pane marked unread keeps its mark until luneta's zellij comes to it
    /// and stays: one it is on as the mark is made keeps it until it leaves
    /// and comes back. A pane with nothing new in it can be marked too, and
    /// the mark taken away.
    #[test]
    fn a_pane_marked_unread_stays_so_until_you_come_back_to_it() {
        let mut news = News::default();
        news.dwell(Some(pane(1)), finished, at(200));
        news.toggle_unread(pane(1), at(201));
        news.toggle_unread(pane(3), at(201));
        news.dwell(Some(pane(1)), finished, at(205));
        assert!(news.is_unread(pane(1)));

        news.dwell(Some(pane(3)), finished, at(206));
        news.dwell(Some(pane(1)), finished, ms(206_500));
        news.dwell(None, finished, at(207));
        assert!(news.is_unread(pane(1)) && news.is_unread(pane(3)));
        news.dwell(Some(pane(1)), finished, at(208));
        news.dwell(Some(pane(1)), finished, at(209));
        assert!(!news.is_unread(pane(1)) && news.is_unread(pane(3)));

        news.toggle_unread(pane(3), at(210));
        assert!(!news.is_unread(pane(3)));
    }

    /// A session the daemon has not listed for `GONE` takes what you saw and
    /// marked in it with it. One it leaves out of a list for less, as a
    /// rename can, keeps it, and so does each while the daemon lists one it
    /// cannot tell from another.
    #[test]
    fn a_session_gone_takes_its_news_with_it() {
        let mut news = News::default();
        news.dwell(Some(pane(1)), finished, at(200));
        news.dwell(Some(pane(1)), finished, at(201));
        news.toggle_unread(pane(2), at(201));
        let (live, other) = (BTreeSet::from([session(10)]), BTreeSet::from([session(20)]));
        news.listed(Some(&other), at(210));
        news.listed(Some(&live), at(215));
        news.listed(Some(&other), at(220));
        news.listed(None, at(225));
        news.listed(Some(&other), at(231));
        assert_eq!(new(&news), [false, true]);
        assert!(news.is_unread(pane(2)));

        news.listed(Some(&other), at(240));
        news.listed(Some(&other), at(241));
        assert_eq!(new(&news), [true, true]);
        assert!(!news.is_unread(pane(2)));
    }
}
