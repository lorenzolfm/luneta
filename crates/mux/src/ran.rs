//! What a pane's shell says it runs, by its marks, and what the server saw
//! of it: as luneta's fish hook (`shell/luneta.fish`) says it of a zellij
//! pane's shell, by the same rules.
//!
//! - A shell is ready from its first prompt until it runs a command.
//! - Only the mark of a command's end ends it: a prompt while it runs is its
//!   own, as fish's `read` draws one with the shell's line editor.
//! - `exec` gives the pane to another program, and nothing would say how
//!   what it runs ends: the shell says nothing from there on.
//! - A shell says a job it stopped with `Ctrl z` ended, with a status of 0:
//!   the job whose group the server last saw in front, and stands stopped as
//!   the shell says it ended, stopped.
//! - `fg` brings the last job back, which is what runs. Its shell cannot say
//!   how that ends, as the status it says is `fg`'s.

use std::time::{Duration, Instant, SystemTime};

use crate::api;
use crate::front::Front;
use crate::marks::Mark;
use crate::process::Group;

/// The most jobs stopped that a shell is followed by, the oldest let go of
/// past them.
const STOPPED_MAX: usize = 64;

/// A pane's shell, as its marks say it.
#[derive(Default)]
pub struct Shell {
    ran: Option<Ran>,
    /// The jobs it stopped, by their groups, with their commands, the last
    /// stopped the last.
    stopped: Vec<(Group, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ran {
    Ready,
    Going(Going),
    Ended { command: Option<String>, at: SystemTime, ending: Option<Ending>, took: Duration },
    Stopped { command: Option<String>, at: SystemTime },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Going {
    /// The command line, on one line, where the shell said one.
    command: Option<String>,
    since: SystemTime,
    started: Instant,
    /// Whether the line typed was `fg`, which says nothing of how the job
    /// it brought back ends.
    resumes: bool,
    front: Front,
    /// The group of the shell's job that the server last saw in front.
    seen: Option<Group>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    Status(i32),
    Resumed,
}

/// A process group's leader, as the kernel says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    Runs,
    Stopped,
}

/// When something was said: by the clock, for what it says, and by the
/// instant, for how long it took.
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
    /// Takes in `mark`, which the shell wrote at `now`, with what a process
    /// group's leader is: stopped or not, or else gone.
    pub fn marked(&mut self, mark: Mark, now: Now, job: impl Fn(Group) -> Option<Job>) {
        match mark {
            Mark::Prompt => {
                if self.ran.is_none() {
                    self.ran = Some(Ran::Ready);
                }
            },
            Mark::Command(line) => {
                let mut command =
                    line.map(|line| line.replace('\n', " ")).filter(|line| !line.is_empty());
                let typed = command.as_deref();
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
                        status.map(Ending::Status)
                    };
                    self.ran = Some(going.ended(ending, now));
                }
            },
        }
    }

    /// Whether a command runs.
    #[must_use]
    pub fn going(&self) -> bool {
        matches!(self.ran, Some(Ran::Going(_)))
    }

    /// Takes in what the server saw in front of the pane's terminal while a
    /// command runs: the group `group`, of which `program`'s is the shell's,
    /// and what that does.
    pub fn saw(&mut self, group: Group, program: u32, front: Front) {
        if let Some(Ran::Going(going)) = &mut self.ran {
            going.front = front;
            if group.leader() != program {
                going.seen = Some(group);
            }
        }
    }

    /// The shell's program ended: it says no more.
    pub fn gone(&mut self) {
        self.ran = None;
        self.stopped.clear();
    }

    /// What the shell said, as the protocol says it.
    #[must_use]
    pub fn said(&self) -> Option<api::Ran> {
        use api::ran::{Ended, Going, Is, Ready, Resumed, Stopped};
        let is = match self.ran.as_ref()? {
            Ran::Ready => Is::Ready(Ready {}),
            Ran::Going(going) => Is::Going(Going {
                command: going.command.clone(),
                since: Some(going.since.into()),
                front: going.front.said().into(),
            }),
            Ran::Ended { command, at, ending, took } => Is::Ended(Ended {
                command: command.clone(),
                at: Some((*at).into()),
                ending: ending.map(|ending| match ending {
                    Ending::Status(status) => api::ran::ended::Ending::Status(status),
                    Ending::Resumed => api::ran::ended::Ending::Resumed(Resumed {}),
                }),
                took: Some((*took).try_into().expect("a command ran for under 2^63 seconds")),
            }),
            Ran::Stopped { command, at } => {
                Is::Stopped(Stopped { command: command.clone(), at: Some((*at).into()) })
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

/// Whether `line` runs `exec`: as a command of its own, or after `;`, `&`,
/// `|`, `and`, `or` or `not`.
fn execs(line: &str) -> bool {
    line.split([';', '&', '|']).any(|command| {
        let joining = |word: &&str| matches!(*word, "and" | "or" | "not");
        let mut words = command.split_whitespace().skip_while(joining);
        words.next() == Some("exec")
    })
}

/// Whether `line` is `fg`, with or without a job named.
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

    fn ended(command: Option<&str>, at: u64, ending: Option<Ending>, took: u64) -> Ran {
        let command = command.map(str::to_owned);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(at);
        Ran::Ended { command, at, ending, took: Duration::from_secs(took) }
    }

    fn stopped_at(command: &str, at: u64) -> Ran {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(at);
        Ran::Stopped { command: Some(command.to_owned()), at }
    }

    /// The command line of the command going in `shell`.
    fn going(shell: &Shell) -> Option<&str> {
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        going.command.as_deref()
    }

    const NONE_STOPPED: fn(Group) -> Option<Job> = |_| None;

    /// The job of each group, stopped where `stopped` says.
    fn stopped_if(stopped: fn(u32) -> bool) -> impl Fn(Group) -> Option<Job> {
        move |group| Some(if stopped(group.leader()) { Job::Stopped } else { Job::Runs })
    }

    /// Ready at its first prompt, a command runs from its mark, on one line,
    /// and ends with its status at its own; a prompt after keeps what it ran.
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
        let make = ended(Some("make test"), 5, Some(Ending::Status(2)), 3);
        assert_eq!(shell.ran, Some(make.clone()));
        shell.marked(Mark::Prompt, now(6), NONE_STOPPED);
        assert_eq!(shell.ran, Some(make.clone()));
        shell.marked(Mark::Done(Some(0)), now(7), NONE_STOPPED);
        assert_eq!(shell.ran, Some(make));
    }

    /// A prompt while a command runs is the command's own, as fish's `read`
    /// draws one, and the command runs on to the mark of its end; and one
    /// whose shell said no line, or an empty one, and no status, says
    /// neither.
    #[test]
    fn a_prompt_while_a_command_runs_is_its_own() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(command("read -P 'name? ' x"), now(1), NONE_STOPPED);
        shell.marked(Mark::Prompt, now(2), NONE_STOPPED);
        assert!(shell.going());
        shell.marked(Mark::Done(Some(0)), now(3), NONE_STOPPED);
        let read = ended(Some("read -P 'name? ' x"), 3, Some(Ending::Status(0)), 2);
        assert_eq!(shell.ran, Some(read));
        shell.marked(Mark::Command(None), now(4), NONE_STOPPED);
        assert_eq!(going(&shell), None);
        shell.marked(Mark::Done(None), now(5), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(None, 5, None, 1)));
        shell.marked(command(""), now(6), NONE_STOPPED);
        assert_eq!(going(&shell), None);
    }

    /// `exec` leaves the shell saying nothing, in each place it may be typed.
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
            shell.marked(command(line), at(2), NONE_STOPPED);
            assert!(shell.going(), "{line}");
        }
    }

    /// A job seen in front that stands stopped as its shell says it ended
    /// stopped; `fg` brings it back as what runs, and its end says it was
    /// resumed, whatever status `fg` gave.
    #[test]
    fn a_job_stopped_is_stopped_and_fg_brings_it_back() {
        let now = shell_at(Instant::now());
        let stopped = stopped_if(|group| group == 40);
        let mut shell = Shell::default();
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

    /// A job that ended, though seen in front, ended; and `fg` skips a
    /// stopped job that is gone since, and names one with `fg %1` as typed.
    #[test]
    fn fg_brings_back_the_last_job_that_still_stands_stopped() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        for (group, line) in [(40, "vim a"), (41, "vim b")] {
            shell.marked(command(line), now(1), NONE_STOPPED);
            shell.saw(Group(group), 10, Front::Interactive);
            shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        }
        shell.marked(command("fg %1"), now(3), stopped_if(|_| true));
        let Some(Ran::Going(fg)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!((fg.command.as_deref(), fg.resumes), (Some("fg %1"), true));
        shell.marked(Mark::Done(Some(0)), now(4), NONE_STOPPED);
        shell.marked(command("fg"), now(5), |group| (group == Group(40)).then_some(Job::Runs));
        assert_eq!(going(&shell), Some("vim a"));
        shell.marked(command("true"), now(6), NONE_STOPPED);
        shell.saw(Group(42), 10, Front::Working);
        shell.marked(Mark::Done(Some(0)), now(7), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended(Some("true"), 7, Some(Ending::Status(0)), 1)));
    }

    /// A job `fg %1` brought back and stopped again stands stopped once, by
    /// the name it had, and `fg` brings it back by that name.
    #[test]
    fn a_job_stopped_again_stands_stopped_once_by_its_name() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(command("vim a"), now(1), NONE_STOPPED);
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        shell.marked(command("fg %1"), now(3), stopped_if(|_| true));
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(4), stopped_if(|_| true));
        assert_eq!(shell.ran, Some(stopped_at("vim a", 4)));
        assert_eq!(shell.stopped, [(Group(40), Some("vim a".to_owned()))]);
        shell.marked(command("fg"), now(5), stopped_if(|_| true));
        assert_eq!(going(&shell), Some("vim a"));
        assert_eq!(shell.stopped, []);
    }

    /// A job that leads the group of one stopped before, which ended since,
    /// stands stopped by its own name, and in that one's place: the kernel
    /// gave the group's number to it.
    #[test]
    fn a_job_in_a_group_of_one_gone_stands_stopped_by_its_own_name() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(command("vim a"), now(1), NONE_STOPPED);
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        shell.marked(command("less x"), now(3), stopped_if(|_| true));
        shell.saw(Group(40), 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(4), stopped_if(|_| true));
        assert_eq!(shell.ran, Some(stopped_at("less x", 4)));
        assert_eq!(shell.stopped, [(Group(40), Some("less x".to_owned()))]);
    }

    /// Its program gone, the shell says nothing.
    #[test]
    fn a_shell_gone_says_nothing() {
        let mut shell = Shell::default();
        shell.marked(command("ls"), at(1), NONE_STOPPED);
        shell.gone();
        assert_eq!(shell.said(), None);
    }
}
