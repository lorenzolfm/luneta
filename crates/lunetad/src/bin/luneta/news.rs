//! What is new in each pane since you were last in it, an agent that finished
//! or a command its shell finished, and the panes you marked unread. The
//! sidebar dots their rows until you go there.
//!
//! You were in a pane once luneta's zellij stayed on it a moment, `DWELL`, so
//! that a pane you only pass through on the way to another keeps its dot. Each
//! luneta sees you only on the pane its own zellij is on, and only while its
//! window has the focus: one you are not looking at sees nothing for you. It
//! keeps what you saw in a `Ledger`, the same for every luneta of its daemon:
//! up to when you saw each pane. What finished there by then, you saw. It keeps
//! in another when you were last in each, whatever finished there, which says
//! the session you looked at last to a luneta that starts with none named: you
//! can mark a pane read from anywhere, and it says what you saw, not where you
//! were.
//!
//! What finished is dated as the daemon says: by Claude Code, as an agent
//! comes to `idle`, and by the shell's hook, as a command ends. Both date it
//! by the host's clock, which luneta dates what you saw by too.
//!
//! A pane marked unread keeps its dot whatever is new in it, until you go
//! there: a visit that started after the mark was made. So a pane you mark as
//! you stay in it keeps its dot until you leave it and come back. A pane
//! marked read is one you were just in: whatever gave it its dot, it has none.
//!
//! What was kept of a session goes once the daemon has not listed it for a
//! while, `GONE`: a session renamed as the daemon reads zellij's folder is
//! left out of one list, and comes back in the next. Only a list the daemon
//! gives as the host stands counts, and one it read all of zellij's folder
//! for, as the sidebar says.
//!
//! It is kept in luneta's runtime folder, beside the daemon's socket: its
//! files are named for sockets that daemon lists, and go with the folder, as
//! those sockets do.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use lunetad::folder;
use notify::RecommendedWatcher;

use crate::host::Event;
use crate::ledger::{Key, Ledger, Session};
use crate::live::{Lists, Missing};
use crate::state;

/// How long luneta's zellij stays on a pane before you were there.
const DWELL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct News {
    /// Up to when you saw each pane.
    seen: Ledger,
    /// When you were last in each pane, dated once a visit.
    been: Ledger,
    /// When you marked each pane unread.
    unread: Ledger,
    /// The pane luneta's zellij is on, as the last `dwell` found it, and since
    /// when: by the host's clock, which a mark is dated by, and by luneta's
    /// own, which no one sets, for how long.
    dwelling: Option<(Key, SystemTime, Instant)>,
    /// Since when the daemon has not listed each session something is kept
    /// of, by the lists `listed` heard.
    missing: Missing,
}

impl News {
    /// The news every luneta of the daemon in `runtime`, luneta's runtime
    /// folder, shares, as it stands. The folder is made as the daemon makes
    /// it, for this user alone: a daemon refuses one open to others.
    pub fn within(runtime: &Path) -> Self {
        match folder::make(runtime) {
            Ok(folder) => News {
                seen: Ledger::within(folder.path().join("seen")),
                been: Ledger::within(folder.path().join("been")),
                unread: Ledger::within(folder.path().join("unread")),
                ..News::default()
            },
            Err(refusal) => News::unshared(&refusal.to_string()),
        }
    }

    /// News of this luneta's alone, as it has no folder to share it through,
    /// for `why`.
    pub fn unshared(why: &str) -> Self {
        let unshared = || Ledger::unshared(why.to_owned());
        News { seen: unshared(), been: unshared(), unread: unshared(), ..News::default() }
    }

    /// Why the news is this luneta's alone, where a folder failed it.
    pub fn lost(&self) -> Option<&str> {
        self.seen.lost().or(self.been.lost()).or(self.unread.lost())
    }

    /// Takes up what the other lunetas saw and marked since, and says whether
    /// that changed anything that shows: where you were, no row does.
    pub fn reload(&mut self) -> bool {
        self.been.reload();
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

    /// When you were last in a pane of `session`, if you were, in any luneta
    /// of its daemon.
    pub fn last_seen(&self, session: Session) -> Option<SystemTime> {
        self.been.latest(session)
    }

    /// Marks the pane of `key` unread at `now`.
    pub fn mark_unread(&mut self, key: Key, now: SystemTime) {
        self.unread.set(key, Some(now));
    }

    /// Marks the pane of `key` read at `now`, as a visit would for its dot:
    /// what finished there last, at `finished`, you saw, and its unread mark
    /// goes. It says nothing of where you were.
    pub fn read(&mut self, key: Key, finished: Option<SystemTime>, now: SystemTime) {
        self.saw(key, finished, now);
        if self.is_unread(key) {
            self.unread.set(key, None);
        }
    }

    /// luneta's zellij is on the pane of `here` at `now`, and at `at` by
    /// luneta's own clock, while you look at it, or else on none, and what
    /// finished last in each pane did at the moment `finished` says. You were
    /// in the pane it is on once it has stayed there for `DWELL`, and so in
    /// the one it was on before, if it stayed there that long, until now:
    /// what finished in either, you saw, and a mark made before you came, you
    /// saw too. Says whether that changed anything.
    pub fn dwell(
        &mut self,
        here: Option<Key>,
        finished: impl Fn(Key) -> Option<SystemTime>,
        now: SystemTime,
        at: Instant,
    ) -> bool {
        let (left, dwelling) = match (self.dwelling.take(), here) {
            (Some(was), Some(here)) if was.0 == here => (None, Some(was)),
            (was, here) => (was, here.map(|here| (here, now, at))),
        };
        let mut changed = false;
        for (key, since, from) in left.into_iter().chain(dwelling) {
            if at.duration_since(from) >= DWELL {
                changed |= self.visit(key, since, finished(key), now);
            }
        }
        self.dwelling = dwelling;
        changed
    }

    /// You were in the pane of `key` from `since` until `now`, and what
    /// finished there last did at `finished`. Says whether that changed
    /// anything that shows.
    fn visit(
        &mut self,
        key: Key,
        since: SystemTime,
        finished: Option<SystemTime>,
        now: SystemTime,
    ) -> bool {
        if self.been.get(key).is_none_or(|been| been < since) {
            self.been.set(key, Some(now));
        }
        let mut changed = self.saw(key, finished, now);
        if self.unread.get(key).is_some_and(|marked| marked < since) {
            self.unread.set(key, None);
            changed = true;
        }
        changed
    }

    /// You saw by `now` what finished last in the pane of `key`, at
    /// `finished`. Says whether that changed anything.
    fn saw(&mut self, key: Key, finished: Option<SystemTime>, now: SystemTime) -> bool {
        if !self.is_new(key, finished) {
            return false;
        }
        // The hook dates a command's end to the last moment of its second,
        // which can be a moment from now.
        self.seen.set(key, finished.map(|finished| finished.max(now)));
        true
    }

    /// The daemon lists the sessions of `live` at `at`, by luneta's own
    /// clock, or none, as it does not answer. What was kept of a session
    /// that has ended since, as `Missing` tells, goes, for every luneta.
    pub fn listed(&mut self, live: Option<&Lists>, at: Instant) {
        let ledgers = [&self.seen, &self.been, &self.unread];
        let kept: BTreeSet<Session> = ledgers.into_iter().flat_map(Ledger::sessions).collect();
        let gone = self.missing.gone(&kept, live, at);
        if gone.is_empty() {
            return;
        }
        self.seen.forget(&gone);
        self.been.forget(&gone);
        self.unread.forget(&gone);
    }

    /// The folders the news is shared through, while it is.
    fn dirs(&self) -> Vec<&Path> {
        [self.seen.dir(), self.been.dir(), self.unread.dir()].into_iter().flatten().collect()
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
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::LazyLock;

    use super::*;
    use crate::machines::Machine;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn ms(millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
    }

    /// luneta's own clock as the host's says `now`: as long after one moment
    /// of it as `now` is after the epoch.
    fn steady(now: SystemTime) -> Instant {
        static START: LazyLock<Instant> = LazyLock::new(Instant::now);
        *START + now.duration_since(SystemTime::UNIX_EPOCH).unwrap()
    }

    /// luneta's zellij on `here` at `now`, by both clocks.
    fn dwell(news: &mut News, here: Option<Key>, now: SystemTime) -> bool {
        news.dwell(here, finished, now, steady(now))
    }

    fn session(started: u64) -> Session {
        Session::new(at(started), std::num::NonZeroU64::MIN).unwrap()
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
        dwell(&mut news, Some(pane(1)), ms(100_000));
        dwell(&mut news, Some(pane(2)), ms(100_500));
        assert!(!dwell(&mut news, Some(pane(2)), ms(101_000)));
        assert!(dwell(&mut news, Some(pane(2)), ms(101_500)));
        dwell(&mut news, None, ms(101_600));
        assert_eq!(new(&news), [true, false]);

        let later = |key: Key| finished(key).map(|at| at + Duration::from_secs(10));
        assert!([1, 2].iter().all(|id| news.is_new(pane(*id), later(pane(*id)))));
    }

    /// The session you looked at last is the one you stayed on last, with
    /// nothing new in it too: one you only passed through does not count, a
    /// long stay is dated once, and coming back dates it again. What finishes
    /// there after you last looked is still new.
    #[test]
    fn a_stay_dates_the_session_with_nothing_new_in_it() {
        let other = |pane: u32| Key { session: session(20), pane };
        let nothing = |_: Key| None;
        let mut news = News::default();

        // Through the session at 10, and on to the one at 20, to stay.
        news.dwell(Some(pane(3)), nothing, at(200), steady(at(200)));
        news.dwell(Some(other(3)), nothing, ms(200_500), steady(ms(200_500)));
        news.dwell(Some(other(3)), nothing, ms(201_500), steady(ms(201_500)));
        assert_eq!(news.last_seen(session(10)), None);
        assert_eq!(news.last_seen(session(20)), Some(ms(201_500)));

        // Dated once as it stays.
        news.dwell(Some(other(3)), nothing, at(900), steady(at(900)));
        assert_eq!(news.last_seen(session(20)), Some(ms(201_500)));

        // Back to the one at 10, which is the last you looked at now, and
        // what finishes in it from then on is new.
        news.dwell(Some(pane(3)), nothing, at(1_000), steady(at(1_000)));
        news.dwell(Some(pane(3)), nothing, at(1_001), steady(at(1_001)));
        assert_eq!(news.last_seen(session(10)), Some(at(1_001)));
        assert!(news.is_new(pane(3), Some(at(1_002))));
    }

    /// A clock set forward is no time spent: a pane luneta's zellij passes
    /// through as the host's clock jumps an hour keeps its dot.
    #[test]
    fn a_pane_passed_through_as_the_clock_jumps_keeps_its_dot() {
        let mut news = News::default();
        news.dwell(Some(pane(1)), finished, at(200), steady(at(200)));
        news.dwell(None, finished, at(3_800), steady(ms(200_500)));
        assert_eq!(new(&news), [true, true]);
    }

    /// A pane marked unread keeps its mark until luneta's zellij comes to it
    /// and stays: one it is on as the mark is made keeps it until it leaves
    /// and comes back. A pane with nothing new in it can be marked too, and
    /// marked read again.
    #[test]
    fn a_pane_marked_unread_stays_so_until_you_come_back_to_it() {
        let mut news = News::default();
        dwell(&mut news, Some(pane(1)), at(200));
        news.mark_unread(pane(1), at(201));
        news.mark_unread(pane(3), at(201));
        dwell(&mut news, Some(pane(1)), at(205));
        assert!(news.is_unread(pane(1)));

        dwell(&mut news, Some(pane(3)), at(206));
        dwell(&mut news, Some(pane(1)), ms(206_500));
        dwell(&mut news, None, at(207));
        assert!(news.is_unread(pane(1)) && news.is_unread(pane(3)));
        dwell(&mut news, Some(pane(1)), at(208));
        dwell(&mut news, Some(pane(1)), at(209));
        assert!(!news.is_unread(pane(1)) && news.is_unread(pane(3)));

        news.read(pane(3), None, at(210));
        assert!(!news.is_unread(pane(3)));
    }

    /// A pane marked read has its dot go as if you were just in it, whatever
    /// is new in it: what finished there you saw, though the hook dates its
    /// end after now, and its unread mark goes. What finishes there after is
    /// new again.
    #[test]
    fn a_pane_marked_read_loses_its_dot_until_something_new_finishes() {
        let mut news = News::default();
        news.mark_unread(pane(2), at(101));
        news.read(pane(1), finished(pane(1)), ms(101_500));
        news.read(pane(2), finished(pane(2)), ms(101_500));
        assert_eq!(new(&news), [false, false]);
        assert!(!news.is_unread(pane(2)));

        let later = |key: Key| finished(key).map(|at| at + Duration::from_secs(10));
        assert!([1, 2].iter().all(|id| news.is_new(pane(*id), later(pane(*id)))));
    }

    /// `u` on a pane says what you saw there, from wherever you are: it does
    /// not make that the session you looked at last.
    #[test]
    fn marking_a_pane_read_does_not_say_you_were_in_it() {
        let mut news = News::default();
        news.read(pane(1), finished(pane(1)), at(200));
        assert_eq!(news.last_seen(session(10)), None);
    }

    /// A session the daemon has not listed for `GONE` takes what you saw and
    /// marked in it with it. One it leaves out of a list for less, as a
    /// rename can, keeps it, and so does each while the daemon lists one it
    /// cannot tell from another.
    #[test]
    fn a_session_gone_takes_its_news_with_it() {
        let mut news = News::default();
        dwell(&mut news, Some(pane(1)), at(200));
        dwell(&mut news, Some(pane(1)), at(201));
        news.mark_unread(pane(2), at(201));
        let listing = |session: Session| {
            let mut live = Lists::default();
            live.lists(&Machine::Local, [session], true);
            live
        };
        let (live, other) = (listing(session(10)), listing(session(20)));
        news.listed(Some(&other), steady(at(210)));
        news.listed(Some(&live), steady(at(215)));
        news.listed(Some(&other), steady(at(220)));
        news.listed(None, steady(at(225)));
        news.listed(Some(&other), steady(at(231)));
        assert_eq!(new(&news), [false, true]);
        assert!(news.is_unread(pane(2)));
        assert_eq!(news.last_seen(session(10)), Some(at(201)));

        news.listed(Some(&other), steady(at(240)));
        news.listed(Some(&other), steady(at(241)));
        assert_eq!(new(&news), [true, true]);
        assert!(!news.is_unread(pane(2)));
        assert_eq!(news.last_seen(session(10)), None);
    }

    /// The news of a luneta that comes before any daemon makes luneta's
    /// runtime folder as a daemon takes it, for you alone. In one open to
    /// others, it is this luneta's alone, and says why.
    #[test]
    fn the_runtime_folder_the_news_makes_is_one_a_daemon_takes() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("luneta");
        let news = News::within(&runtime);
        assert!(matches!(folder::open(&runtime), Ok(Some(_))));
        assert_eq!((news.lost(), news.dirs().len()), (None, 3));

        fs::set_permissions(&runtime, Permissions::from_mode(0o755)).unwrap();
        let news = News::within(&runtime);
        let open = news.lost().is_some_and(|lost| lost.contains("is open to other users"));
        assert!(open, "{:?}", news.lost());
    }
}
