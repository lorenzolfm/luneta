//! What lunetad, the daemon of the host, says of it through `luneta watch`:
//! the panes of each session, how the work tree stands where each pane works,
//! and the agents. The sidebar, the picker and the menu take them from the
//! daemon alone: while it does not answer, they say so, and ask it again
//! every few seconds. Which sessions there are, and their tabs and titles,
//! still come from zellij.
//!
//! Told the version of the host last seen, `luneta watch` waits for the next
//! one, or four seconds at the most, before it answers: asked again as soon
//! as it has, it says each change as it comes. zellij runs each command as a
//! task of its own, so one that waits holds nothing else up.

use luneta_protocol::v1::{Host, Version};
use prost::Message as _;

use crate::context::unstarted;
use crate::elapsed::{Held, Moment};

pub const CONTEXT_VALUE: &str = "daemon";

/// The CLI that asks the daemon, as zellij finds it on its server's `PATH`.
const CLI: &str = "luneta";

/// The command that asks the daemon how the host stands now, which it answers
/// at once, as it is told no version to wait on the next of.
pub const LOOK: [&str; 2] = [CLI, "watch"];

/// How long a daemon that did not answer is left before it is asked again:
/// one that is not running costs no more than a command that fails at once.
const RETRY: Held = Held::from_secs(5);

#[derive(Default)]
pub struct Daemon {
    heard: Heard,
    /// Whether a `luneta watch` is out, yet to answer.
    asking: bool,
}

/// What the daemon said last.
#[derive(Default)]
enum Heard {
    /// Nothing yet.
    #[default]
    Nothing,
    /// The host, as of this version.
    Host(Version),
    /// No answer that reads, at this moment.
    Silent(Moment),
    /// `luneta` could not be started at all, at this moment: it is not
    /// installed, or not on the server's `PATH`. It is not looked for again
    /// until `wake`, so that zellij does not log the failure every few
    /// seconds.
    Absent(Moment),
}

/// What a `luneta watch` answered.
#[derive(Debug)]
pub enum Answer {
    /// The host, changed since it was last seen.
    Changed(Host),
    /// The host as it was last seen.
    Same,
    /// Nothing that reads, and why, as the sidebar and the picker say it.
    Silent(String),
}

impl Daemon {
    /// The command that asks the daemon, if one is to `now`: none while one
    /// is out, nor for `RETRY` after it did not answer.
    pub fn ask(&mut self, now: Moment) -> Option<Vec<String>> {
        if self.asking {
            return None;
        }
        let after = match &self.heard {
            Heard::Host(version) => Some(*version),
            Heard::Silent(at) if now.since(*at) < RETRY => return None,
            Heard::Absent(_) => return None,
            Heard::Nothing | Heard::Silent(_) => None,
        };
        self.asking = true;
        let mut command: Vec<String> = LOOK.iter().map(|part| (*part).to_owned()).collect();
        if let Some(Version { pid, seq }) = after {
            command.extend(["--after".to_owned(), format!("{pid}:{seq}")]);
        }
        Some(command)
    }

    /// What the command `ask` gave answered, `now`.
    pub fn answered(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        now: Moment,
    ) -> Answer {
        self.asking = false;
        let (version, host) = match read(exit_code, stdout, stderr) {
            Ok(read) => read,
            Err(why) => {
                // Once as it stops, and not at every ask after: one that was
                // never there is not news, as it is before it is installed.
                if matches!(self.heard, Heard::Host(_)) {
                    eprintln!("luneta: lunetad stopped answering: {why}");
                }
                self.heard = if unstarted(exit_code, stderr) {
                    Heard::Absent(now)
                } else {
                    Heard::Silent(now)
                };
                return Answer::Silent(silence(exit_code, stderr, &why));
            },
        };
        let seen = matches!(self.heard, Heard::Host(was) if was == version);
        self.heard = Heard::Host(version);
        if seen {
            Answer::Same
        } else {
            Answer::Changed(host)
        }
    }

    /// `luneta` is looked for again if it could not be started, for a sidebar
    /// that comes back to whatever was installed while it was away.
    pub fn wake(&mut self) {
        if let Heard::Absent(at) = self.heard {
            self.heard = Heard::Silent(at);
        }
    }
}

/// What a `LOOK` answered: the host, or why it did not say, as the menu says
/// it.
pub fn looked(exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<Host, String> {
    let (_, host) =
        read(exit_code, stdout, stderr).map_err(|why| silence(exit_code, stderr, &why))?;
    Ok(host)
}

/// Why `luneta watch` did not say how the host stands, as the sidebar, the
/// picker and the menu say it: `luneta` could not be started, or lunetad did
/// not answer, for `why`.
fn silence(exit_code: Option<i32>, stderr: &[u8], why: &str) -> String {
    if unstarted(exit_code, stderr) {
        format!("{CLI} cannot be started: {why}")
    } else {
        format!("lunetad does not answer: {why}")
    }
}

/// The host as `luneta watch` wrote it out, and its version, or why it did
/// not.
fn read(exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<(Version, Host), String> {
    if exit_code != Some(0) {
        let said = String::from_utf8_lossy(stderr);
        let said = said.lines().next().unwrap_or("").trim();
        return Err(if said.is_empty() { format!("{CLI} watch failed") } else { said.to_owned() });
    }
    let host = Host::decode(stdout).map_err(|error| format!("{CLI} watch: {error}"))?;
    // A version is always there: a host without one is a newer daemon's.
    let version = host.version.ok_or_else(|| format!("{CLI} watch: an answer with no version"))?;
    Ok((version, host))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSED: &[u8] = b"luneta: /run/user/1000/luneta/lunetad.sock: transport error: \
        Connection refused (os error 111)\n";

    fn host(pid: u32, seq: u64) -> Vec<u8> {
        Host { version: Some(Version { pid, seq }), ..Host::default() }.encode_to_vec()
    }

    /// `luneta watch`, and `args` after it.
    fn watch(args: &[&str]) -> Vec<String> {
        ["luneta", "watch"].iter().chain(args).map(|arg| (*arg).to_owned()).collect()
    }

    /// The first ask is answered at once, and each after it waits for the
    /// host to change from the version the last answer had: one at a time,
    /// as asked again while one is out, they would pile up in zellij.
    #[test]
    fn the_daemon_is_asked_for_the_next_change_one_ask_at_a_time() {
        let now = Moment::from_millis(1_000_000);
        let mut daemon = Daemon::default();
        assert_eq!(daemon.ask(now), Some(watch(&[])));
        assert_eq!(daemon.ask(now), None);
        assert!(matches!(daemon.answered(Some(0), &host(7, 3), b"", now), Answer::Changed(_)));
        assert_eq!(daemon.ask(now), Some(watch(&["--after", "7:3"])));
        assert!(matches!(daemon.answered(Some(0), &host(7, 3), b"", now), Answer::Same));
        assert_eq!(daemon.ask(now), Some(watch(&["--after", "7:3"])));
        // A daemon that started again counts from 0, under another pid.
        assert!(matches!(daemon.answered(Some(0), &host(8, 0), b"", now), Answer::Changed(_)));
        assert_eq!(daemon.ask(now), Some(watch(&["--after", "8:0"])));
    }

    /// A daemon that is not running, or answers what does not read, is said
    /// not to answer, and why, and is asked again a few seconds on, from the
    /// start. A `luneta` that is not there at all is looked for again only
    /// as the sidebar wakes, as zellij logs each command it cannot start.
    #[test]
    fn a_daemon_that_does_not_answer_is_asked_again_a_while_on() {
        /// How `luneta watch` ended, what it wrote out, and what the sidebar
        /// says of it.
        type Failure<'a> = (Option<i32>, &'a [u8], &'a [u8], &'a str);

        let at = |secs: u64| Moment::from_millis(1_000_000 + secs * 1000);
        let now = at(0);
        let unversioned = Host::default().encode_to_vec();
        let failures: [Failure; 4] = [
            (
                Some(1),
                b"",
                REFUSED,
                "lunetad does not answer: luneta: /run/user/1000/luneta/lunetad.sock: transport \
                 error: Connection refused (os error 111)",
            ),
            (
                Some(0),
                b"\xff\xff",
                b"",
                "lunetad does not answer: luneta watch: failed to decode Protobuf message: \
                 invalid varint",
            ),
            (Some(0), &unversioned, b"", "lunetad does not answer: luneta watch: an answer with no version"),
            (None, b"", b"", "lunetad does not answer: luneta watch failed"),
        ];
        for (exit_code, stdout, stderr, said) in failures {
            let mut daemon = Daemon::default();
            daemon.ask(now);
            daemon.answered(Some(0), &host(7, 3), b"", now);
            daemon.ask(now);
            let answer = daemon.answered(exit_code, stdout, stderr, now);
            assert!(matches!(&answer, Answer::Silent(why) if why == said), "{answer:?}");
            assert_eq!(daemon.ask(at(4)), None);
            assert_eq!(daemon.ask(at(5)), Some(watch(&[])));
        }

        let mut daemon = Daemon::default();
        daemon.ask(now);
        let answer = daemon.answered(Some(2), b"", b"No such file or directory (os error 2)", now);
        let said = "luneta cannot be started: No such file or directory (os error 2)";
        assert!(matches!(&answer, Answer::Silent(why) if why == said), "{answer:?}");
        assert_eq!(daemon.ask(at(60)), None);
        daemon.wake();
        assert_eq!(daemon.ask(at(60)), Some(watch(&[])));
    }
}
