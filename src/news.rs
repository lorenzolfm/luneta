//! What is new in each pane since you were last in it: an agent that
//! finished, or a command its shell finished. The sidebar dots its row
//! until you go there.
//!
//! You were in a pane once you stayed there a moment, `DWELL`, so that a
//! pane you only pass through on the way to another keeps its dot. A pane's
//! visit and what its shell ran are kept in a `Ledger`, the same for every
//! session's sidebar, each of which sees you only in its own session's panes,
//! and the shells only while its session has a client.
//!
//! An agent finished when claude-ps says it came to `idle`. A shell says what
//! it is doing in its title, as fish titles its pane: the command it runs,
//! and the cwd once it is back at its prompt. A command finished once a title
//! that named it gives way to the prompt.
//!
//! The ledgers keep a session's records by its name, and a session is its
//! name and when it was made: what a pane ran before its session was made, a
//! session made earlier by the same name ran, in a pane of its own. And a
//! session that turns up by a new name, made when one that leaves was, is
//! that one renamed, whoever renamed it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use crate::agents::{AgentRow, AgentSet, Status};
use crate::elapsed::{Held, Moment};
use crate::ledger::{Ledger, Record};
use crate::tree::{self, Branch, Doing};

const VISITED: &str = "/cache/visited";

const RAN: &str = "/cache/ran";

/// How long you stay in a pane before it counts as a visit.
const DWELL: Held = Held::from_secs(1);

/// How many `prune`s in a row a session is missing from before its records
/// go: a session can drop out of one list as zellij writes it again.
const GONE: u32 = 5;

/// How far apart two readings of when a session was made may be and still
/// be the same moment: zellij counts a session's age in whole seconds, from
/// a list it read a moment before.
const MADE: Held = Held::from_secs(3);

/// What a shell ran last, as its title said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    /// The command it runs, as fish cut it, since a sidebar first saw it.
    Going { command: String, since: Moment },
    /// The command it ran, and when a sidebar saw it back at its prompt.
    Ended { command: String, at: Moment },
}

impl Run {
    /// When a sidebar saw it start, or end.
    fn moment(&self) -> Moment {
        match self {
            Run::Going { since, .. } => *since,
            Run::Ended { at, .. } => *at,
        }
    }
}

const GOING: &str = "going";

const ENDED: &str = "ended";

impl Record for Run {
    fn read(text: &str) -> Option<Self> {
        let mut parts = text.trim_end_matches('\n').splitn(3, ' ');
        let (word, millis, command) = (parts.next()?, parts.next()?, parts.next()?);
        let moment = Moment::from_millis(millis.parse().ok()?);
        let command = command.to_owned();
        match word {
            GOING => Some(Run::Going { command, since: moment }),
            ENDED => Some(Run::Ended { command, at: moment }),
            _ => None,
        }
    }

    fn write(&self) -> String {
        let (word, moment, command) = match self {
            Run::Going { command, since } => (GOING, since, command),
            Run::Ended { command, at } => (ENDED, at, command),
        };
        format!("{word} {} {command}\n", moment.millis())
    }
}

/// When you were last in a pane.
impl Record for Moment {
    fn read(text: &str) -> Option<Self> {
        text.trim().parse().ok().map(Moment::from_millis)
    }

    fn write(&self) -> String {
        format!("{}\n", self.millis())
    }
}

#[derive(Default)]
pub struct News {
    visits: Ledger<Moment>,
    runs: Ledger<Run>,
    /// The pane you were in as the last `watch` saw, and since when.
    dwelling: Option<Dwelling>,
    /// How many `prune`s in a row have found each session missing.
    missing: BTreeMap<String, u32>,
    /// When each session the last `watch` saw was made, to know one renamed.
    made: BTreeMap<String, Moment>,
    /// When the last `watch` was, or before the first, when the news was read.
    now: Moment,
}

#[derive(Clone)]
struct Dwelling {
    session: String,
    pane: u32,
    since: Moment,
}

impl News {
    /// The news every session's sidebar shares, as it stands.
    pub fn shared() -> Self {
        News {
            visits: Ledger::within(PathBuf::from(VISITED)),
            runs: Ledger::within(PathBuf::from(RAN)),
            now: Moment::now(),
            ..News::default()
        }
    }

    /// Takes up what the other sidebars saw since.
    pub fn reload(&mut self) {
        self.visits.reload();
        self.runs.reload();
    }

    /// Takes up what every shell's title in `branches` says it is doing at
    /// `now`, and that you are in `here`: a pane you stayed in has been
    /// visited, once there is something new in it. So has the one you were
    /// in as the last `watch` saw, if you stayed there, which you have left
    /// since: what it finished as you left it, you saw.
    pub fn watch(
        &mut self,
        branches: &[Branch],
        agents: &AgentSet,
        here: Option<(&str, u32)>,
        now: Moment,
    ) {
        self.now = now;
        let made: BTreeMap<String, Moment> =
            branches.iter().map(|branch| (branch.name.clone(), now.before(branch.age))).collect();
        self.follow(&made);
        let seated: HashMap<(&str, u32), &AgentRow> =
            agents.rows.iter().map(|row| ((row.seat.session(), row.pane), row)).collect();
        for branch in branches {
            let born = now.before(branch.age).back(MADE);
            for leaf in &branch.panes {
                if seated.contains_key(&(branch.name.as_str(), leaf.id)) {
                    continue;
                }
                let kept = self.runs.get(&branch.name, leaf.id);
                // One from before its session was made, another session made
                // by the same name ran, in a pane of its own.
                let run = kept.filter(|run| run.moment() >= born);
                let next = step(run, &tree::doing(&leaf.title, leaf.cwd.as_deref()), now);
                if next.as_ref() != kept {
                    self.runs.set(&branch.name, leaf.id, next);
                }
            }
        }
        self.made = made;

        let before = self.dwelling.take();
        self.dwelling = here.map(|(session, pane)| match &before {
            Some(dwelling) if dwelling.session == session && dwelling.pane == pane => {
                dwelling.clone()
            },
            _ => Dwelling { session: session.to_owned(), pane, since: now },
        });
        for dwelt in before.iter().chain(&self.dwelling) {
            if now.since(dwelt.since) < DWELL {
                continue;
            }
            let (session, pane) = (dwelt.session.as_str(), dwelt.pane);
            let agent = seated.get(&(session, pane)).copied();
            let visited = self.visits.get(session, pane).copied();
            if self.latest(session, pane, agent) > visited {
                self.visits.set(session, pane, Some(now));
            }
        }
    }

    /// Whether `agent`, in `pane` of `session`, finished since you were last
    /// there.
    pub fn finished(&self, session: &str, pane: u32, agent: &AgentRow) -> bool {
        let visited = self.visits.get(session, pane).copied();
        agent.status == Status::Idle && Some(agent.came) > visited
    }

    /// The command the shell in `pane` of `session` finished since you were
    /// last there, and how long ago.
    pub fn ran(&self, session: &str, pane: u32) -> Option<(&str, Held)> {
        let visited = self.visits.get(session, pane).copied();
        match self.runs.get(session, pane)? {
            Run::Ended { command, at } if Some(*at) > visited => {
                Some((command.as_str(), self.now.since(*at)))
            },
            Run::Ended { .. } | Run::Going { .. } => None,
        }
    }

    /// When the last thing finished in a pane, whether its agent or its
    /// shell: a visit after that has seen it all.
    fn latest(&self, session: &str, pane: u32, agent: Option<&AgentRow>) -> Option<Moment> {
        let finished = agent.filter(|agent| agent.status == Status::Idle).map(|agent| agent.came);
        let ran = match self.runs.get(session, pane) {
            Some(Run::Ended { at, .. }) => Some(*at),
            Some(Run::Going { .. }) | None => None,
        };
        finished.max(ran)
    }

    /// Forgets the records of each session that `live` has left out of as
    /// many lists in a row as `GONE` says: it is gone, and a session made
    /// later by its name numbers its panes from the start again.
    pub fn prune(&mut self, live: &[String]) {
        let kept: BTreeSet<String> =
            self.visits.sessions().chain(self.runs.sessions()).map(str::to_owned).collect();
        let mut missing = BTreeMap::new();
        for session in kept.into_iter().filter(|session| !live.contains(session)) {
            let lists = self.missing.get(&session).map_or(1, |lists| lists + 1);
            if lists < GONE {
                missing.insert(session, lists);
                continue;
            }
            self.visits.forget(&session);
            self.runs.forget(&session);
        }
        self.missing = missing;
    }

    /// Hands what was kept of each session that has left the list since the
    /// last `watch` to the one that has joined it, made at the same moment
    /// as `made` says, and no other: zellij renamed it, whoever asked it to.
    fn follow(&mut self, made: &BTreeMap<String, Moment>) {
        let at_once = |a: Moment, b: Moment| a.since(b).max(b.since(a)) < MADE;
        let mut renamed = Vec::new();
        for (from, was) in &self.made {
            if made.contains_key(from) {
                continue;
            }
            let mut joined = made
                .iter()
                .filter(|(to, is)| !self.made.contains_key(*to) && at_once(**is, *was))
                .map(|(to, _)| to);
            if let (Some(to), None) = (joined.next(), joined.next()) {
                renamed.push((from.clone(), to.clone()));
            }
        }
        for (from, to) in renamed {
            self.visits.rename(&from, &to);
            self.runs.rename(&from, &to);
        }
    }
}

/// What a shell ran last, as it was `run`, once its title says it is `doing`
/// something at `now`. A title that is neither the prompt nor a command says
/// nothing of whether the command ended, and the record of one going goes.
fn step(run: Option<&Run>, doing: &Doing, now: Moment) -> Option<Run> {
    match (run, doing) {
        (Some(Run::Going { command, .. }), Doing::Running(running)) if command == running => {
            run.cloned()
        },
        (_, Doing::Running(command)) => Some(Run::Going { command: command.clone(), since: now }),
        (Some(Run::Going { command, .. }), Doing::Prompt) => {
            Some(Run::Ended { command: command.clone(), at: now })
        },
        (Some(Run::Going { .. }), Doing::Unknown) => None,
        (Some(Run::Ended { .. }) | None, Doing::Prompt | Doing::Unknown) => run.cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Live;
    use crate::close::Closes;
    use crate::elapsed::Age;
    use crate::places::Places;
    use crate::sessions::Selection;
    use crate::tree::Leaf;

    fn at(secs: u64) -> Moment {
        Moment::from_millis(secs * 1000)
    }

    #[test]
    fn a_run_reads_as_it_is_written() {
        for run in [
            Run::Going { command: "cargo build --relea".to_owned(), since: at(5) },
            Run::Ended { command: "make".to_owned(), at: at(9) },
        ] {
            assert_eq!(Run::read(&run.write()), Some(run.clone()), "{run:?}");
        }
        assert_eq!(Run::read("going 12"), None);
        assert_eq!(Run::read("gone 12 make"), None);
        assert_eq!(Moment::read(&at(7).write()), Some(at(7)));
    }

    /// A command runs, then ends: the prompt after it says it ended, and when.
    /// One that gives way to another ran no end the shell showed, and a title
    /// that says neither forgets the one going, which may never end.
    #[test]
    fn a_shell_ran_a_command_once_its_title_goes_back_to_the_prompt() {
        let going = |command: &str, since| Run::Going { command: command.to_owned(), since };
        let ended = |command: &str, at| Run::Ended { command: command.to_owned(), at };
        let running = |command: &str| Doing::Running(command.to_owned());
        for (run, doing, after) in [
            (None, running("make"), Some(going("make", at(9)))),
            (Some(going("make", at(2))), running("make"), Some(going("make", at(2)))),
            (Some(going("make", at(2))), running("nvim"), Some(going("nvim", at(9)))),
            (Some(going("make", at(2))), Doing::Prompt, Some(ended("make", at(9)))),
            (Some(going("make", at(2))), Doing::Unknown, None),
            (Some(ended("make", at(2))), Doing::Prompt, Some(ended("make", at(2)))),
            (Some(ended("make", at(2))), Doing::Unknown, Some(ended("make", at(2)))),
            (Some(ended("make", at(2))), running("make"), Some(going("make", at(9)))),
            (None, Doing::Prompt, None),
            (None, Doing::Unknown, None),
        ] {
            let case = format!("{run:?} then {doing:?}");
            assert_eq!(step(run.as_ref(), &doing, at(9)), after, "{case}");
        }
    }

    /// Session `s`, made at 0, with a shell in pane 1, as titled, and an
    /// agent in pane 2 whose status came `age` seconds before `now`.
    fn session(title: &str, agent: &str, age: u64, now: Moment) -> (Vec<Branch>, AgentSet) {
        let leaf = |id: u32, title: &str| Leaf {
            id,
            title: title.to_owned(),
            tab: String::new(),
            tab_id: None,
            cwd: Some("/home/you/w/s".to_owned()),
            runs: None,
            checkout: None,
            closes: Closes::Pane,
        };
        let branches = vec![Branch {
            name: "s".to_owned(),
            age: Age::from_secs(now.millis() / 1000),
            panes: vec![leaf(1, title), leaf(2, "✳ Claude Code")],
        }];
        let json = format!(
            r#"[{{"status":"{agent}","status_age":{age},"pid":7,"cwd":"/home/you/w/s",
                 "zellij":{{"session":"s","pane":"2"}}}}]"#
        );
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), json.as_bytes(), b"", now);
        let places = Places::of(&[("s", &[(1, "/home/you/w/s"), (2, "/home/you/w/s")])]);
        let live = Live::new(Some("s"), &[], &places, &branches);
        agents.rebuild("", &live, None, Age::ZERO, Selection::SnapToTop);
        (branches, agents)
    }

    /// What the shell in pane 1 finished, and how long ago, and whether the
    /// agent in pane 2 finished, since you were last in each.
    fn news_of(news: &News, agents: &AgentSet) -> (Option<(String, Held)>, bool) {
        let agent = agents.rows.iter().find(|row| row.pane == 2).expect("an agent in pane 2");
        let ran = news.ran("s", 1).map(|(command, age)| (command.to_owned(), age));
        (ran, news.finished("s", 2, agent))
    }

    fn ran(command: &str, age: u64) -> (String, Held) {
        (command.to_owned(), Held::from_secs(age))
    }

    /// The agent in pane 2 finishes at 100, and the shell in pane 1 runs
    /// `cargo test` until 103: both are new until you stay in their panes,
    /// and new again once each finishes once more.
    #[test]
    fn what_finished_is_new_until_you_stay_in_its_pane() {
        let mut news = News::default();
        let (busy, agents) = session("cargo test ~/w/s", "busy", 0, at(98));
        news.watch(&busy, &agents, None, at(98));
        let (idle, agents) = session("~/w/s", "idle", 3, at(103));
        news.watch(&idle, &agents, None, at(103));
        assert_eq!(news_of(&news, &agents), (Some(ran("cargo test", 0)), true));

        // Passing through pane 2, and staying in pane 1 for a second.
        news.watch(&idle, &agents, Some(("s", 2)), at(104));
        news.watch(&idle, &agents, Some(("s", 1)), at(104));
        news.watch(&idle, &agents, Some(("s", 1)), at(105));
        assert_eq!(news_of(&news, &agents), (None, true));
        // Staying in pane 2, then on to pane 1 before another `watch`.
        news.watch(&idle, &agents, Some(("s", 2)), at(106));
        news.watch(&idle, &agents, Some(("s", 2)), at(107));
        news.watch(&idle, &agents, Some(("s", 1)), at(108));
        assert_eq!(news_of(&news, &agents), (None, false));

        let (busy, agents) = session("cargo test ~/w/s", "busy", 0, at(110));
        news.watch(&busy, &agents, None, at(110));
        let (idle, agents) = session("~/w/s", "idle", 1, at(113));
        news.watch(&idle, &agents, None, at(113));
        news.watch(&idle, &agents, None, at(120));
        assert_eq!(news_of(&news, &agents), (Some(ran("cargo test", 7)), true));
    }

    /// The pane you stay in takes no visit while nothing in it is new, so
    /// that what finishes after you leave it is.
    #[test]
    fn a_command_that_ends_after_you_leave_its_pane_is_new() {
        let mut news = News::default();
        let (running, agents) = session("make ~/w/s", "busy", 0, at(100));
        for now in 100..110 {
            news.watch(&running, &agents, Some(("s", 1)), at(now));
        }
        news.watch(&running, &agents, Some(("s", 2)), at(110));
        let (prompt, agents) = session("~/w/s", "busy", 0, at(111));
        news.watch(&prompt, &agents, Some(("s", 2)), at(111));
        assert_eq!(news_of(&news, &agents), (Some(ran("make", 0)), false));
    }

    /// A session made again by the name of one gone, before any sidebar saw
    /// the one go, has none of what that one's shells ran: its pane 1 is
    /// another pane.
    #[test]
    fn a_session_made_again_by_its_name_has_none_of_what_the_last_one_ran() {
        let mut news = News::default();
        let (running, agents) = session("make ~/w/s", "busy", 0, at(100));
        news.watch(&running, &agents, None, at(100));
        let (prompt, agents) = session("~/w/s", "busy", 0, at(101));
        news.watch(&prompt, &agents, None, at(101));
        assert_eq!(news_of(&news, &agents).0, Some(ran("make", 0)));

        let (mut again, agents) = session("~/w/s", "busy", 0, at(500));
        again[0].age = Age::from_secs(10);
        news.watch(&again, &agents, None, at(500));
        assert_eq!(news_of(&news, &agents).0, None);
        assert_eq!(news.runs.get("s", 1), None);
    }

    /// A session renamed, by the picker or by zellij itself, keeps under its
    /// new name what its shells ran and what you saw in it: zellij says it
    /// was made when it was.
    #[test]
    fn a_session_renamed_keeps_what_finished_in_it_and_what_you_saw() {
        /// Session `s` by `name`, with its shell titled, at `now`.
        fn watch(news: &mut News, name: &str, title: &str, here: Option<u32>, now: u64) {
            let (mut branches, agents) = session(title, "busy", 0, at(now));
            branches[0].name = name.to_owned();
            news.watch(&branches, &agents, here.map(|pane| (name, pane)), at(now));
        }
        let mut news = News::default();
        watch(&mut news, "s", "make ~/w/s", None, 100);
        watch(&mut news, "s", "~/w/s", None, 101);
        watch(&mut news, "t", "~/w/s", None, 102);
        assert_eq!(news.ran("t", 1).map(|(command, _)| command), Some("make"));
        assert_eq!(news.ran("s", 1), None);

        // You stay in it under its new name, and it is renamed back.
        watch(&mut news, "t", "~/w/s", Some(1), 103);
        watch(&mut news, "t", "~/w/s", Some(1), 104);
        watch(&mut news, "s", "~/w/s", None, 105);
        assert!(news.runs.get("s", 1).is_some());
        assert_eq!(news.ran("s", 1), None);
        assert_eq!(news.runs.get("t", 1), None);
    }

    /// A session missing from the session list loses its records only once
    /// it has been missing from `GONE` lists in a row.
    #[test]
    fn a_session_s_records_go_once_it_stays_gone() {
        let mut news = News::default();
        let (idle, agents) = session("~/w/s", "idle", 1, at(10));
        news.watch(&idle, &agents, Some(("s", 2)), at(10));
        news.watch(&idle, &agents, Some(("s", 2)), at(11));
        assert!(!news_of(&news, &agents).1);

        let live = ["s".to_owned()];
        for lists in 1..GONE {
            news.prune(&[]);
            assert!(!news_of(&news, &agents).1, "{lists}");
        }
        news.prune(&live);
        for _ in 0..GONE {
            news.prune(&[]);
        }
        assert!(news_of(&news, &agents).1);
    }
}
