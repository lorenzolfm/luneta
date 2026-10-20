//! What the shell in each pane says it runs, and how what it ran ended.
//!
//! A title says only that a command runs, cut to twenty characters, and not
//! how it ended. A hook in the shell says it all: fish's `fish_preexec` and
//! `fish_postexec` write a file for each shell, `<pane>.<started>`, in a
//! folder for its session under the host's runtime folder, as the README
//! shows. Its one line is one of
//!
//! ```text
//! going <since> <tty> <command>
//! ended <at> <status> <took> <command>
//! stopped <at> <command>
//! ```
//!
//! with `<started>`, `<since>` and `<at>` in seconds since the epoch, `<took>`
//! in milliseconds, `<status>` `-` where the shell cannot say it, as fish
//! cannot after `fg`, and `stopped` for a job stopped with `Ctrl z`. A pane's
//! shell is the one that started last. A shell without the hook writes none,
//! and its title says what it can.
//!
//! What runs is at work, or waits for you to type, or has the terminal to
//! itself, as an editor does: what the terminal says of the command in front
//! tells which. `ps` marks the processes in front with a `+` and says where
//! each sleeps in the kernel, and `stty` reads how the terminal takes keys.

use std::collections::{BTreeMap, HashMap};

use crate::elapsed::{Held, Moment};

pub const CONTEXT_VALUE: &str = "shells";

/// Reads the record of each shell of the sessions it is handed, and for each
/// command still going, how its terminal takes keys and where the processes
/// in front of it sleep. A command going on a terminal that is gone is no
/// pane's, and would fail `ps` for all of them, so it is left out. It reads
/// the records only in a folder of the user's own: what is in one that others
/// share, as `/tmp` is, anyone could have written.
const SCRIPT: &str = r#"
root=${XDG_RUNTIME_DIR:-$TMPDIR}
[ -n "$root" ] && [ -O "$root/luneta" ] && [ ! -L "$root/luneta" ] || exit 0
cd "$root/luneta/commands" 2>/dev/null || exit 0
ttys=
for session in "$@"; do
  for file in "$session"/*; do
    [ -f "$file" ] || continue
    IFS= read -r said < "$file" || continue
    case $said in
    'going '*)
      tty=${said#going * }
      tty=${tty%% *}
      [ -c "$tty" ] || continue
      case " $ttys " in
      *" $tty "*) ;;
      *) ttys="$ttys $tty" ;;
      esac
      ;;
    esac
    printf 'said\t%s\t%s\t%s\n' "$session" "${file##*/}" "$said"
  done
done
[ -n "$ttys" ] || exit 0
set -f
for tty in $ttys; do
  printf 'stty\t%s\t' "$tty"
  stty -a < "$tty" 2>/dev/null | tr '\n' ' '
  echo
done
ps -o tty= -o stat= -o wchan= -t "$(echo $ttys | tr ' ' ',')" 2>/dev/null |
  while IFS= read -r line; do printf 'ps\t%s\n' "$line"; done
"#;

/// The command that reads what the shells of `sessions` say.
pub fn query(sessions: &[String]) -> Vec<&str> {
    let mut command = vec!["sh", "-c", SCRIPT, "sh"];
    command.extend(sessions.iter().map(String::as_str));
    command
}

/// What a shell's hook said last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Said {
    /// It runs `command`, on the terminal `tty`, since `since`.
    Going { command: String, since: Moment, tty: String },
    /// It ran `command`, for as long as `took`, and it ended at `at` with
    /// `status`, if the shell could say it.
    Ended { command: String, at: Moment, status: Option<i32>, took: Held },
    /// It stopped the job it ran, which waits to be brought back.
    Stopped,
}

impl Said {
    fn read(line: &str) -> Option<Self> {
        let (word, rest) = line.split_once(' ')?;
        let (secs, rest) = rest.split_once(' ')?;
        let secs: u64 = secs.parse().ok()?;
        // The hook dates it to the second it started or ended in: the last
        // moment of it, so that a command never ran longer than it did, nor
        // ended before you left.
        let moment = Moment::from_millis(secs.saturating_mul(1000).saturating_add(999));
        match word {
            "going" => {
                let (tty, command) = rest.split_once(' ')?;
                let (command, tty) = (command.to_owned(), tty.to_owned());
                Some(Said::Going { command, since: moment, tty })
            },
            "ended" => {
                let (status, rest) = rest.split_once(' ')?;
                let (took, command) = rest.split_once(' ')?;
                let took = Held::from_millis(took.parse().ok()?);
                let status = match status {
                    "-" => None,
                    status => Some(status.parse().ok()?),
                };
                Some(Said::Ended { command: command.to_owned(), at: moment, status, took })
            },
            "stopped" => Some(Said::Stopped),
            _ => None,
        }
    }
}

/// What the command in front of a terminal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    /// It works, and reads nothing from the terminal.
    Working,
    /// It waits for you to type: it reads the terminal a line at a time, or
    /// with the echo off, as a password prompt does.
    Asking,
    /// It takes the terminal over a key at a time, as an editor, a pager or
    /// `ssh` does, for as long as you use it.
    Interactive,
}

/// Where a process that reads its terminal sleeps in the kernel, as `ps`
/// names it: Linux's, and the BSDs'. A blocking read of a socket sleeps in
/// Linux's too, and reads as a prompt.
const READING: [&str; 3] = ["wait_woken", "n_tty_read", "ttyin"];

/// A process in front of its terminal, as `ps` says: its state, with `s` for
/// the leader of its session, the shell the terminal was opened for, and
/// where it sleeps in the kernel.
#[derive(Clone, Copy, Debug)]
struct Process<'a> {
    stat: &'a str,
    wchan: &'a str,
}

impl Front {
    /// What the command in front does, as the terminal's `modes` say, as
    /// `stty -a` writes them, and the processes in front of it. The shell
    /// itself in front, its terminal a key at a time, is its own `read`
    /// asking you something, as fish's line editor takes the keys.
    fn of(modes: &str, front: &[Process]) -> Self {
        let has = |mode: &str| modes.split_whitespace().any(|m| m == mode);
        let shell = front.iter().any(|process| process.stat.contains('s'));
        let reading = front.iter().any(|process| READING.contains(&process.wchan));
        if has("-icanon") {
            if shell { Front::Asking } else { Front::Interactive }
        } else if has("-echo") || reading {
            Front::Asking
        } else {
            Front::Working
        }
    }
}

#[derive(Default)]
pub struct Shells {
    /// What the shell in each pane of each session said, and when it started.
    said: BTreeMap<String, BTreeMap<u32, (Moment, Said)>>,
    /// What the command in front of each terminal a command is going on does.
    fronts: HashMap<String, Front>,
    asking: bool,
}

impl Shells {
    /// Whether to ask again: not while an answer is on its way.
    pub fn ask(&mut self) -> bool {
        if self.asking {
            return false;
        }
        self.asking = true;
        true
    }

    /// What the shells said, in place of what they said before. A script that
    /// failed says nothing.
    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8]) {
        self.asking = false;
        self.said.clear();
        self.fronts.clear();
        if exit_code != Some(0) {
            return;
        }
        let stdout = String::from_utf8_lossy(stdout);
        let mut modes: HashMap<&str, &str> = HashMap::new();
        let mut fronts: HashMap<String, Vec<Process>> = HashMap::new();
        for line in stdout.lines() {
            let Some((kind, rest)) = line.split_once('\t') else {
                continue;
            };
            match kind {
                "said" => {
                    let mut parts = rest.splitn(3, '\t');
                    let (Some(session), Some(file), Some(said)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        continue;
                    };
                    let Some((pane, started)) = file.split_once('.') else {
                        continue;
                    };
                    let (Ok(pane), Ok(started), Some(said)) =
                        (pane.parse(), started.parse::<u64>(), Said::read(said))
                    else {
                        continue;
                    };
                    let started = Moment::from_millis(started.saturating_mul(1000));
                    // A pane's shell is the one that started last: the others
                    // were a session's that had its name.
                    let panes = self.said.entry(session.to_owned()).or_default();
                    if panes.get(&pane).is_none_or(|(was, _)| *was < started) {
                        panes.insert(pane, (started, said));
                    }
                },
                "stty" => {
                    if let Some((tty, said)) = rest.split_once('\t') {
                        modes.insert(tty, said);
                    }
                },
                "ps" => {
                    let mut fields = rest.split_whitespace();
                    let (Some(tty), Some(stat)) = (fields.next(), fields.next()) else {
                        continue;
                    };
                    if stat.contains('+') {
                        let process = Process { stat, wchan: fields.next().unwrap_or("") };
                        fronts.entry(terminal(tty).to_owned()).or_default().push(process);
                    }
                },
                _ => {},
            }
        }
        for (tty, modes) in modes {
            let front = fronts.get(terminal(tty)).map_or(&[][..], Vec::as_slice);
            self.fronts.insert(tty.to_owned(), Front::of(modes, front));
        }
    }

    /// What the shell in `pane` of `session` said last, and when it started.
    pub fn said(&self, session: &str, pane: u32) -> Option<(Moment, &Said)> {
        self.said.get(session)?.get(&pane).map(|(started, said)| (*started, said))
    }

    /// What the command in front of `tty` does, or nothing where the script
    /// found no terminal to ask.
    pub fn front(&self, tty: &str) -> Option<Front> {
        self.fronts.get(tty).copied()
    }

    /// The shells that said `said`, each in its session and pane, started at
    /// its moment, and the terminals that do as `fronts` say.
    #[cfg(test)]
    pub fn of(said: Vec<(&str, u32, Moment, Said)>, fronts: Vec<(&str, Front)>) -> Self {
        let mut shells = Shells::default();
        for (session, pane, started, said) in said {
            shells.said.entry(session.to_owned()).or_default().insert(pane, (started, said));
        }
        shells.fronts = fronts.into_iter().map(|(tty, front)| (tty.to_owned(), front)).collect();
        shells
    }
}

/// A terminal as `ps` names it: `pts/3` for `/dev/pts/3`.
fn terminal(tty: &str) -> &str {
    tty.strip_prefix("/dev/").unwrap_or(tty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_reads_as_the_hook_writes_it() {
        assert_eq!(
            Said::read("going 1760900000 /dev/pts/3 cargo build --release"),
            Some(Said::Going {
                command: "cargo build --release".to_owned(),
                since: Moment::from_millis(1_760_900_000_999),
                tty: "/dev/pts/3".to_owned(),
            })
        );
        assert_eq!(
            Said::read("ended 1760900042 101 41873 cargo test"),
            Some(Said::Ended {
                command: "cargo test".to_owned(),
                at: Moment::from_millis(1_760_900_042_999),
                status: Some(101),
                took: Held::from_millis(41_873),
            })
        );
        let fg = Said::read("ended 1760900050 - 7120 cargo test");
        assert!(matches!(fg, Some(Said::Ended { status: None, .. })), "{fg:?}");
        assert_eq!(Said::read("stopped 1760900045 cargo test"), Some(Said::Stopped));
        for wrong in [
            "",
            "going",
            "going 12 /dev/pts/3",
            "went 12 /dev/pts/3 make",
            "ended 12 x 5 make",
            "stopped 12",
        ] {
            assert_eq!(Said::read(wrong), None, "{wrong:?}");
        }
    }

    /// A terminal that takes keys one at a time is a program's own, or the
    /// shell's, which reads one line in its own editor. One with its echo off,
    /// or a process in front that reads it, waits for you to type. Else the
    /// command is at work.
    #[test]
    fn a_terminal_says_whether_its_command_works_asks_or_is_interactive() {
        let cooked = "speed 38400 baud; line = 0; isig icanon iexten echo echoe";
        let raw = "speed 38400 baud; line = 0; isig -icanon iexten -echo echoe";
        let password = "speed 38400 baud; line = 0; isig icanon iexten -echo echoe";
        let ps = |stat, wchan| Process { stat, wchan };
        for (modes, front, said) in [
            (cooked, vec![ps("S+", "hrtimer_nanosleep")], Front::Working),
            (cooked, vec![ps("S+", "do_wait"), ps("S+", "wait_woken")], Front::Asking),
            (cooked, vec![ps("S+", "ttyin")], Front::Asking),
            (password, vec![ps("Ss+", "0")], Front::Asking),
            (raw, vec![ps("S+", "wait_woken")], Front::Interactive),
            (raw, vec![ps("Sl+", "do_epoll_wait")], Front::Interactive),
            (raw, vec![ps("Ss+", "poll_schedule_timeout.constprop.0")], Front::Asking),
        ] {
            assert_eq!(Front::of(modes, &front), said, "{modes:?} {front:?}");
        }
    }

    /// What the script writes: each shell's record, and for the terminals of
    /// the commands going, their modes and the processes on them. Only the
    /// ones in front count. A pane's shell is the one that started last, and
    /// a file that does not say when its shell started, or is still being
    /// written, is none.
    #[test]
    fn an_answer_says_what_each_pane_ran_and_what_its_terminal_does() {
        let answer = [
            "said\tluneta\t3.90\tgoing 100 /dev/pts/3 cargo build",
            "said\tluneta\t4.90\tgoing 100 /dev/pts/4 rm -i notes",
            "said\tluneta\t5.95\tended 100 0 1500 make",
            "said\tluneta\t5.80\tended 100 1 1500 make",
            "said\tluneta\t6\tended 100 0 1500 make",
            "said\tluneta\t6.90.new\tended 100 0 1500 make",
            "said\tluneta\tx.90\tended 100 0 1500 make",
            "said\tbipa\t3.90\tgoing 100 /dev/pts/9 nvim",
            "stty\t/dev/pts/3\tspeed 38400 baud; icanon echo ",
            "stty\t/dev/pts/4\tspeed 38400 baud; icanon echo ",
            "stty\t/dev/pts/9\tspeed 38400 baud; -icanon -echo ",
            "ps\tpts/3    Ss   do_wait",
            "ps\tpts/3    Sl+  futex_do_wait",
            "ps\tpts/4    Ss   wait_woken",
            "ps\tpts/4    S+   wait_woken",
        ]
        .join("\n");
        let mut shells = Shells::default();
        assert!(shells.ask());
        assert!(!shells.ask());
        shells.ingest(Some(0), answer.as_bytes());
        assert!(shells.ask());
        assert!(matches!(shells.said("luneta", 3), Some((_, Said::Going { .. }))));
        let (five, newest) = (shells.said("luneta", 5), Moment::from_millis(95_000));
        let from_newest = matches!(five, Some((at, Said::Ended { status: Some(0), .. })) if at == newest);
        assert!(from_newest, "{five:?}");
        assert_eq!(shells.said("luneta", 6), None);
        assert!(matches!(shells.said("bipa", 3), Some((_, Said::Going { .. }))));
        assert_eq!(shells.said("bipa", 4), None);
        assert_eq!(shells.front("/dev/pts/3"), Some(Front::Working));
        assert_eq!(shells.front("/dev/pts/4"), Some(Front::Asking));
        assert_eq!(shells.front("/dev/pts/9"), Some(Front::Interactive));

        shells.ingest(Some(1), answer.as_bytes());
        assert_eq!(shells.said("luneta", 3), None);
    }

    /// The script reads the records of the sessions it is handed, and no
    /// others, and asks about the terminal of a command going, by the name
    /// its record gives, which is no pattern of files. One going on a
    /// terminal that is gone is no pane's. A folder that is a link to one is
    /// none of the user's own, and says nothing.
    #[cfg(unix)]
    #[test]
    fn the_script_reads_the_records_of_the_sessions_it_is_handed() {
        let runtime = std::env::temp_dir().join(format!("luneta-shells-{}", std::process::id()));
        let commands = runtime.join("luneta/commands");
        let starred = runtime.join("tty*");
        let starred_going = format!("going 100 {} sleep 30\n", starred.display());
        for (session, pane, said) in [
            ("s", "1.90", "going 100 /dev/null sleep 30\n"),
            ("s", "2.90", "ended 100 1 2500 false\n"),
            ("s", "3.90", "going 100 /dev/luneta-gone sleep 30\n"),
            ("s", "4.90", starred_going.as_str()),
            ("gone", "1.90", "ended 100 0 2500 true\n"),
        ] {
            std::fs::create_dir_all(commands.join(session)).unwrap();
            std::fs::write(commands.join(session).join(pane), said).unwrap();
        }
        std::os::unix::fs::symlink("/dev/null", &starred).unwrap();
        std::fs::write(runtime.join("ttyfile"), "").unwrap();
        let linked = runtime.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(runtime.join("luneta"), linked.join("luneta")).unwrap();
        let sessions = ["s".to_owned(), "never".to_owned()];
        let command = query(&sessions);
        let run = |runtime: &std::path::Path| {
            std::process::Command::new(command[0])
                .args(&command[1..])
                .env("XDG_RUNTIME_DIR", runtime)
                .output()
                .unwrap()
        };
        let (output, through_link) = (run(&runtime), run(&linked));
        std::fs::remove_dir_all(&runtime).unwrap();
        assert_eq!(through_link.status.code(), Some(0));
        assert_eq!(String::from_utf8_lossy(&through_link.stdout), "");
        assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
        let mut shells = Shells::default();
        shells.ingest(output.status.code(), &output.stdout);
        let one = shells.said("s", 1);
        assert!(matches!(one, Some((_, Said::Going { tty, .. })) if tty == "/dev/null"), "{one:?}");
        assert!(matches!(shells.said("s", 2), Some((_, Said::Ended { status: Some(1), .. }))));
        assert_eq!(shells.said("s", 3), None);
        assert_eq!(shells.said("gone", 1), None);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.lines().any(|line| line.starts_with("stty\t/dev/null\t")), "{stdout}");
        assert!(!stdout.contains("ttyfile"), "{stdout}");
    }
}
