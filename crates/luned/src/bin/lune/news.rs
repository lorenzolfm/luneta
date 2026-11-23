use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use luned::folder;
use notify::RecommendedWatcher;

use crate::host::Event;
use crate::ledger::{Key, Ledger, Session};
use crate::live::{Lists, Missing};
use crate::state;

const DWELL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct News {
    seen: Ledger<Key>,
    stayed: Ledger<Session>,
    unread: Ledger<Key>,
    dwelling: Option<(Key, SystemTime, Instant)>,
    projects_missing: Missing<Session>,
    views_missing: Missing<Key>,
}

impl News {
    pub fn within(runtime: &Path) -> Self {
        match folder::make(runtime) {
            Ok(folder) => News {
                seen: Ledger::within(folder.path().join("seen")),
                stayed: Ledger::within(folder.path().join("stayed")),
                unread: Ledger::within(folder.path().join("unread")),
                ..News::default()
            },
            Err(refusal) => News::unshared(&refusal.to_string()),
        }
    }

    pub fn unshared(why: &str) -> Self {
        News {
            seen: Ledger::unshared(why.to_owned()),
            stayed: Ledger::unshared(why.to_owned()),
            unread: Ledger::unshared(why.to_owned()),
            ..News::default()
        }
    }

    pub fn lost(&self) -> Option<&str> {
        self.seen.lost().or(self.stayed.lost()).or(self.unread.lost())
    }

    pub fn reload(&mut self) -> bool {
        self.stayed.reload();
        let seen = self.seen.reload();
        self.unread.reload() || seen
    }

    pub fn is_new(&self, key: Key, finished: Option<SystemTime>) -> bool {
        finished.is_some_and(|finished| self.seen.get(key).is_none_or(|seen| seen < finished))
    }

    pub fn is_unread(&self, key: Key) -> bool {
        self.unread.get(key).is_some()
    }

    pub fn last_seen(&self, session: Session) -> Option<SystemTime> {
        self.stayed.get(session)
    }

    pub fn mark_unread(&mut self, key: Key, now: SystemTime) {
        self.unread.set(key, Some(now));
    }

    pub fn read(&mut self, key: Key, finished: Option<SystemTime>, now: SystemTime) {
        self.saw(key, finished, now);
        if self.is_unread(key) {
            self.unread.set(key, None);
        }
    }

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

    fn visit(
        &mut self,
        key: Key,
        since: SystemTime,
        finished: Option<SystemTime>,
        now: SystemTime,
    ) -> bool {
        if self.stayed.get(key.session).is_none_or(|stayed| stayed < since) {
            self.stayed.set(key.session, Some(now));
        }
        let mut changed = self.saw(key, finished, now);
        if self.unread.get(key).is_some_and(|marked| marked < since) {
            self.unread.set(key, None);
            changed = true;
        }
        changed
    }

    fn saw(&mut self, key: Key, finished: Option<SystemTime>, now: SystemTime) -> bool {
        if !self.is_new(key, finished) {
            return false;
        }
        self.seen.set(key, finished.map(|finished| finished.max(now)));
        true
    }

    pub fn listed(&mut self, live: Option<&Lists>, at: Instant) {
        let kept: BTreeSet<Session> = self.stayed.kept().collect();
        let gone = self.projects_missing.gone(&kept, live, at);
        self.stayed.forget(&gone);
        let kept: BTreeSet<Key> = self.seen.kept().chain(self.unread.kept()).collect();
        let gone = self.views_missing.gone(&kept, live, at);
        self.seen.forget(&gone);
        self.unread.forget(&gone);
    }

    fn dirs(&self) -> Vec<&Path> {
        [self.seen.dir(), self.stayed.dir(), self.unread.dir()].into_iter().flatten().collect()
    }
}

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

    fn steady(now: SystemTime) -> Instant {
        static START: LazyLock<Instant> = LazyLock::new(Instant::now);
        *START + now.duration_since(SystemTime::UNIX_EPOCH).unwrap()
    }

    fn dwell(news: &mut News, here: Option<Key>, now: SystemTime) -> bool {
        news.dwell(here, finished, now, steady(now))
    }

    fn session(project: u64) -> Session {
        Session::Project(luned::projects::ProjectId::of(project).unwrap())
    }

    fn id(view: u64) -> luned::projects::ViewId {
        luned::projects::ViewId::of(view).unwrap()
    }

    fn view(view: u64) -> Key {
        Key { session: session(10), view: id(view) }
    }

    fn finished(key: Key) -> Option<SystemTime> {
        match key.view.get() {
            1 => Some(at(100)),
            2 => Some(ms(101_999)),
            _ => None,
        }
    }

    fn new(news: &News) -> [bool; 2] {
        [1, 2].map(|id| news.is_new(view(id), finished(view(id))))
    }

    fn listing(session: Session, views: &[u64]) -> Lists {
        let mut live = Lists::default();
        live.lists(&Machine::Local, [(session, views.iter().map(|view| id(*view)))], true);
        live
    }

    #[test]
    fn what_finished_is_new_until_you_stay_in_its_view() {
        let mut news = News::default();
        dwell(&mut news, Some(view(1)), ms(100_000));
        dwell(&mut news, Some(view(2)), ms(100_500));
        assert!(!dwell(&mut news, Some(view(2)), ms(101_000)));
        assert!(dwell(&mut news, Some(view(2)), ms(101_500)));
        dwell(&mut news, None, ms(101_600));
        assert_eq!(new(&news), [true, false]);

        let later = |key: Key| finished(key).map(|at| at + Duration::from_secs(10));
        assert!([1, 2].iter().all(|id| news.is_new(view(*id), later(view(*id)))));
    }

    #[test]
    fn a_stay_dates_the_session_with_nothing_new_in_it() {
        let other = |view: u64| Key { session: session(20), view: id(view) };
        let nothing = |_: Key| None;
        let mut news = News::default();

        news.dwell(Some(view(3)), nothing, at(200), steady(at(200)));
        news.dwell(Some(other(3)), nothing, ms(200_500), steady(ms(200_500)));
        news.dwell(Some(other(3)), nothing, ms(201_500), steady(ms(201_500)));
        assert_eq!(news.last_seen(session(10)), None);
        assert_eq!(news.last_seen(session(20)), Some(ms(201_500)));

        news.dwell(Some(other(3)), nothing, at(900), steady(at(900)));
        assert_eq!(news.last_seen(session(20)), Some(ms(201_500)));

        news.dwell(Some(view(3)), nothing, at(1_000), steady(at(1_000)));
        news.dwell(Some(view(3)), nothing, at(1_001), steady(at(1_001)));
        assert_eq!(news.last_seen(session(10)), Some(at(1_001)));
        assert!(news.is_new(view(3), Some(at(1_002))));
    }

    #[test]
    fn a_view_passed_through_as_the_clock_jumps_keeps_its_dot() {
        let mut news = News::default();
        news.dwell(Some(view(1)), finished, at(200), steady(at(200)));
        news.dwell(None, finished, at(3_800), steady(ms(200_500)));
        assert_eq!(new(&news), [true, true]);
    }

    #[test]
    fn a_view_marked_unread_stays_so_until_you_come_back_to_it() {
        let mut news = News::default();
        dwell(&mut news, Some(view(1)), at(200));
        news.mark_unread(view(1), at(201));
        news.mark_unread(view(3), at(201));
        dwell(&mut news, Some(view(1)), at(205));
        assert!(news.is_unread(view(1)));

        dwell(&mut news, Some(view(3)), at(206));
        dwell(&mut news, Some(view(1)), ms(206_500));
        dwell(&mut news, None, at(207));
        assert!(news.is_unread(view(1)) && news.is_unread(view(3)));
        dwell(&mut news, Some(view(1)), at(208));
        dwell(&mut news, Some(view(1)), at(209));
        assert!(!news.is_unread(view(1)) && news.is_unread(view(3)));

        news.read(view(3), None, at(210));
        assert!(!news.is_unread(view(3)));
    }

    #[test]
    fn a_view_marked_read_loses_its_dot_until_something_new_finishes() {
        let mut news = News::default();
        news.mark_unread(view(2), at(101));
        news.read(view(1), finished(view(1)), ms(101_500));
        news.read(view(2), finished(view(2)), ms(101_500));
        assert_eq!(new(&news), [false, false]);
        assert!(!news.is_unread(view(2)));

        let later = |key: Key| finished(key).map(|at| at + Duration::from_secs(10));
        assert!([1, 2].iter().all(|id| news.is_new(view(*id), later(view(*id)))));
    }

    #[test]
    fn marking_a_view_read_does_not_say_you_were_in_it() {
        let mut news = News::default();
        news.read(view(1), finished(view(1)), at(200));
        assert_eq!(news.last_seen(session(10)), None);
    }

    #[test]
    fn a_session_gone_takes_its_news_with_it() {
        let mut news = News::default();
        dwell(&mut news, Some(view(1)), at(200));
        dwell(&mut news, Some(view(1)), at(201));
        news.mark_unread(view(2), at(201));
        let (live, other) = (listing(session(10), &[1, 2]), listing(session(20), &[1]));
        news.listed(Some(&other), steady(at(210)));
        news.listed(Some(&live), steady(at(215)));
        news.listed(Some(&other), steady(at(220)));
        news.listed(None, steady(at(225)));
        news.listed(Some(&other), steady(at(231)));
        assert_eq!(new(&news), [false, true]);
        assert!(news.is_unread(view(2)));
        assert_eq!(news.last_seen(session(10)), Some(at(201)));

        news.listed(Some(&other), steady(at(240)));
        news.listed(Some(&other), steady(at(241)));
        assert_eq!(new(&news), [true, true]);
        assert!(!news.is_unread(view(2)));
        assert_eq!(news.last_seen(session(10)), None);
    }

    #[test]
    fn a_view_closed_takes_its_marks_and_leaves_its_project_s_moment() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("lune");
        let mut news = News::within(&runtime);
        dwell(&mut news, Some(view(1)), at(200));
        dwell(&mut news, Some(view(1)), at(201));
        news.mark_unread(view(1), at(202));
        news.mark_unread(view(2), at(202));
        let mut open = listing(session(10), &[2]);
        open.unreached(&Machine::Remote("me@vps".parse().unwrap()));
        news.listed(Some(&open), steady(at(210)));
        news.listed(Some(&open), steady(at(219)));
        assert_eq!((new(&news), news.is_unread(view(1))), ([false, true], true));

        news.listed(Some(&open), steady(at(220)));
        for news in [&news, &News::within(&runtime)] {
            assert_eq!(new(news), [true, true]);
            assert_eq!([1, 2].map(|id| news.is_unread(view(id))), [false, true]);
            assert_eq!(news.last_seen(session(10)), Some(at(201)));
        }
    }

    #[test]
    fn the_runtime_folder_the_news_makes_is_one_a_daemon_takes() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("lune");
        let news = News::within(&runtime);
        assert!(matches!(folder::open(&runtime), Ok(Some(_))));
        assert_eq!((news.lost(), news.dirs().len()), (None, 3));

        fs::set_permissions(&runtime, Permissions::from_mode(0o755)).unwrap();
        let news = News::within(&runtime);
        let open = news.lost().is_some_and(|lost| lost.contains("is open to other users"));
        assert!(open, "{:?}", news.lost());
    }
}
