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

/// The most jobs stopped that a shell is followed by, the oldest let go of
/// past them.
const STOPPED_MAX: usize = 64;

/// A pane's shell, as its marks say it.
#[derive(Default)]
pub struct Shell {
    ran: Option<Ran>,
    /// The jobs it stopped, by their groups, with their commands, the last
    /// stopped the last.
    stopped: Vec<(u32, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ran {
    Ready,
    Going(Going),
    Ended { command: String, at: SystemTime, ending: Option<Ending>, took: Duration },
    Stopped { command: String, at: SystemTime },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Going {
    command: String,
    since: SystemTime,
    started: Instant,
    /// Whether the line typed was `fg`, which says nothing of how the job
    /// it brought back ends.
    resumes: bool,
    front: Front,
    /// The group of the shell's job that the server last saw in front.
    seen: Option<u32>,
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
    pub fn marked(&mut self, mark: Mark, now: Now, job: impl Fn(u32) -> Option<Job>) {
        match mark {
            Mark::Prompt => {
                if self.ran.is_none() {
                    self.ran = Some(Ran::Ready);
                }
            },
            Mark::Command(line) => {
                let mut command = line.unwrap_or_default().replace('\n', " ");
                if execs(&command) {
                    self.ran = None;
                    return;
                }
                let resumes = is_fg(&command);
                if command.trim() == "fg" {
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
                if !self.going() {
                    return;
                }
                let Some(Ran::Going(going)) = self.ran.take() else {
                    unreachable!("a command runs, as `going` said");
                };
                if let Some(group) = going.seen.filter(|group| job(*group) == Some(Job::Stopped)) {
                    let was = self.stopped.iter().position(|(stopped, _)| *stopped == group);
                    let command = match was {
                        Some(place) => self.stopped.remove(place).1,
                        None => going.command,
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
    pub fn saw(&mut self, group: u32, program: u32, front: Front) {
        if let Some(Ran::Going(going)) = &mut self.ran {
            going.front = front;
            if group != program {
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

    fn ended(command: &str, at: u64, ending: Option<Ending>, took: u64) -> Ran {
        let (command, at) = (command.to_owned(), SystemTime::UNIX_EPOCH + Duration::from_secs(at));
        Ran::Ended { command, at, ending, took: Duration::from_secs(took) }
    }

    const NONE_STOPPED: fn(u32) -> Option<Job> = |_| None;

    /// The job of each group, stopped where `stopped` says.
    fn stopped_if(stopped: fn(u32) -> bool) -> impl Fn(u32) -> Option<Job> {
        move |group| Some(if stopped(group) { Job::Stopped } else { Job::Runs })
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
        assert!(shell.going());
        let Some(Ran::Going(going)) = &shell.ran else { unreachable!() };
        assert_eq!(going.command, "make test");
        shell.saw(40, 10, Front::Asking);
        let said = shell.said().and_then(|said| said.is);
        let Some(api::ran::Is::Going(going)) = said else { panic!("{said:?}") };
        assert_eq!(going.front(), api::Front::Asking);
        shell.marked(Mark::Done(Some(2)), now(5), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("make test", 5, Some(Ending::Status(2)), 3)));
        shell.marked(Mark::Prompt, now(6), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("make test", 5, Some(Ending::Status(2)), 3)));
        shell.marked(Mark::Done(Some(0)), now(7), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("make test", 5, Some(Ending::Status(2)), 3)));
    }

    /// A prompt while a command runs is the command's own, as fish's `read`
    /// draws one, and the command runs on to the mark of its end; and one
    /// whose shell said no status says none.
    #[test]
    fn a_prompt_while_a_command_runs_is_its_own() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(command("read -P 'name? ' x"), now(1), NONE_STOPPED);
        shell.marked(Mark::Prompt, now(2), NONE_STOPPED);
        assert!(shell.going());
        shell.marked(Mark::Done(Some(0)), now(3), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("read -P 'name? ' x", 3, Some(Ending::Status(0)), 2)));
        shell.marked(Mark::Command(None), now(4), NONE_STOPPED);
        shell.marked(Mark::Done(None), now(5), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("", 5, None, 1)));
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
        shell.saw(10, 10, Front::Working);
        shell.saw(40, 10, Front::Working);
        shell.saw(10, 10, Front::Working);
        shell.marked(Mark::Done(Some(0)), now(3), &stopped);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(3);
        assert_eq!(shell.ran, Some(Ran::Stopped { command: "sleep 30".to_owned(), at }));
        shell.marked(command("fg"), now(4), stopped_if(|_| false));
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!(going.command, "sleep 30");
        shell.marked(Mark::Done(Some(0)), now(6), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("sleep 30", 6, Some(Ending::Resumed), 2)));
        shell.marked(command("fg"), now(7), NONE_STOPPED);
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!(going.command, "fg");
    }

    /// A job that ended, though seen in front, ended; and `fg` skips a
    /// stopped job that is gone since, and names one with `fg %1` as typed.
    #[test]
    fn fg_brings_back_the_last_job_that_still_stands_stopped() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        for (group, line) in [(40, "vim a"), (41, "vim b")] {
            shell.marked(command(line), now(1), NONE_STOPPED);
            shell.saw(group, 10, Front::Interactive);
            shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        }
        shell.marked(command("fg %1"), now(3), stopped_if(|_| true));
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!((going.command.as_str(), going.resumes), ("fg %1", true));
        shell.marked(Mark::Done(Some(0)), now(4), NONE_STOPPED);
        shell.marked(command("fg"), now(5), |group| (group == 40).then_some(Job::Runs));
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!(going.command, "vim a");
        shell.marked(command("true"), now(6), NONE_STOPPED);
        shell.saw(42, 10, Front::Working);
        shell.marked(Mark::Done(Some(0)), now(7), NONE_STOPPED);
        assert_eq!(shell.ran, Some(ended("true", 7, Some(Ending::Status(0)), 1)));
    }

    /// A job `fg %1` brought back and stopped again stands stopped once, by
    /// the name it had, and `fg` brings it back by that name.
    #[test]
    fn a_job_stopped_again_stands_stopped_once_by_its_name() {
        let now = shell_at(Instant::now());
        let mut shell = Shell::default();
        shell.marked(command("vim a"), now(1), NONE_STOPPED);
        shell.saw(40, 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(2), stopped_if(|_| true));
        shell.marked(command("fg %1"), now(3), stopped_if(|_| true));
        shell.saw(40, 10, Front::Interactive);
        shell.marked(Mark::Done(Some(0)), now(4), stopped_if(|_| true));
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(4);
        assert_eq!(shell.ran, Some(Ran::Stopped { command: "vim a".to_owned(), at }));
        assert_eq!(shell.stopped, [(40, "vim a".to_owned())]);
        shell.marked(command("fg"), now(5), stopped_if(|_| true));
        let Some(Ran::Going(going)) = &shell.ran else { panic!("{:?}", shell.ran) };
        assert_eq!(going.command, "vim a");
        assert_eq!(shell.stopped, []);
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
