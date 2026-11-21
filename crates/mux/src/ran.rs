use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use luneta_protocol::shell::{Ending, Front};

use crate::api;
use crate::front;
use crate::marks::Mark;
use crate::process::Group;

const STOPPED_MAX: usize = 64;

#[derive(Default)]
pub struct Shell {
    ran: Option<Ran>,
    stopped: Vec<(Group, Option<Said>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ran {
    Ready,
    Going(Going),
    Ended { command: Option<Said>, at: SystemTime, ending: Option<Ending>, took: Duration },
    Stopped { command: Option<Said>, at: SystemTime },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Going {
    command: Option<Said>,
    since: SystemTime,
    started: Instant,
    resumes: bool,
    front: Front,
    seen: Option<Group>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    Runs,
    Stopped,
}

#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub at: SystemTime,
    pub instant: Instant,
}

impl Now {
    #[must_use]
    pub fn current() -> Now {
        Now { at: SystemTime::now(), instant: Instant::now() }
    }
}

impl Shell {
    pub fn read(
        &mut self,
        marks: Vec<Mark>,
        now: Now,
        in_front: bool,
        job: impl Fn(Group) -> Option<Job>,
    ) {
        let last_command = marks.iter().rposition(|mark| matches!(mark, Mark::Command(_)));
        for (at, mark) in marks.into_iter().enumerate() {
            let typed_ahead = last_command.is_some_and(|last| at < last);
            if matches!(mark, Mark::Command(_)) || in_front || typed_ahead {
                self.marked(mark, now, &job);
            }
        }
    }

    fn marked(&mut self, mark: Mark, now: Now, job: impl Fn(Group) -> Option<Job>) {
        match mark {
            Mark::Prompt => {
                if self.ran.is_none() {
                    self.ran = Some(Ran::Ready);
                }
            },
            Mark::Command(line) => {
                if self.ran.is_none() || self.going() {
                    return;
                }
                let mut command = line.and_then(|line| Said::new(line.replace('\n', " ")));
                let typed = command.as_ref().map(Said::as_str);
                if typed.is_some_and(execs) {
                    self.ran = None;
                    return;
                }
                let resumes = typed.is_some_and(is_fg);
                if typed.map(str::trim) == Some("fg") {
                    self.stopped.retain(|(group, _)| job(*group).is_some());
                    if let Some((_, job)) = self.stopped.pop() {
                        command = job;
                    }
                }
                let (since, started) = (now.at, now.instant);
                let front = Front::Working;
                let going = Going { command, since, started, resumes, front, seen: None };
                self.ran = Some(Ran::Going(going));
            },
            Mark::Done(status) => {
                let Some(Ran::Going(going)) = self.ran.take_if(|ran| matches!(ran, Ran::Going(_)))
                else {
                    return;
                };
                if let Some(group) = going.seen.filter(|group| job(*group) == Some(Job::Stopped)) {
                    let was = self.stopped.iter().position(|(stopped, _)| *stopped == group);
                    let named = was.map(|place| self.stopped.remove(place).1);
                    let command = match named {
                        Some(named) if going.resumes => named,
                        _ => going.command,
                    };
                    if self.stopped.len() == STOPPED_MAX {
                        self.stopped.remove(0);
                    }
                    assert!(!self.stopped.iter().any(|(stopped, _)| *stopped == group));
                    self.stopped.push((group, command.clone()));
                    assert!(self.stopped.len() <= STOPPED_MAX);
                    self.ran = Some(Ran::Stopped { command, at: now.at });
                } else {
                    let ending = if going.resumes {
                        Some(Ending::Resumed)
                    } else {
                        status.map(Ending::Exited)
                    };
                    self.ran = Some(going.ended(ending, now));
                }
            },
        }
    }

    #[must_use]
    pub fn going(&self) -> bool {
        matches!(self.ran, Some(Ran::Going(_)))
    }

    pub fn saw(&mut self, group: Group, program: u32, front: Front) {
        if let Some(Ran::Going(going)) = &mut self.ran {
            going.front = front;
            if group.leader() != program {
                going.seen = Some(group);
            }
        }
    }

    pub fn gone(&mut self) {
        self.ran = None;
        self.stopped.clear();
    }

    #[must_use]
    pub fn said(&self) -> Option<api::Ran> {
        use api::ran::{Ended, Going, Is, Ready, Resumed, Stopped};
        let is = match self.ran.as_ref()? {
            Ran::Ready => Is::Ready(Ready {}),
            Ran::Going(going) => Is::Going(Going {
                command: told(going.command.as_ref()),
                since: Some(going.since.into()),
                front: front::said(going.front).into(),
            }),
            Ran::Ended { command, at, ending, took } => Is::Ended(Ended {
                command: told(command.as_ref()),
                at: Some((*at).into()),
                ending: ending.map(|ending| match ending {
                    Ending::Exited(status) => api::ran::ended::Ending::Status(status),
                    Ending::Resumed => api::ran::ended::Ending::Resumed(Resumed {}),
                }),
                took: Some((*took).try_into().expect("a command ran for under 2^63 seconds")),
            }),
            Ran::Stopped { command, at } => {
                Is::Stopped(Stopped { command: told(command.as_ref()), at: Some((*at).into()) })
            },
        };
        Some(api::Ran { is: Some(is) })
    }
}

impl Going {
    fn ended(self, ending: Option<Ending>, now: Now) -> Ran {
        let took = now.instant.saturating_duration_since(self.started);
        Ran::Ended { command: self.command, at: now.at, ending, took }
    }
}

fn told(command: Option<&Said>) -> Option<String> {
    command.map(|command| command.as_str().to_owned())
}

fn execs(line: &str) -> bool {
    line.split([';', '&', '|']).any(|command| {
        let joining = |word: &&str| matches!(*word, "and" | "or" | "not");
        let mut words = command.split_whitespace().skip_while(joining);
        words.next() == Some("exec")
    })
}

fn is_fg(line: &str) -> bool {
    line.split_whitespace().next() == Some("fg")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> Now {
        let base = Instant::now();
        Now {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            instant: base + Duration::from_secs(secs),
        }
    }

    fn shell_at(base: Instant) -> impl Fn(u64) -> Now {
        move |secs| Now {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            instant: base + Duration::from_secs(secs),
        }
    }

    fn command(line: &str) -> Mark {
        Mark::Command(Some(line.to_owned()))
    }

    fn said(command: &str) -> Option<Said> {
        Said::new(command.to_owned())
    }

    fn ended(command: Option<&str>, at: u64, ending: Option<Ending>, took: u64) -> Ran {
        let command = command.and_then(said);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(at);
        Ran::Ended { command, at, ending, took: Duration::from_secs(took) }
    }

    fn stopped_at(command: &str, at: u64) -> Ran {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(at);
        Ran::Stopped { command: said(command), at }
    }

    fn going(shell: &Shell) -> Option<&str> {
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        going.command.as_ref().map(Said::as_str)
    }

    const NONE_STOPPED: fn(Group) -> Option<Job> = |_| None;

    fn stopped_if(stopped: fn(u32) -> bool) -> impl Fn(Group) -> Option<Job> {
        move |group| Some(if stopped(group.leader()) { Job::Stopped } else { Job::Runs })
    }

    #[test]
    fn a_command_goes_from_its_mark_and_ends_at_its_own() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        assert_eq!(shell.said(), None);
        shell.marked(Mark::Prompt, now(1), NONE_STOPPED);
        assert_eq!(shell.ran, Some(Ran::Ready));
        shell.marked(command("make\ntest"), now(2), NONE_STOPPED);
        assert_eq!(going(&shell), Some("make test"));
        shell.saw(Group(40), 10, Front::Asking);
        let said = shell.said().and_then(|said| said.is);
        let Some(api::ran::Is::Going(going)) = said else { panic!("{said:?}") };
        assert_eq!(going.front(), api::Front::Asking);
        shell.marked(Mark::Done(Some(2)), now(5), NONE_STOPPED);
        let make = ended(Some("make test"), 5, Some(Ending::Exited(2)), 3);
        assert_eq!(shell.ran, Some(make.clone()));
        shell.marked(Mark::Prompt, now(6), NONE_STOPPED);
        assert_eq!(shell.ran, Some(make.clone()));
        shell.marked(Mark::Done(Some(0)), now(7), NONE_STOPPED);
        assert_eq!(shell.ran, Some(make));
    }

    #[test]
    fn a_prompt_while_a_command_runs_is_its_own() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(Mark::Prompt, now(0), NONE_STOPPED);
        shell.marked(command("read -P 'name? ' x"), now(1), NONE_STOPPED);
        shell.marked(Mark::Prompt, now(2), NONE_STOPPED);
        assert!(shell.going());
        shell.marked(Mark::Done(Some(0)), now(3), NONE_STOPPED);
        let read = ended(Some("read -P 'name? ' x"), 3, Some(Ending::Exited(0)), 2);
        assert_eq!(shell.ran, Some(read));
        shell.marked(Mark::Command(None), now(4), NONE_STOPPED);
        assert_eq!(going(&shell), None);
        shell.marked(Mark::Done(None), now(5), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(None, 5, None, 1)));
        shell.marked(command(""), now(6), NONE_STOPPED);
        assert_eq!(going(&shell), None);
    }

    #[test]
    fn exec_leaves_the_shell_saying_nothing() {
        for line in ["exec fish", "  exec", "cd /; exec nvim", "true && exec x", "not exec x"] {
            let mut shell = Shell::default();
            shell.marked(Mark::Prompt, at(1), NONE_STOPPED);
            shell.marked(command(line), at(2), NONE_STOPPED);
            assert_eq!(shell.said(), None, "{line}");
        }
        for line in ["execute", "echo exec", "nexec x"] {
            let mut shell = Shell::default();
            shell.marked(Mark::Prompt, at(1), NONE_STOPPED);
            shell.marked(command(line), at(2), NONE_STOPPED);
            assert!(shell.going(), "{line}");
        }
    }

    #[test]
    fn a_job_stopped_is_stopped_and_fg_brings_it_back() {
        let now = shell_at(Instant::now());
        let stopped = stopped_if(|group| group == 40);
        let mut shell = Shell::default();
        shell.marked(Mark::Prompt, now(0), NONE_STOPPED);
        shell.marked(command("sleep 30"), now(1), NONE_STOPPED);
        shell.saw(Group(10), 10, Front::Working);
        shell.saw(Group(40), 10, Front::Working);
        shell.saw(Group(10), 10, Front::Working);
        shell.marked(Mark::Done(Some(0)), now(3), &stopped);
        assert_eq!(shell.ran, Some(stopped_at("sleep 30", 3)));
        shell.marked(command("fg"), now(4), stopped_if(|_| false));
        assert_eq!(going(&shell), Some("sleep 30"));
        shell.marked(Mark::Done(Some(0)), now(6), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(Some("sleep 30"), 6, Some(Ending::Resumed), 2)));
        shell.marked(command("fg"), now(7), NONE_STOPPED);
        assert_eq!(going(&shell), Some("fg"));
    }

    #[test]
    fn fg_brings_back_the_last_job_that_still_stands_stopped() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(Mark::Prompt, now(0), NONE_STOPPED);
        for (group, line) in [(40, "vim a"), (41, "vim b")] {
            shell.marked(command(line), now(1), NONE_STOPPED);
            shell.saw(Group(group), 10, Front::Interactive);
            shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        }
        shell.marked(command("fg %1"), now(3), stopped_if(|_| true));
        let Some(Ran::Going(fg)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!((fg.command.as_ref().map(Said::as_str), fg.resumes), (Some("fg %1"), true));
        shell.marked(Mark::Done(Some(0)), now(4), NONE_STOPPED);
        shell.marked(command("fg"), now(5), |group| (group == Group(40)).then_some(Job::Runs));
        assert_eq!(going(&shell), Some("vim a"));
        shell.marked(Mark::Done(Some(0)), now(6), NONE_STOPPED);
        shell.marked(command("true"), now(6), NONE_STOPPED);
        shell.saw(Group(42), 10, Front::Working);
        shell.marked(Mark::Done(Some(0)), now(7), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(Some("true"), 7, Some(Ending::Exited(0)), 1)));
    }

    #[test]
    fn a_job_stopped_again_stands_stopped_once_by_its_name() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(Mark::Prompt, now(0), NONE_STOPPED);
        shell.marked(command("vim a"), now(1), NONE_STOPPED);
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        shell.marked(command("fg %1"), now(3), stopped_if(|_| true));
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(4), stopped_if(|_| true));
        assert_eq!(shell.ran, Some(stopped_at("vim a", 4)));
        assert_eq!(shell.stopped, [(Group(40), said("vim a"))]);
        shell.marked(command("fg"), now(5), stopped_if(|_| true));
        assert_eq!(going(&shell), Some("vim a"));
        assert_eq!(shell.stopped, []);
    }

    #[test]
    fn a_job_in_a_group_of_one_gone_stands_stopped_by_its_own_name() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(Mark::Prompt, now(0), NONE_STOPPED);
        shell.marked(command("vim a"), now(1), NONE_STOPPED);
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        shell.marked(command("less x"), now(3), stopped_if(|_| true));
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(4), stopped_if(|_| true));
        assert_eq!(shell.ran, Some(stopped_at("less x", 4)));
        assert_eq!(shell.stopped, [(Group(40), said("less x"))]);
    }

    #[test]
    fn the_marks_of_what_a_command_runs_are_none_of_the_shell_s() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.read(vec![Mark::Prompt, command("ssh host")], now(1), true, NONE_STOPPED);
        let remote = vec![Mark::Prompt, command("ls"), Mark::Done(Some(0)), Mark::Prompt];
        shell.read(remote, now(2), false, NONE_STOPPED);
        assert_eq!(going(&shell), Some("ssh host"));
        shell.read(vec![Mark::Done(Some(255)), Mark::Prompt], now(4), true, NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(Some("ssh host"), 4, Some(Ending::Exited(255)), 3)));
    }

    #[test]
    fn a_shell_of_no_marks_says_nothing_of_another_s() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        for mark in [Mark::Prompt, command("make"), Mark::Done(Some(2)), Mark::Prompt] {
            shell.read(vec![mark], now(1), false, NONE_STOPPED);
        }
        assert_eq!(shell.said(), None);
        shell.read(vec![command("make")], now(2), true, NONE_STOPPED);
        assert_eq!(shell.said(), None);
    }

    #[test]
    fn a_line_typed_ahead_of_the_first_prompt_runs() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.read(vec![Mark::Prompt, command("claude")], now(1), false, NONE_STOPPED);
        assert_eq!(going(&shell), Some("claude"));
    }

    #[test]
    fn a_line_typed_ahead_ends_the_last_and_runs() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.read(vec![Mark::Prompt, command("make")], now(1), true, NONE_STOPPED);
        let ahead = vec![Mark::Done(Some(0)), Mark::Prompt, command("vim")];
        shell.read(ahead, now(3), false, NONE_STOPPED);
        assert_eq!(going(&shell), Some("vim"));
        shell.read(vec![Mark::Done(Some(1))], now(4), false, NONE_STOPPED);
        assert_eq!(going(&shell), Some("vim"));
        shell.read(vec![Mark::Done(Some(0))], now(5), true, NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(Some("vim"), 5, Some(Ending::Exited(0)), 2)));
    }

    #[test]
    fn a_shell_gone_says_nothing() {
        let mut shell = Shell::default();
        shell.marked(Mark::Prompt, at(0), NONE_STOPPED);
        shell.marked(command("ls"), at(1), NONE_STOPPED);
        shell.gone();
        assert_eq!(shell.said(), None);
    }
}
