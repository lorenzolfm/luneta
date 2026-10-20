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
//! An agent finished when claude-ps says it came to `idle`. A shell with
//! luneta's hook says what it runs, and how it ended, in `Shells`, and only a
//! command that ran a while, `LONG`, counts: an `ls` is no news. A shell
//! without it says what it is doing in its title, as fish titles its pane:
//! the command it runs, and the cwd once it is back at its prompt. A command
//! finished once a title that named it gives way to the prompt.
//!
//! Every sidebar reads what the shells ran from the session list, whose
//! titles are the same for all of them, and seconds late. A visit reads the
//! title the pane gives itself too, which zellij sends as it changes: a
//! command you saw end, the session list may say ended only once you have
//! left.
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
use crate::shells::{Front, Said, Shells};
use crate::tree::{self, Branch, Doing, Mark, Outcome};

const VISITED: &str = "/cache/visited";

const RAN: &str = "/cache/ran";

/// How long you stay in a pane before it counts as a visit.
const DWELL: Held = Held::from_secs(1);

/// How long a command runs before its row says it runs, and says how it
/// ended once it does.
const LONG: Held = Held::from_secs(1);

/// How many lists in a row a session, or a pane of one, is missing from
/// before its records go: one can drop out of a list as zellij writes it
/// again.
const GONE: u32 = 5;

/// How far apart two readings of when a session was made may be and still
/// be the same moment: zellij counts a session's age in whole seconds, from
/// a list it read a moment before.
const MADE: Held = Held::from_secs(3);

/// How much later than another a sidebar may date the moment an agent
/// finished. Each dates it from its own answer of claude-ps, which counts
/// whole seconds from a whole second: one may put it two seconds after
/// another, and so after the visit the other wrote as you watched the agent
/// finish.
const SKEW: Held = Held::from_secs(3);

/// How far the session list's titles may lag behind the title a pane gives
/// itself: zellij writes them out every second, and they show a shell back
/// at its prompt some four seconds after its command ended.
const LAG: Held = Held::from_secs(10);

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

/// When you were last in a pane, and what its shell was doing then, as the
/// title the pane gave itself said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    at: Moment,
    doing: Doing,
}

impl Visit {
    /// Whether you saw an agent finish, as `came` dates it: by the time you
    /// were there, as late as another sidebar may date what this one did.
    fn saw_finish(&self, came: Moment) -> bool {
        came.since(self.at) <= SKEW
    }

    /// Whether you saw `command` end, as its record dates it `at`: by the
    /// time you were there, or, if the pane's own title had ended it by
    /// then, as late as the session list may have taken to say so.
    fn saw_end(&self, command: &str, at: Moment) -> bool {
        at <= self.at || (at.since(self.at) <= LAG && ends(&self.doing, command))
    }
}

/// Whether a shell `doing` this is done with `command`: back at its prompt,
/// or on to another.
fn ends(doing: &Doing, command: &str) -> bool {
    match doing {
        Doing::Prompt => true,
        Doing::Running(running) => running != command,
        Doing::Unknown => false,
    }
}

const PROMPT: &str = "prompt";

const RUNNING: &str = "running";

impl Record for Visit {
    fn read(text: &str) -> Option<Self> {
        let text = text.trim_end_matches('\n');
        let (millis, doing) = match text.split_once(' ') {
            None => (text, Doing::Unknown),
            Some((millis, PROMPT)) => (millis, Doing::Prompt),
            Some((millis, said)) => {
                let command = said.strip_prefix(RUNNING)?.strip_prefix(' ')?;
                (millis, Doing::Running(command.to_owned()))
            },
        };
        Some(Visit { at: Moment::from_millis(millis.parse().ok()?), doing })
    }

    fn write(&self) -> String {
        let millis = self.at.millis();
        match &self.doing {
            Doing::Prompt => format!("{millis} {PROMPT}\n"),
            Doing::Running(command) => format!("{millis} {RUNNING} {command}\n"),
            Doing::Unknown => format!("{millis}\n"),
        }
    }
}

#[derive(Default)]
pub struct News {
    visits: Ledger<Visit>,
    runs: Ledger<Run>,
    /// What the shells with luneta's hook say, as the host keeps it.
    pub shells: Shells,
    /// The pane you were in as the last `watch` saw, and since when.
    dwelling: Option<Dwelling>,
    /// How many `prune`s in a row have found each session missing.
    missing: BTreeMap<String, u32>,
    /// How many `watch`es in a row have found each pane with a record
    /// missing from its session, which was listed.
    closed: BTreeMap<(String, u32), u32>,
    /// When each session the last `watch` saw was made, to know one renamed.
    made: BTreeMap<String, Moment>,
    /// When the last `watch` was, or before the first, when the news was read.
    now: Moment,
}

struct Dwelling {
    session: String,
    pane: u32,
    since: Moment,
    /// The command the pane's own title last said its shell ran while you
    /// were there, until a visit saw it end.
    ran: Option<String>,
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
    /// `now`, and that you are in `here`, the panes of whose session give
    /// themselves the titles `own` says. A pane you stayed in has been
    /// visited, once there is something new in it, or its own title has
    /// ended a command the session list has yet to. So has the one you were
    /// in as the last `watch` saw, if you stayed there, which you have left
    /// since: what it finished as you left it, you saw.
    pub fn watch(
        &mut self,
        branches: &[Branch],
        agents: &AgentSet,
        here: Option<(&str, u32)>,
        own: impl Fn(u32) -> Option<String>,
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
        self.sweep(branches);

        // What the shell in a pane you were in is doing, by its own title.
        let doing_of = |dwelt: &Dwelling| {
            let Some(title) = own(dwelt.pane) else {
                return Doing::Unknown;
            };
            let leaf = branches
                .iter()
                .filter(|branch| branch.name == dwelt.session)
                .flat_map(|branch| &branch.panes)
                .find(|leaf| leaf.id == dwelt.pane);
            tree::doing(&title, leaf.and_then(|leaf| leaf.cwd.as_deref()))
        };
        let (left, mut dwelling) = match (self.dwelling.take(), here) {
            (Some(before), Some((session, pane)))
                if before.session == session && before.pane == pane =>
            {
                (None, Some(before))
            },
            (before, here) => {
                let dwelling = here.map(|(session, pane)| Dwelling {
                    session: session.to_owned(),
                    pane,
                    since: now,
                    ran: None,
                });
                (before, dwelling)
            },
        };
        if let Some(left) = &left {
            let agent = seated.get(&(left.session.as_str(), left.pane)).copied();
            self.visit(left, &doing_of(left), agent, now);
        }
        if let Some(dwelling) = &mut dwelling {
            let agent = seated.get(&(dwelling.session.as_str(), dwelling.pane)).copied();
            let doing = doing_of(dwelling);
            let visited = self.visit(dwelling, &doing, agent, now);
            match doing {
                Doing::Running(command) => dwelling.ran = Some(command),
                Doing::Prompt | Doing::Unknown if visited => dwelling.ran = None,
                Doing::Prompt | Doing::Unknown => {},
            }
        }
        self.dwelling = dwelling;
    }

    /// Writes that you were in the pane you are or were in as `dwelt`, its
    /// shell `doing` this by its own title, once you stayed a while and there
    /// is something new in it to have seen: what finished, as its records
    /// say, a command its record has going that its own title has ended, or
    /// one its own title said it ran while you were there, and has ended.
    /// Says whether it wrote.
    fn visit(
        &mut self,
        dwelt: &Dwelling,
        doing: &Doing,
        agent: Option<&AgentRow>,
        now: Moment,
    ) -> bool {
        if now.since(dwelt.since) < DWELL {
            return false;
        }
        let (session, pane) = (dwelt.session.as_str(), dwelt.pane);
        let visit = self.visits.get(session, pane);
        let finished = self.latest(session, pane, agent) > visit.map(|visit| visit.at);
        let ending = match self.runs.get(session, pane) {
            // Unless a visit since it went saw its title end it already.
            Some(Run::Going { command, since }) => {
                ends(doing, command)
                    && visit.is_none_or(|visit| visit.at < *since || !ends(&visit.doing, command))
            },
            Some(Run::Ended { .. }) | None => false,
        };
        let ran = dwelt.ran.as_deref().is_some_and(|command| ends(doing, command));
        if !(finished || ending || ran) {
            return false;
        }
        self.visits.set(session, pane, Some(Visit { at: now, doing: doing.clone() }));
        true
    }

    /// Whether `agent`, in `pane` of `session`, finished since you were last
    /// there.
    pub fn finished(&self, session: &str, pane: u32, agent: &AgentRow) -> bool {
        let visit = self.visits.get(session, pane);
        agent.status == Status::Idle && visit.is_none_or(|visit| !visit.saw_finish(agent.came))
    }

    /// The command the shell in `pane` of `session` finished since you were
    /// last there, and how long ago.
    fn ran(&self, session: &str, pane: u32) -> Option<(&str, Held)> {
        let visit = self.visits.get(session, pane);
        match self.runs.get(session, pane)? {
            Run::Ended { command, at } if visit.is_none_or(|visit| !visit.saw_end(command, *at)) => {
                Some((command.as_str(), self.now.since(*at)))
            },
            Run::Ended { .. } | Run::Going { .. } => None,
        }
    }

    /// The mark of the command the shell in `pane` of `session` runs, or
    /// finished since you were last there, and the command. A shell with the
    /// hook says how one ended, and the title of one without, only that it
    /// did. A command that runs as an editor does, as long as you use it, has
    /// none. You see what ended in the pane you are in, `here`.
    pub fn command(&self, session: &str, pane: u32, here: bool) -> Option<(Mark, &str)> {
        let Some(said) = self.said(session, pane) else {
            let (command, age) = self.ran(session, pane).filter(|_| !here)?;
            return Some((Mark::Ran { age, outcome: Outcome::Untold }, command));
        };
        match said {
            Said::Going { command, since, tty } => {
                let age = self.now.since(*since);
                // A terminal that said nothing says neither.
                let mark = match self.shells.front(tty) {
                    _ if age < LONG => return None,
                    Some(Front::Working) => Mark::Running { age },
                    Some(Front::Asking) => Mark::Asking { age },
                    Some(Front::Interactive) | None => return None,
                };
                Some((mark, command))
            },
            Said::Ended { command, at, status, took } => {
                let visit = self.visits.get(session, pane);
                if here || *took < LONG || visit.is_some_and(|visit| *at <= visit.at) {
                    return None;
                }
                let outcome = match status {
                    Some(0) => Outcome::Passed,
                    Some(_) => Outcome::Failed,
                    None => Outcome::Untold,
                };
                Some((Mark::Ran { age: self.now.since(*at), outcome }, command))
            },
            // A job stopped with `Ctrl z` neither runs nor ended.
            Said::Stopped => None,
        }
    }

    /// What the hook in `pane` of `session` said last, if its shell started
    /// in that session: one that started before the session was made is a
    /// shell of another session by that name, gone or renamed since, which
    /// goes on writing under the name it had.
    fn said(&self, session: &str, pane: u32) -> Option<&Said> {
        let born = self.made.get(session).map(|made| made.back(MADE));
        let (started, said) = self.shells.said(session, pane)?;
        born.is_none_or(|born| started >= born).then_some(said)
    }

    /// When the last thing finished in a pane, whether its agent or its
    /// shell: a visit after that has seen it all.
    fn latest(&self, session: &str, pane: u32, agent: Option<&AgentRow>) -> Option<Moment> {
        let finished = agent.filter(|agent| agent.status == Status::Idle).map(|agent| agent.came);
        let ran = match (self.said(session, pane), self.runs.get(session, pane)) {
            (Some(Said::Ended { at, .. }), _) | (None, Some(Run::Ended { at, .. })) => Some(*at),
            (Some(Said::Going { .. } | Said::Stopped), _)
            | (None, Some(Run::Going { .. }) | None) => None,
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

    /// Forgets the records of each pane that its session, as `branches`
    /// lists it, has left out of as many lists in a row as `GONE` says: it
    /// closed, and its records would only pile up.
    fn sweep(&mut self, branches: &[Branch]) {
        let mut closed = BTreeMap::new();
        for branch in branches {
            let open: BTreeSet<u32> = branch.panes.iter().map(|leaf| leaf.id).collect();
            let kept: BTreeSet<u32> = self
                .visits
                .panes(&branch.name)
                .chain(self.runs.panes(&branch.name))
                .filter(|pane| !open.contains(pane))
                .collect();
            for pane in kept {
                let key = (branch.name.clone(), pane);
                let lists = self.closed.get(&key).map_or(1, |lists| lists + 1);
                if lists < GONE {
                    closed.insert(key, lists);
                    continue;
                }
                self.visits.set(&branch.name, pane, None);
                self.runs.set(&branch.name, pane, None);
            }
        }
        self.closed = closed;
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

    /// No pane's own title known, as for a session of another sidebar.
    fn untitled(_: u32) -> Option<String> {
        None
    }

    #[test]
    fn a_record_reads_as_it_is_written() {
        for run in [
            Run::Going { command: "cargo build --relea".to_owned(), since: at(5) },
            Run::Ended { command: "make".to_owned(), at: at(9) },
        ] {
            assert_eq!(Run::read(&run.write()), Some(run.clone()), "{run:?}");
        }
        assert_eq!(Run::read("going 12"), None);
        assert_eq!(Run::read("gone 12 make"), None);
        for doing in [Doing::Prompt, Doing::Running("git commit -m 'fix".to_owned()), Doing::Unknown] {
            let visit = Visit { at: at(7), doing };
            assert_eq!(Visit::read(&visit.write()), Some(visit.clone()), "{visit:?}");
        }
        assert_eq!(Visit::read("7000 ran make"), None);
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
        news.watch(&busy, &agents, None, untitled, at(98));
        let (idle, agents) = session("~/w/s", "idle", 3, at(103));
        news.watch(&idle, &agents, None, untitled, at(103));
        assert_eq!(news_of(&news, &agents), (Some(ran("cargo test", 0)), true));

        // Passing through pane 2, and staying in pane 1 for a second.
        news.watch(&idle, &agents, Some(("s", 2)), untitled, at(104));
        news.watch(&idle, &agents, Some(("s", 1)), untitled, at(104));
        news.watch(&idle, &agents, Some(("s", 1)), untitled, at(105));
        assert_eq!(news_of(&news, &agents), (None, true));
        // Staying in pane 2, then on to pane 1 before another `watch`.
        news.watch(&idle, &agents, Some(("s", 2)), untitled, at(106));
        news.watch(&idle, &agents, Some(("s", 2)), untitled, at(107));
        news.watch(&idle, &agents, Some(("s", 1)), untitled, at(108));
        assert_eq!(news_of(&news, &agents), (None, false));

        let (busy, agents) = session("cargo test ~/w/s", "busy", 0, at(110));
        news.watch(&busy, &agents, None, untitled, at(110));
        let (idle, agents) = session("~/w/s", "idle", 1, at(113));
        news.watch(&idle, &agents, None, untitled, at(113));
        news.watch(&idle, &agents, None, untitled, at(120));
        assert_eq!(news_of(&news, &agents), (Some(ran("cargo test", 7)), true));
    }

    fn going(command: &str, since: Moment) -> Said {
        Said::Going { command: command.to_owned(), since, tty: "/dev/pts/3".to_owned() }
    }

    fn ended(command: &str, at: Moment, status: Option<i32>, took: u64) -> Said {
        Said::Ended { command: command.to_owned(), at, status, took: Held::from_millis(took) }
    }

    /// The mark of what the shell in pane 1 runs, or ran, and its command.
    fn command_of(news: &News, here: bool) -> Option<(Mark, String)> {
        news.command("s", 1, here).map(|(mark, command)| (mark, command.to_owned()))
    }

    /// A command the hook says is going has no mark for its first second. It
    /// is at work after that, or waits for you to type, as its terminal says,
    /// and takes the terminal over as an editor does, and then has none, as
    /// on a terminal that said nothing. Its row goes by the whole command,
    /// which the title cuts.
    #[test]
    fn a_command_its_hook_says_runs_a_while_is_at_work_or_waits_for_you() {
        let (branches, agents) = session("cargo build --relea ~/w/s", "busy", 0, at(100));
        let build = "cargo build --release";
        for (front, mark) in [
            (Some(Front::Working), Some(Mark::Running { age: Held::from_secs(2) })),
            (Some(Front::Asking), Some(Mark::Asking { age: Held::from_secs(2) })),
            (Some(Front::Interactive), None),
            (None, None),
        ] {
            let fronts = front.map(|front| ("/dev/pts/3", front)).into_iter().collect();
            let shells = Shells::of(vec![("s", 1, at(0), going(build, at(100)))], fronts);
            let mut news = News { shells, ..News::default() };
            news.watch(&branches, &agents, None, untitled, Moment::from_millis(100_500));
            assert_eq!(command_of(&news, false), None, "{front:?}");
            news.watch(&branches, &agents, Some(("s", 1)), untitled, at(102));
            let ran = command_of(&news, true);
            assert_eq!(ran, mark.map(|mark| (mark, build.to_owned())), "{front:?}");
        }
    }

    /// A command that ran for a second or more has `✓` once it ends with 0,
    /// and `✗` with anything else, or `$` where its shell could not say, until
    /// you stay in its pane. One that took less has none, nor does a job stopped with `Ctrl z`, nor what a shell
    /// says that started before its session was made, however new: it is a
    /// shell of a session renamed since, which had the name.
    #[test]
    fn a_command_that_ran_a_while_says_how_it_ended_until_you_stay_in_its_pane() {
        let (branches, agents) = session("~/w/s", "busy", 0, at(110));
        let age = Held::from_secs(7);
        let ran_as = |outcome| Some(Mark::Ran { age, outcome });
        for (said, mark) in [
            (ended("make", at(103), Some(0), 3_000), ran_as(Outcome::Passed)),
            (ended("make", at(103), Some(2), 3_000), ran_as(Outcome::Failed)),
            (ended("make", at(103), None, 3_000), ran_as(Outcome::Untold)),
            (ended("make", at(103), Some(0), 999), None),
            (Said::Stopped, None),
        ] {
            let shells = Shells::of(vec![("s", 1, at(0), said.clone())], vec![]);
            let mut news = News { shells, ..News::default() };
            news.watch(&branches, &agents, Some(("s", 2)), untitled, at(110));
            let ran = mark.clone().map(|mark| (mark, "make".to_owned()));
            assert_eq!(command_of(&news, false), ran, "{said:?}");
            assert_eq!(command_of(&news, true), None, "{said:?}");
            news.watch(&branches, &agents, Some(("s", 1)), untitled, at(111));
            news.watch(&branches, &agents, Some(("s", 1)), untitled, at(112));
            assert_eq!(command_of(&news, false), None, "{said:?}");
        }

        let mut later = branches;
        later[0].age = Age::from_secs(10);
        let passed = Some(Mark::Ran { age, outcome: Outcome::Passed });
        for (started, mark) in [(at(50), None), (at(100), passed)] {
            let said = ended("make", at(103), Some(0), 3_000);
            let shells = Shells::of(vec![("s", 1, started, said)], vec![]);
            let mut news = News { shells, ..News::default() };
            news.watch(&later, &agents, None, untitled, at(110));
            let ran = mark.map(|mark| (mark, "make".to_owned()));
            assert_eq!(command_of(&news, false), ran, "{started:?}");
        }
    }

    /// A shell with no hook says by its title that a command ended, and not
    /// how.
    #[test]
    fn a_shell_without_the_hook_says_a_command_ended_but_not_how() {
        let mut news = News::default();
        let (running, agents) = session("make ~/w/s", "busy", 0, at(100));
        news.watch(&running, &agents, None, untitled, at(100));
        let (prompt, agents) = session("~/w/s", "busy", 0, at(104));
        news.watch(&prompt, &agents, None, untitled, at(104));
        news.watch(&prompt, &agents, None, untitled, at(106));
        let age = Held::from_secs(2);
        let ran = Mark::Ran { age, outcome: Outcome::Untold };
        assert_eq!(command_of(&news, false), Some((ran, "make".to_owned())));
    }

    /// The pane you stay in takes no visit while nothing in it is new, so
    /// that what finishes after you leave it is.
    #[test]
    fn a_command_that_ends_after_you_leave_its_pane_is_new() {
        let mut news = News::default();
        let (running, agents) = session("make ~/w/s", "busy", 0, at(100));
        for now in 100..110 {
            news.watch(&running, &agents, Some(("s", 1)), untitled, at(now));
        }
        news.watch(&running, &agents, Some(("s", 2)), untitled, at(110));
        let (prompt, agents) = session("~/w/s", "busy", 0, at(111));
        news.watch(&prompt, &agents, Some(("s", 2)), untitled, at(111));
        assert_eq!(news_of(&news, &agents), (Some(ran("make", 0)), false));
    }

    /// Each session's sidebar dates an agent's finish from its own answer of
    /// claude-ps, in whole seconds from a whole second. You watch the agent
    /// in pane 2 finish at 100.1s: this sidebar hears at 100.35s, and writes
    /// your visit at 100.5s; another hears at 401s, and dates it at 102s.
    /// Both have seen it. One that finishes once you have gone is new.
    #[test]
    fn an_agent_you_saw_finish_is_seen_however_a_sidebar_dates_it() {
        let ms = Moment::from_millis;
        let mut news = News::default();
        let (busy, agents) = session("~/w/s", "busy", 30, ms(98_000));
        news.watch(&busy, &agents, Some(("s", 2)), untitled, ms(98_000));
        news.watch(&busy, &agents, Some(("s", 2)), untitled, ms(99_000));
        let (idle, here) = session("~/w/s", "idle", 0, ms(100_350));
        news.watch(&idle, &here, Some(("s", 2)), untitled, ms(100_500));
        let (_, there) = session("~/w/s", "idle", 299, ms(401_000));
        assert!(!news_of(&news, &here).1 && !news_of(&news, &there).1);

        let (_, later) = session("~/w/s", "idle", 0, ms(104_000));
        assert!(news_of(&news, &later).1);
    }

    /// Watches session `s` at `now`, the shell in pane 1 titled `listed` in
    /// the session list and `own` by itself, and you in pane `here`.
    fn shell(news: &mut News, listed: &str, own: &str, here: Option<u32>, now: u64) {
        let (branches, agents) = session(listed, "busy", 0, at(now));
        let own = |pane| (pane == 1).then(|| own.to_owned());
        news.watch(&branches, &agents, here.map(|pane| ("s", pane)), own, at(now));
    }

    /// The session list shows a shell back at its prompt seconds after the
    /// title the pane gives itself does. What you saw end in pane 1 is seen
    /// once the list says it ended: whether you stayed, left the pane or the
    /// session before another `watch`, or saw it run and end before the list
    /// showed it at all. What ends once you have left is new.
    #[test]
    fn a_command_you_saw_end_is_seen_once_the_session_list_says_so() {
        let (going, back) = ("make ~/w/s", "~/w/s");
        // `make` ends at 100 by its own title, and at 104 by the list's. You
        // stay to 101, or leave as it ends, for pane 2 or another session.
        for (then, left) in [(Some(2), 102), (Some(2), 100), (None, 100)] {
            let mut news = News::default();
            for now in 90..100 {
                shell(&mut news, going, going, Some(1), now);
            }
            for now in 100..left {
                shell(&mut news, going, back, Some(1), now);
            }
            for now in left..104 {
                shell(&mut news, going, back, then, now);
            }
            shell(&mut news, back, back, then, 104);
            assert_eq!(news.ran("s", 1), None, "{then:?} at {left}");
        }

        // `make` runs from 90 to 91 by its own title, and you leave at 92.
        // The list shows it from 94 to 96.
        let mut news = News::default();
        shell(&mut news, back, back, Some(1), 89);
        shell(&mut news, back, going, Some(1), 90);
        shell(&mut news, back, back, Some(1), 91);
        for now in 92..94 {
            shell(&mut news, back, back, Some(2), now);
        }
        shell(&mut news, going, back, Some(2), 94);
        shell(&mut news, back, back, Some(2), 96);
        assert_eq!(news.ran("s", 1), None);

        // You leave at 100 while `make` runs, and it ends at 102.
        let mut news = News::default();
        for now in 90..100 {
            shell(&mut news, going, going, Some(1), now);
        }
        shell(&mut news, going, going, Some(2), 100);
        for now in 102..106 {
            shell(&mut news, going, back, Some(2), now);
        }
        shell(&mut news, back, back, Some(2), 106);
        assert_eq!(news.ran("s", 1).map(|(command, _)| command), Some("make"));
    }

    /// A pane its session leaves out of `GONE` lists in a row has closed,
    /// and what was kept of it goes. One left out of fewer keeps it.
    #[test]
    fn a_closed_pane_s_records_go_once_it_stays_out_of_the_list() {
        let mut news = News::default();
        let (running, agents) = session("make ~/w/s", "idle", 1, at(100));
        news.watch(&running, &agents, Some(("s", 2)), untitled, at(100));
        let (prompt, agents) = session("~/w/s", "idle", 2, at(101));
        news.watch(&prompt, &agents, Some(("s", 2)), untitled, at(101));
        let kept = |news: &News| (news.runs.get("s", 1).is_some(), news.visits.get("s", 2).is_some());
        assert_eq!(kept(&news), (true, true));

        let (mut closed, _) = session("~/w/s", "idle", 2, at(101));
        closed[0].panes.clear();
        for now in 102..101 + u64::from(GONE) {
            news.watch(&closed, &agents, None, untitled, at(now));
        }
        news.watch(&prompt, &agents, None, untitled, at(110));
        for now in 111..110 + u64::from(GONE) {
            news.watch(&closed, &agents, None, untitled, at(now));
        }
        assert_eq!(kept(&news), (true, true));
        news.watch(&closed, &agents, None, untitled, at(120));
        assert_eq!(kept(&news), (false, false));
    }

    /// A session made again by the name of one gone, before any sidebar saw
    /// the one go, has none of what that one's shells ran: its pane 1 is
    /// another pane.
    #[test]
    fn a_session_made_again_by_its_name_has_none_of_what_the_last_one_ran() {
        let mut news = News::default();
        let (running, agents) = session("make ~/w/s", "busy", 0, at(100));
        news.watch(&running, &agents, None, untitled, at(100));
        let (prompt, agents) = session("~/w/s", "busy", 0, at(101));
        news.watch(&prompt, &agents, None, untitled, at(101));
        assert_eq!(news_of(&news, &agents).0, Some(ran("make", 0)));

        let (mut again, agents) = session("~/w/s", "busy", 0, at(500));
        again[0].age = Age::from_secs(10);
        news.watch(&again, &agents, None, untitled, at(500));
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
            news.watch(&branches, &agents, here.map(|pane| (name, pane)), untitled, at(now));
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
        news.watch(&idle, &agents, Some(("s", 2)), untitled, at(10));
        news.watch(&idle, &agents, Some(("s", 2)), untitled, at(11));
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
