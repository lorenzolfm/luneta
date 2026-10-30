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
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::shell::Ending;
use lunetad::folder;

use crate::due::Due;
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

/// What a rule of Claude Code's reads as more than a path, as gitignore
/// does, or as where the rule ends: a file whose path holds one is not let
/// read by a rule that names it.
const PATTERN: [char; 7] = ['*', '?', '[', ']', '\\', '(', ')'];

/// How many files this luneta has kept, which tells apart the names of
/// those it keeps in the same moment.
static FILES: AtomicU64 = AtomicU64::new(0);

/// What a pane printed, as luneta found it in what zellij dumped: `T` is
/// the lines of it, which a file holds once luneta kept them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Printed<T = Kept> {
    /// What the command printed, after the line it was typed on, or the
    /// `fg` that brought its job back.
    After(T),
    /// Nothing follows the line the command was typed on, the first the
    /// pane holds: it printed nothing.
    Nothing,
    /// Nothing follows the last line that holds the command after a word,
    /// which may be a line it printed, and a line above it the one it was
    /// typed on: all the pane holds, up to and with that line, for the agent
    /// to tell.
    Unsure(T),
    /// No line holds the command, as after a `clear`, or once it scrolled
    /// out of what zellij keeps: all the pane holds, where it holds any.
    Top(Option<T>),
}

/// A file of what a pane printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// Its path, with every link in it followed, as the agent's leave to read
    /// it names it: text, with nothing in it a rule reads as a pattern.
    pub file: String,
    /// How many lines it holds, and how many came before them, left out.
    pub lines: usize,
    pub left_out: usize,
}

impl<T> Printed<T> {
    /// What it printed, with the lines of it made into what `make` makes of
    /// them, or what went wrong making them.
    fn try_map<U, E>(self, make: impl FnOnce(T) -> Result<U, E>) -> Result<Printed<U>, E> {
        Ok(match self {
            Printed::After(lines) => Printed::After(make(lines)?),
            Printed::Nothing => Printed::Nothing,
            Printed::Unsure(lines) => Printed::Unsure(make(lines)?),
            Printed::Top(lines) => Printed::Top(lines.map(make).transpose()?),
        })
    }
}

impl Printed {
    /// What an agent is told: `asks`, what it is asked to do, then the
    /// command that `ended`, and how, where its shell can say, and where what
    /// it printed is: for a job `fg` brought back, what it printed after
    /// that.
    pub fn prompt(&self, asks: &str, ended: &Ended) -> String {
        let cannot = "with an exit status my shell cannot say";
        let (how, after, line) = match ended.ending {
            Some(Ending::Exited(status)) => {
                (format!("ended with exit status {status}"), "", "the line I ran it on")
            },
            None => (format!("ended, {cannot}"), "", "the line I ran it on"),
            Some(Ending::Resumed) => (
                format!("ended once I brought it back with `fg`, {cannot}"),
                " after `fg`",
                "the `fg` I brought it back with",
            ),
        };
        let screen = |Kept { file, lines, left_out }: &Kept| {
            let holds = if *left_out == 0 {
                "all my screen has".to_owned()
            } else {
                format!("the last {lines} of the {} lines my screen has", lines + left_out)
            };
            format!("{file} holds {holds}")
        };
        let printed = match self {
            Printed::After(Kept { file, left_out: 0, .. }) => {
                format!("What it printed{after} is in {file}.")
            },
            Printed::After(Kept { file, lines, left_out }) => {
                let of = lines + left_out;
                format!("The last {lines} of the {of} lines it printed{after} are in {file}.")
            },
            Printed::Nothing => format!("It printed nothing{after}."),
            Printed::Unsure(kept) => format!(
                "Nothing follows the last line on my screen that looks like {line}, which may \
                 be one it printed, so {}.",
                screen(kept)
            ),
            Printed::Top(None) => format!("My screen no longer shows {line}, nor anything else."),
            Printed::Top(Some(kept)) => format!(
                "My screen no longer shows {line}, so {}, which may begin before what it printed.",
                screen(kept)
            ),
        };
        format!("{asks} In my shell, `{}` {how}. {printed}", ended.command.as_str())
    }

    /// The file the agent is let read, where there is one.
    pub fn file(&self) -> Option<&str> {
        match self {
            Printed::After(kept) | Printed::Unsure(kept) | Printed::Top(Some(kept)) => {
                Some(&kept.file)
            },
            Printed::Nothing | Printed::Top(None) => None,
        }
    }
}

/// Has zellij dump the terminal `pane` of `session`, and keeps what it
/// printed after the line the command that `ended` was last typed on, for
/// the menu numbered `menu`, on a thread of its own, and hands back what it
/// kept, or why it kept nothing, as `Due` does.
pub fn read(menu: u64, session: &str, pane: u32, ended: Ended, events: &Sender<Event>) {
    let session = session.to_owned();
    let due = Due::new(events, move |printed| Event::Printed { menu, printed });
    thread::spawn(move || {
        let printed = zellij::dump(&session, pane).and_then(|dump| {
            let runtime = folder::find()
                .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to keep it in")?;
            keep(&runtime, cut(&dump, typed(&ended)), SystemTime::now())
        });
        due.pay(printed);
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

/// The lines of what a pane printed, as `cut` found them: the last `TAIL`
/// at most, never none, and how many came before them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tail<'a> {
    lines: Vec<&'a str>,
    left_out: usize,
}

/// What `dump`, the text of a shell's pane, holds after the line `typed` was
/// last typed on, up to the prompt the shell waits at, its last line; or,
/// where no line holds `typed`, all it holds up to that prompt. Of a prompt
/// of more than one line, the lines before the one the shell waits on stay,
/// as luneta cannot tell them from what the command printed. The blank lines
/// at either end go.
fn cut<'a>(dump: &'a str, typed: &str) -> Printed<Tail<'a>> {
    let mut lines: Vec<&str> = dump.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    // The prompt the shell waits at.
    lines.pop();
    let Some(on) = ran_on(&lines, typed) else {
        return Printed::Top(tail(&lines));
    };
    if let Some(after) = tail(&lines[on + 1..]) {
        return Printed::After(after);
    }
    // The line that holds `typed` may be one the command printed, after
    // another word, and a line above it the one it was typed on.
    match tail(&lines[..=on]) {
        Some(screen) if screen.lines.len() + screen.left_out > 1 => Printed::Unsure(screen),
        _ => Printed::Nothing,
    }
}

/// The last `TAIL` of `lines` at most, with the blank lines at either end
/// gone, and how many came before them; none where every line is blank.
fn tail<'a>(lines: &[&'a str]) -> Option<Tail<'a>> {
    let filled = |line: &&str| !line.trim().is_empty();
    let first = lines.iter().position(filled)?;
    let last = lines.iter().rposition(filled)?;
    let lines = &lines[first..=last];
    let left_out = lines.len().saturating_sub(TAIL);
    Some(Tail { lines: lines[left_out..].to_vec(), left_out })
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

/// Keeps the lines of `cut` in a file of their own, where it has any.
fn keep(runtime: &Path, cut: Printed<Tail<'_>>, now: SystemTime) -> Result<Printed, String> {
    cut.try_map(|tail| write(runtime, &tail, now))
}

/// Writes `tail` in a file of its own, for this user alone, in the folder
/// `FOLDER` of luneta's folder `runtime`, made as the daemon makes it, and
/// takes away those kept there `KEPT` before `now`. None where the folder's
/// path, with every link in it followed, is not text, or holds what a rule
/// of Claude Code's reads as a pattern: no rule would let the agent read it.
fn write(runtime: &Path, tail: &Tail<'_>, now: SystemTime) -> Result<Kept, String> {
    let Tail { lines, left_out } = tail;
    let folder = folder::make(runtime).and_then(|_| folder::make(&runtime.join(FOLDER)));
    let folder = folder.map_err(|refusal| refusal.to_string())?;
    let found = fs::canonicalize(folder.path());
    let found = found.map_err(|error| format!("{}: {error}", folder.path().display()))?;
    let found = found.into_os_string().into_string();
    let found = found.map_err(|found| format!("{} is not text", Path::new(&found).display()))?;
    if let Some(pattern) = found.chars().find(|c| PATTERN.contains(c)) {
        return Err(format!(
            "{found} holds `{pattern}`, which a rule of Claude Code's reads as more than a path"
        ));
    }
    sweep(folder.path(), now);
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let count = FILES.fetch_add(1, Ordering::Relaxed);
    let file = format!("{found}/{since}-{}-{count}.txt", std::process::id());
    let failed = |error: io::Error| format!("{file}: {error}");
    let mut text = lines.join("\n");
    text.push('\n');
    let mut written =
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file).map_err(failed)?;
    written.write_all(text.as_bytes()).map_err(failed)?;
    Ok(Kept { file, lines: lines.len(), left_out: *left_out })
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

    /// What `cut` found, with the lines of it.
    fn kept<'a>(dump: &'a str, typed: &str) -> Printed<Vec<&'a str>> {
        let kept = cut(dump, typed).try_map(|tail| {
            assert_eq!(tail.left_out, 0);
            Ok::<_, ()>(tail.lines)
        });
        kept.unwrap()
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
        assert_eq!(kept(dump, command), Printed::After(vec!["line 1", "line 2", "line 3"]));
    }

    /// The last line the command was run on is the one: the output of a run
    /// before goes. A command of more than one line, which the hook says on
    /// one, ends on its last, and a line that ends with a right prompt holds
    /// it too.
    #[test]
    fn the_command_s_last_run_is_found_over_its_lines() {
        let dump = "➜ cargo test\nfirst run\n\n➜ cargo test       12:00\nsecond run\n➜\n";
        assert_eq!(kept(dump, "cargo test"), Printed::After(vec!["second run"]));
        let dump = "~> begin\n       echo one\n       echo two\n   end\none\ntwo\n~>\n";
        assert_eq!(kept(dump, "begin echo one echo two end"), Printed::After(vec!["one", "two"]));
    }

    /// The command alone on a line, with no prompt in front of it, is not
    /// where it was run, nor is a word that ends with it.
    #[test]
    fn the_command_s_line_has_a_prompt_in_front_of_it() {
        let dump = "➜ ls\nls\ntools\n➜\n";
        assert_eq!(kept(dump, "ls"), Printed::After(vec!["ls", "tools"]));
    }

    /// Nothing after the first line the pane holds, the command's, is
    /// nothing printed. Nothing after a line below others is not: that line
    /// may be one the command printed, which holds it after a word, as
    /// `Error: make failed` holds `make`, and all the pane holds is kept with
    /// it, for the agent to tell.
    #[test]
    fn nothing_after_the_command_s_line_is_nothing_printed_only_with_nothing_above() {
        assert_eq!(kept("➜ true\n➜\n\n", "true"), Printed::Nothing);
        assert_eq!(kept("\n\n➜ true\n\n➜\n", "true"), Printed::Nothing);
        assert_eq!(kept("out\n➜ true\n➜\n", "true"), Printed::Unsure(vec!["out", "➜ true"]));
        let dump = "➜ make\ncc -o a a.c\na.c:1: error\nError: make failed\n➜\n";
        let screen = vec!["➜ make", "cc -o a a.c", "a.c:1: error", "Error: make failed"];
        assert_eq!(kept(dump, "make"), Printed::Unsure(screen));
    }

    /// With no line that holds the command, as after a `clear`, all the pane
    /// holds up to the prompt is kept, and says so. A prompt of more lines
    /// than one leaves those before the one the shell waits on.
    #[test]
    fn without_the_command_s_line_all_the_pane_holds_is_kept() {
        assert_eq!(kept("out\nmore\n➜\n", "make"), Printed::Top(Some(vec!["out", "more"])));
        assert_eq!(kept("\n➜\n", "make"), Printed::Top(None));
        let dump = "\nluneta on main\n➜ make\nbuilt\n\nluneta on main took 3s\n➜\n";
        let built = vec!["built", "", "luneta on main took 3s"];
        assert_eq!(kept(dump, "make"), Printed::After(built));
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
        assert_eq!(kept(dump, typed(&resumed)), Printed::After(back));
        let numbered = ended("fg %1", Some(Ending::Resumed));
        assert_eq!(kept("➜ fg %1\nerror\n➜\n", typed(&numbered)), Printed::After(vec!["error"]));
    }

    /// Of a long output, the last `TAIL` lines are kept, and the rest counted.
    #[test]
    fn a_long_output_keeps_its_last_lines() {
        let output: Vec<String> = (1..=TAIL + 5).map(|n| format!("line {n}")).collect();
        let dump = format!("➜ seq\n{}\n➜\n", output.join("\n"));
        let Printed::After(Tail { lines, left_out }) = cut(&dump, "seq") else {
            panic!("no output");
        };
        assert_eq!((lines.len(), left_out), (TAIL, 5));
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
        assert_eq!(keep(&runtime, Printed::Nothing, now), Ok(Printed::Nothing));
        assert_eq!(keep(&runtime, Printed::Top(None), now), Ok(Printed::Top(None)));
        assert!(!runtime.exists());

        let cut = Printed::Top(Some(Tail { lines: vec!["one", "two"], left_out: 3 }));
        let first = keep(&runtime, cut.clone(), now).expect("kept");
        let folder = fs::canonicalize(runtime.join(FOLDER)).expect("the folder");
        let Printed::Top(Some(Kept { file, lines: 2, left_out: 3 })) = &first else {
            panic!("kept {first:?}");
        };
        let file = Path::new(file);
        assert_eq!(file.parent(), Some(folder.as_path()));
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
        let second = keep(&runtime, cut, now).expect("kept");
        assert_ne!(second.file(), first.file());
        assert!(!old.exists());
        assert!(young.exists() && file.exists());
    }

    /// A file whose path holds what a rule of Claude Code's reads as a
    /// pattern, as gitignore does, or as where the rule ends, is one no rule
    /// would let the agent read, nor that one alone: luneta keeps none there.
    #[test]
    fn a_file_no_rule_names_alone_is_not_kept() {
        let root = tempfile::tempdir().expect("a folder");
        let tail = || Printed::After(Tail { lines: vec!["error"], left_out: 0 });
        for named in ["lune[ta", "lune*ta", "lune(ta)"] {
            let runtime = root.path().join(named).join("luneta");
            fs::create_dir_all(runtime.parent().expect("a parent")).expect("made");
            let refused = keep(&runtime, tail(), SystemTime::now()).expect_err("refused");
            let says = "`, which a rule of Claude Code's reads as more than a path";
            assert!(refused.ends_with(says), "{refused}");
            let kept = fs::read_dir(runtime.join(FOLDER)).expect("the folder").count();
            assert_eq!(kept, 0, "{named}");
        }
    }

    /// The agent is told what the command was and how it ended, and where
    /// what it printed is, or that it printed nothing, and how much of it
    /// that is; or, where luneta is not sure where it starts, or found no
    /// line it was run on, where all the screen is.
    #[test]
    fn the_agent_is_told_the_command_how_it_ended_and_where_what_it_printed_is() {
        let file = "/run/luneta/printed/1.txt".to_owned();
        let kept = |lines, left_out| Kept { file: file.clone(), lines, left_out };
        let failed = ended("cargo test", Some(Ending::Exited(101)));
        let told = |printed: Printed| printed.prompt("Fix this.", &failed);
        let ran = "Fix this. In my shell, `cargo test` ended with exit status 101.";
        let gone = "My screen no longer shows the line I ran it on";
        assert_eq!(
            told(Printed::After(kept(2, 0))),
            format!("{ran} What it printed is in /run/luneta/printed/1.txt.")
        );
        assert_eq!(
            told(Printed::After(kept(2000, 7))),
            format!(
                "{ran} The last 2000 of the 2007 lines it printed are in \
                 /run/luneta/printed/1.txt."
            )
        );
        assert_eq!(told(Printed::Nothing), format!("{ran} It printed nothing."));
        assert_eq!(
            told(Printed::Unsure(kept(4, 0))),
            format!(
                "{ran} Nothing follows the last line on my screen that looks like the line I ran \
                 it on, which may be one it printed, so /run/luneta/printed/1.txt holds all my \
                 screen has."
            )
        );
        assert_eq!(
            told(Printed::Top(Some(kept(2, 0)))),
            format!(
                "{ran} {gone}, so /run/luneta/printed/1.txt holds all my screen has, which may \
                 begin before what it printed."
            )
        );
        assert_eq!(
            told(Printed::Top(Some(kept(2000, 1)))),
            format!(
                "{ran} {gone}, so /run/luneta/printed/1.txt holds the last 2000 of the 2001 lines \
                 my screen has, which may begin before what it printed."
            )
        );
        assert_eq!(told(Printed::Top(None)), format!("{ran} {gone}, nor anything else."));
        assert_eq!(
            Printed::Nothing.prompt("Explain this.", &ended("vim", None)),
            "Explain this. In my shell, `vim` ended, with an exit status my shell cannot say. It \
             printed nothing."
        );

        // A job `fg` brought back printed after it.
        let resumed = ended("cargo build", Some(Ending::Resumed));
        let told = |printed: Printed| printed.prompt("Fix this.", &resumed);
        let ran = "Fix this. In my shell, `cargo build` ended once I brought it back with `fg`, \
                   with an exit status my shell cannot say.";
        assert_eq!(
            told(Printed::After(kept(2, 0))),
            format!("{ran} What it printed after `fg` is in /run/luneta/printed/1.txt.")
        );
        assert_eq!(
            told(Printed::After(kept(2000, 7))),
            format!(
                "{ran} The last 2000 of the 2007 lines it printed after `fg` are in \
                 /run/luneta/printed/1.txt."
            )
        );
        assert_eq!(told(Printed::Nothing), format!("{ran} It printed nothing after `fg`."));
        assert_eq!(
            told(Printed::Top(None)),
            format!(
                "{ran} My screen no longer shows the `fg` I brought it back with, nor anything \
                 else."
            )
        );
    }
}
