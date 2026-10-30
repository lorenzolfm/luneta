//! What the command a shell ran last printed, for the agent that the menu's
//! `c f` or `c e` starts beside it, to fix what went wrong or explain it.
//! zellij dumps the text of the shell's pane, its scrollback with it, and
//! luneta keeps what follows the line the command was typed on, or the `fg`
//! that brought its job back, up to the prompt the shell waits at, in a file
//! of its own that the agent is let read. The command line and how it ended
//! are what luneta's shell hook said, through the daemon: zellij keeps the
//! text alone, and the hook sees none of it.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::shell::Ending;
use lunetad::folder;

use crate::host::Event;
use crate::menu::Ended;
use crate::zellij;

/// The most lines a file keeps: the last a command printed, which say how it
/// ended. Claude Code reads as many at once.
const TAIL: usize = 2000;

/// How long a file is kept: luneta takes away older ones as it keeps another.
/// The agent reads it as it starts.
const KEPT: Duration = Duration::from_hours(24);

/// The folder the files are kept in, in luneta's.
const FOLDER: &str = "printed";

/// How many files this luneta has kept, which tells apart the names of
/// those it keeps in the same moment.
static FILES: AtomicU64 = AtomicU64::new(0);

/// Where the lines a pane printed start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    /// After the line the command was typed on: `fg`, for a job it brought
    /// back.
    Command,
    /// At the top of what the pane holds: no line holds the command, as
    /// after a `clear`, or once it scrolled out of what zellij keeps.
    Top,
}

/// What a pane printed, as luneta kept it for an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printed {
    pub from: From,
    /// The file it is kept in, none where it printed nothing.
    pub kept: Option<Kept>,
}

/// A file of what a pane printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// Its path, with every link in it followed, as the agent's leave to read
    /// it names it.
    pub file: PathBuf,
    /// How many lines it holds, and how many came before them, left out.
    pub lines: usize,
    pub left_out: usize,
}

impl Printed {
    /// What an agent is told: `asks`, what it is asked to do, then the
    /// command that `ended`, and how, where its shell can say, and where what
    /// it printed is: for a job `fg` brought back, what it printed after
    /// that.
    pub fn prompt(&self, asks: &str, ended: &Ended) -> String {
        let cannot = "with an exit status my shell cannot say";
        let (how, after, gone) = match ended.ending {
            Some(Ending::Exited(status)) => {
                (format!("ended with exit status {status}"), "", "The line I ran it on")
            },
            None => (format!("ended, {cannot}"), "", "The line I ran it on"),
            Some(Ending::Resumed) => (
                format!("ended once I brought it back with `fg`, {cannot}"),
                " after `fg`",
                "The `fg` I brought it back with",
            ),
        };
        let gone = format!("{gone} is no longer on my screen");
        let printed = match (&self.kept, self.from) {
            (None, From::Command) => format!("It printed nothing{after}."),
            (None, From::Top) => format!("{gone}, and nothing else is."),
            (Some(Kept { file, left_out: 0, .. }), From::Command) => {
                format!("What it printed{after} is in {}.", file.display())
            },
            (Some(Kept { file, lines, left_out }), From::Command) => {
                let of = lines + left_out;
                format!(
                    "The last {lines} of the {of} lines it printed{after} are in {}.",
                    file.display()
                )
            },
            (Some(Kept { file, lines, left_out }), From::Top) => {
                let of = lines + left_out;
                let holds = if *left_out == 0 {
                    "all my screen has".to_owned()
                } else {
                    format!("the last {lines} of the {of} lines my screen has")
                };
                format!(
                    "{gone}, so {} holds {holds}, which may begin before what it printed.",
                    file.display()
                )
            },
        };
        format!("{asks} In my shell, `{}` {how}. {printed}", ended.command.as_str())
    }

    /// The file the agent is let read, where there is one.
    pub fn file(&self) -> Option<&Path> {
        self.kept.as_ref().map(|kept| kept.file.as_path())
    }
}

/// Has zellij dump the terminal `pane` of `session`, and keeps what it
/// printed after the line the command that `ended` was last typed on, for
/// the menu numbered `menu`.
pub fn read(menu: u64, session: &str, pane: u32, ended: Ended, events: &Sender<Event>) {
    let (session, events) = (session.to_owned(), events.clone());
    thread::spawn(move || {
        let printed = zellij::dump(&session, pane).and_then(|dump| {
            let runtime = folder::find()
                .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to keep it in")?;
            keep(&runtime, &cut(&dump, typed(&ended)), SystemTime::now())
        });
        // luneta is on its way out.
        let _ = events.send(Event::Printed { menu, printed });
    });
}

/// The line the command that `ended` was typed on, as the hook says it: `fg`,
/// for a job that `fg` brought back, or the command, which the hook says for
/// either.
fn typed(ended: &Ended) -> &str {
    match ended.ending {
        Some(Ending::Resumed) => "fg",
        Some(Ending::Exited(_)) | None => ended.command.as_str(),
    }
}

/// What a pane printed, as `cut` found it in what zellij dumped.
#[derive(Debug, PartialEq, Eq)]
struct Cut<'a> {
    from: From,
    /// The last `TAIL` lines of it at most, and how many came before them.
    lines: Vec<&'a str>,
    left_out: usize,
}

/// What `dump`, the text of a shell's pane, holds after the line `command`
/// was last run on, up to the prompt the shell waits at, its last line; or,
/// where no line holds `command`, all it holds up to that prompt. Of a prompt
/// of more than one line, the lines before the one the shell waits on stay,
/// as luneta cannot tell them from what the command printed. The blank lines
/// at either end go.
fn cut<'a>(dump: &'a str, command: &str) -> Cut<'a> {
    let mut lines: Vec<&str> = dump.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    // The prompt the shell waits at.
    lines.pop();
    let (from, start) = match ran_on(&lines, command) {
        Some(on) => (From::Command, on + 1),
        None => (From::Top, 0),
    };
    let printed = &lines[start..];
    let filled = |line: &&str| !line.trim().is_empty();
    let first = printed.iter().position(filled).unwrap_or(printed.len());
    let end = printed.iter().rposition(filled).map_or(first, |last| last + 1);
    let printed = &printed[first..end];
    let left_out = printed.len().saturating_sub(TAIL);
    Cut { from, lines: printed[left_out..].to_vec(), left_out }
}

/// The line of `lines` that `command` ends on, on the last line that holds
/// it after a prompt: whole, or its start, where it goes on over the lines
/// after it, as a command of more than one line does. Any whitespace counts
/// as one space, as the hook says a command of more than one line on one.
fn ran_on(lines: &[&str], command: &str) -> Option<usize> {
    let command = spaced(command);
    let word = command.split(' ').next().filter(|word| !word.is_empty())?;
    let held = |at: &usize| lines[*at].contains(word);
    let ran = |at: usize| ends_on(&lines[at..], &command).map(|on| at + on);
    (0..lines.len()).rev().filter(held).find_map(ran)
}

/// The line of `lines` that `command` ends on, where it was run on the first
/// of them: it starts at a word of the first line after another, the prompt,
/// and ends at a word's end.
fn ends_on(lines: &[&str], command: &str) -> Option<usize> {
    let first = spaced(lines.first()?);
    let mut text = first.clone();
    // Where each line ends in `text`.
    let mut ends = vec![text.len()];
    for line in &lines[1..] {
        if text.len() >= first.len() + command.len() {
            break;
        }
        let line = spaced(line);
        if !line.is_empty() {
            text.push(' ');
            text.push_str(&line);
        }
        ends.push(text.len());
    }
    let mut starts = first.match_indices(' ').map(|(at, _)| at + 1);
    starts.find_map(|start| {
        let end = start + command.len();
        let ran = text[start..].starts_with(command)
            && matches!(text[end..].chars().next(), None | Some(' '));
        ran.then(|| ends.iter().position(|line_end| *line_end >= end))?
    })
}

/// `text` with any whitespace in it as one space, and none at either end.
fn spaced(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keeps `cut` in a file of its own, for this user alone, in the folder
/// `FOLDER` of luneta's folder `runtime`, made as the daemon makes it, and
/// takes away those kept there `KEPT` before `now`. None where the pane
/// printed nothing.
fn keep(runtime: &Path, cut: &Cut<'_>, now: SystemTime) -> Result<Printed, String> {
    let Cut { from, lines, left_out } = cut;
    if lines.is_empty() {
        return Ok(Printed { from: *from, kept: None });
    }
    let folder = folder::make(runtime).and_then(|_| folder::make(&runtime.join(FOLDER)));
    let folder = folder.map_err(|refusal| refusal.to_string())?;
    sweep(folder.path(), now);
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let count = FILES.fetch_add(1, Ordering::Relaxed);
    let path = folder.path().join(format!("{since}-{}-{count}.txt", std::process::id()));
    let failed = |error: io::Error| format!("{}: {error}", path.display());
    let mut text = lines.join("\n");
    text.push('\n');
    let mut file =
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(failed)?;
    file.write_all(text.as_bytes()).map_err(failed)?;
    let file = fs::canonicalize(&path).map_err(failed)?;
    Ok(Printed { from: *from, kept: Some(Kept { file, lines: lines.len(), left_out: *left_out }) })
}

/// Takes away the files in `folder` last written `KEPT` before `now`. One
/// that does not go stays for the next to take away.
fn sweep(folder: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(found) = entry.metadata() else {
            continue;
        };
        let gone = found.modified().ok().and_then(|at| at.checked_add(KEPT));
        if found.is_file() && gone.is_some_and(|gone| gone < now) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;

    use luneta_protocol::said::Said;

    use super::*;

    /// The lines `cut` kept, and where they start.
    fn kept<'a>(dump: &'a str, command: &str) -> (From, Vec<&'a str>) {
        let Cut { from, lines, left_out } = cut(dump, command);
        assert_eq!(left_out, 0);
        (from, lines)
    }

    /// `command`, which ended as `ending` says.
    fn ended(command: &str, ending: Option<Ending>) -> Ended {
        Ended { command: Said::new(command.to_owned()).unwrap(), ending, at: UNIX_EPOCH }
    }

    /// What a pane printed starts after the line the command was run on, as
    /// fish's own prompt shows it in front, and ends at the prompt the shell
    /// waits at, as zellij dumped them.
    #[test]
    fn what_a_command_printed_is_what_follows_its_line_up_to_the_prompt() {
        let dump = "Welcome to fish, the friendly interactive shell\n\
                    lorenzo@nixos ~/w/luneta>\n\
                    lorenzo@nixos ~/w/luneta> printf \"line %s\\n\" (seq 1 3); false\n\
                    line 1\n\
                    line 2\n\
                    line 3\n\
                    lorenzo@nixos ~/w/luneta [1]>\n";
        let command = "printf \"line %s\\n\" (seq 1 3); false";
        assert_eq!(kept(dump, command), (From::Command, vec!["line 1", "line 2", "line 3"]));
        // One that printed nothing.
        assert_eq!(kept("➜ true\n➜\n\n", "true"), (From::Command, vec![]));
    }

    /// The last line the command was run on is the one: the output of a run
    /// before goes. A command of more than one line, which the hook says on
    /// one, ends on its last, and a line that ends with a right prompt holds
    /// it too.
    #[test]
    fn the_command_s_last_run_is_found_over_its_lines() {
        let dump = "➜ cargo test\nfirst run\n\n➜ cargo test       12:00\nsecond run\n➜\n";
        assert_eq!(kept(dump, "cargo test"), (From::Command, vec!["second run"]));
        let dump = "~> begin\n       echo one\n       echo two\n   end\none\ntwo\n~>\n";
        assert_eq!(kept(dump, "begin echo one echo two end"), (From::Command, vec!["one", "two"]));
    }

    /// The command alone on a line, with no prompt in front of it, is not
    /// where it was run, nor is a word that ends with it.
    #[test]
    fn the_command_s_line_has_a_prompt_in_front_of_it() {
        let dump = "➜ ls\nls\ntools\n➜\n";
        assert_eq!(kept(dump, "ls"), (From::Command, vec!["ls", "tools"]));
    }

    /// With no line that holds the command, as after a `clear`, all the pane
    /// holds up to the prompt is kept, and says so. A prompt of more lines
    /// than one leaves those before the one the shell waits on.
    #[test]
    fn without_the_command_s_line_all_the_pane_holds_is_kept() {
        assert_eq!(kept("out\nmore\n➜\n", "make"), (From::Top, vec!["out", "more"]));
        let dump = "\nluneta on main\n➜ make\nbuilt\n\nluneta on main took 3s\n➜\n";
        let built = vec!["built", "", "luneta on main took 3s"];
        assert_eq!(kept(dump, "make"), (From::Command, built));
    }

    /// A job that `fg` brought back printed after the `fg`, where the hook
    /// says the job's command, and its status as `fg`: what it printed
    /// before it stopped, and what ran while it was stopped, came before. Of
    /// `fg %1`, the hook says the line as it was typed.
    #[test]
    fn a_job_brought_back_printed_after_its_fg() {
        let dump = "➜ cargo build\n   Compiling foo\n^Z\nfish: Job 1, 'cargo build' has stopped\n\
                    ➜ git status\nOn branch main\n➜ fg\nSend job 1 'cargo build' to foreground\n\
                    error[E0308]: mismatched types\n➜\n";
        let resumed = ended("cargo build", Some(Ending::Resumed));
        let back = vec!["Send job 1 'cargo build' to foreground", "error[E0308]: mismatched types"];
        assert_eq!(kept(dump, typed(&resumed)), (From::Command, back));
        let numbered = ended("fg %1", Some(Ending::Resumed));
        assert_eq!(kept("➜ fg %1\nerror\n➜\n", typed(&numbered)), (From::Command, vec!["error"]));
    }

    /// Of a long output, the last `TAIL` lines are kept, and the rest counted.
    #[test]
    fn a_long_output_keeps_its_last_lines() {
        let output: Vec<String> = (1..=TAIL + 5).map(|n| format!("line {n}")).collect();
        let dump = format!("➜ seq\n{}\n➜\n", output.join("\n"));
        let Cut { from, lines, left_out } = cut(&dump, "seq");
        assert_eq!((from, lines.len(), left_out), (From::Command, TAIL, 5));
        assert_eq!((lines[0], lines[TAIL - 1]), ("line 6", output[TAIL + 4].as_str()));
    }

    /// The lines go in a file of their own, for this user alone, in the
    /// folder `printed` of luneta's, which is made so; and the files kept
    /// there more than a day go as another is kept. Nothing printed keeps no
    /// file.
    #[test]
    fn what_a_pane_printed_is_kept_in_a_file_of_this_user_s_alone() {
        let root = tempfile::tempdir().expect("a folder");
        let runtime = root.path().join("luneta");
        let now = SystemTime::now();
        let nothing = Cut { from: From::Command, lines: vec![], left_out: 0 };
        assert_eq!(keep(&runtime, &nothing, now), Ok(Printed { from: From::Command, kept: None }));
        assert!(!runtime.exists());

        let cut = Cut { from: From::Top, lines: vec!["one", "two"], left_out: 3 };
        let first = keep(&runtime, &cut, now).expect("kept");
        let folder = fs::canonicalize(runtime.join(FOLDER)).expect("the folder");
        let Some(Kept { file, lines: 2, left_out: 3 }) = &first.kept else {
            panic!("kept {first:?}");
        };
        assert_eq!((first.from, file.parent()), (From::Top, Some(folder.as_path())));
        assert_eq!(fs::read_to_string(file).expect("the file"), "one\ntwo\n");
        let mode = |path: &Path| fs::metadata(path).expect("there").permissions().mode() & 0o777;
        assert_eq!((mode(&runtime), mode(&folder), mode(file)), (0o700, 0o700, 0o600));

        let aged = |name: &str, ago: Duration| {
            let path = folder.join(name);
            File::create(&path).and_then(|it| it.set_modified(now - ago)).expect("aged");
            path
        };
        let old = aged("old.txt", Duration::from_hours(25));
        let young = aged("young.txt", Duration::from_hours(23));
        let second = keep(&runtime, &cut, now).expect("kept");
        assert_ne!(second.file(), first.file());
        assert!(!old.exists());
        assert!(young.exists() && file.exists());
    }

    /// The agent is told what the command was and how it ended, and where
    /// what it printed is, or that it printed nothing, and how much of it
    /// that is.
    #[test]
    fn the_agent_is_told_the_command_how_it_ended_and_where_what_it_printed_is() {
        let file = PathBuf::from("/run/luneta/printed/1.txt");
        let kept = |from, lines, left_out| Printed {
            from,
            kept: Some(Kept { file: file.clone(), lines, left_out }),
        };
        let none = |from| Printed { from, kept: None };
        let failed = ended("cargo test", Some(Ending::Exited(101)));
        let told = |printed: Printed| printed.prompt("Fix this.", &failed);
        let ran = "Fix this. In my shell, `cargo test` ended with exit status 101.";
        let gone = "The line I ran it on is no longer on my screen";
        assert_eq!(
            told(kept(From::Command, 2, 0)),
            format!("{ran} What it printed is in /run/luneta/printed/1.txt.")
        );
        assert_eq!(
            told(kept(From::Command, 2000, 7)),
            format!(
                "{ran} The last 2000 of the 2007 lines it printed are in \
                 /run/luneta/printed/1.txt."
            )
        );
        assert_eq!(told(none(From::Command)), format!("{ran} It printed nothing."));
        assert_eq!(
            told(kept(From::Top, 2, 0)),
            format!(
                "{ran} {gone}, so /run/luneta/printed/1.txt holds all my screen has, which may \
                 begin before what it printed."
            )
        );
        assert_eq!(
            told(kept(From::Top, 2000, 1)),
            format!(
                "{ran} {gone}, so /run/luneta/printed/1.txt holds the last 2000 of the 2001 lines \
                 my screen has, which may begin before what it printed."
            )
        );
        assert_eq!(told(none(From::Top)), format!("{ran} {gone}, and nothing else is."));
        assert_eq!(
            none(From::Command).prompt("Explain this.", &ended("vim", None)),
            "Explain this. In my shell, `vim` ended, with an exit status my shell cannot say. It \
             printed nothing."
        );

        // A job `fg` brought back printed after it.
        let resumed = ended("cargo build", Some(Ending::Resumed));
        let told = |printed: Printed| printed.prompt("Fix this.", &resumed);
        let ran = "Fix this. In my shell, `cargo build` ended once I brought it back with `fg`, \
                   with an exit status my shell cannot say.";
        assert_eq!(
            told(kept(From::Command, 2, 0)),
            format!("{ran} What it printed after `fg` is in /run/luneta/printed/1.txt.")
        );
        assert_eq!(
            told(kept(From::Command, 2000, 7)),
            format!(
                "{ran} The last 2000 of the 2007 lines it printed after `fg` are in \
                 /run/luneta/printed/1.txt."
            )
        );
        assert_eq!(told(none(From::Command)), format!("{ran} It printed nothing after `fg`."));
        assert_eq!(
            told(none(From::Top)),
            format!(
                "{ran} The `fg` I brought it back with is no longer on my screen, and nothing else \
                 is."
            )
        );
    }
}
