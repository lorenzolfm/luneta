use std::path::Path;
use std::time::SystemTime;

use lune_protocol::said::Said;

use crate::mux::own::api::Dumped;
use crate::mux::own::server::Terminals;
use crate::mux::TerminalKey;
use crate::{folder, keeps};

const TAIL: usize = 2000;

pub(crate) const FOLDER: &str = "printed";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Printed<T = Kept> {
    After(T),
    Nothing,
    Unsure(T),
    Top(Option<T>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub file: String,
    pub lines: usize,
    pub left_out: usize,
}

impl<T> Printed<T> {
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
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        match self {
            Printed::After(kept) | Printed::Unsure(kept) | Printed::Top(Some(kept)) => {
                Some(&kept.file)
            },
            Printed::Nothing | Printed::Top(None) => None,
        }
    }
}

pub fn kept_in(terminal: TerminalKey, typed: &Said) -> Result<Printed, String> {
    kept(&Terminals.dumped(terminal, Dumped::Text)?, typed)
}

fn kept(dump: &str, typed: &Said) -> Result<Printed, String> {
    let runtime = folder::find().map_err(|unfound| format!("{unfound}, to keep it in"))?;
    keep(&runtime, cut(dump, typed.as_str()), SystemTime::now())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tail<'a> {
    lines: Vec<&'a str>,
    left_out: usize,
}

fn cut<'a>(dump: &'a str, typed: &str) -> Printed<Tail<'a>> {
    let mut lines: Vec<&str> = dump.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.pop();
    let Some(on) = ran_on(&lines, typed) else {
        return Printed::Top(tail(&lines));
    };
    if let Some(after) = tail(&lines[on + 1..]) {
        return Printed::After(after);
    }
    match tail(&lines[..=on]) {
        Some(screen) if screen.lines.len() + screen.left_out > 1 => Printed::Unsure(screen),
        _ => Printed::Nothing,
    }
}

fn tail<'a>(lines: &[&'a str]) -> Option<Tail<'a>> {
    let filled = |line: &&str| !line.trim().is_empty();
    let first = lines.iter().position(filled)?;
    let last = lines.iter().rposition(filled)?;
    let lines = &lines[first..=last];
    let left_out = lines.len().saturating_sub(TAIL);
    Some(Tail { lines: lines[left_out..].to_vec(), left_out })
}

fn ran_on(lines: &[&str], command: &str) -> Option<usize> {
    let command = spaced(command);
    let word = command.split(' ').next().filter(|word| !word.is_empty())?;
    let held = |at: &usize| lines[*at].contains(word);
    let ran = |at: usize| ends_on(&lines[at..], &command).map(|on| at + on);
    (0..lines.len()).rev().filter(held).find_map(ran)
}

fn ends_on(lines: &[&str], command: &str) -> Option<usize> {
    let first = spaced(lines.first()?);
    let mut text = first.clone();
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

fn spaced(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn keep(runtime: &Path, cut: Printed<Tail<'_>>, now: SystemTime) -> Result<Printed, String> {
    cut.try_map(|tail| write(runtime, &tail, now))
}

fn write(runtime: &Path, tail: &Tail<'_>, now: SystemTime) -> Result<Kept, String> {
    let Tail { lines, left_out } = tail;
    let mut draft = keeps::Draft::new(runtime, FOLDER, "txt", now)?;
    let mut text = lines.join("\n");
    text.push('\n');
    draft.write(text.as_bytes())?;
    Ok(Kept { file: draft.keep()?, lines: lines.len(), left_out: *left_out })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;

    fn kept<'a>(dump: &'a str, typed: &str) -> Printed<Vec<&'a str>> {
        let kept = cut(dump, typed).try_map(|tail| {
            assert_eq!(tail.left_out, 0);
            Ok::<_, ()>(tail.lines)
        });
        kept.unwrap()
    }

    #[test]
    fn what_a_command_printed_is_what_follows_its_line_up_to_the_prompt() {
        let dump = "Welcome to fish, the friendly interactive shell\n\
                    lorenzo@nixos ~/w/lune>\n\
                    lorenzo@nixos ~/w/lune> printf \"line %s\\n\" (seq 1 3); false\n\
                    line 1\n\
                    line 2\n\
                    line 3\n\
                    lorenzo@nixos ~/w/lune [1]>\n";
        let command = "printf \"line %s\\n\" (seq 1 3); false";
        assert_eq!(kept(dump, command), Printed::After(vec!["line 1", "line 2", "line 3"]));
    }

    #[test]
    fn the_command_s_last_run_is_found_over_its_lines() {
        let dump = "➜ cargo test\nfirst run\n\n➜ cargo test       12:00\nsecond run\n➜\n";
        assert_eq!(kept(dump, "cargo test"), Printed::After(vec!["second run"]));
        let dump = "~> begin\n       echo one\n       echo two\n   end\none\ntwo\n~>\n";
        assert_eq!(kept(dump, "begin echo one echo two end"), Printed::After(vec!["one", "two"]));
    }

    #[test]
    fn the_command_s_line_has_a_prompt_in_front_of_it() {
        let dump = "➜ ls\nls\ntools\n➜\n";
        assert_eq!(kept(dump, "ls"), Printed::After(vec!["ls", "tools"]));
        let dump = "➜ ls\nold\n➜ lsof -i\nnew\n➜\n";
        assert_eq!(kept(dump, "ls"), Printed::After(vec!["old", "➜ lsof -i", "new"]));
    }

    #[test]
    fn nothing_after_the_command_s_line_is_nothing_printed_only_with_nothing_above() {
        assert_eq!(kept("➜ true\n➜\n\n", "true"), Printed::Nothing);
        assert_eq!(kept("\n\n➜ true\n\n➜\n", "true"), Printed::Nothing);
        assert_eq!(kept("out\n➜ true\n➜\n", "true"), Printed::Unsure(vec!["out", "➜ true"]));
        let dump = "➜ make\ncc -o a a.c\na.c:1: error\nError: make failed\n➜\n";
        let screen = vec!["➜ make", "cc -o a a.c", "a.c:1: error", "Error: make failed"];
        assert_eq!(kept(dump, "make"), Printed::Unsure(screen));
    }

    #[test]
    fn without_the_command_s_line_all_the_view_holds_is_kept() {
        assert_eq!(kept("out\nmore\n➜\n", "make"), Printed::Top(Some(vec!["out", "more"])));
        assert_eq!(kept("\n➜\n", "make"), Printed::Top(None));
        let dump = "\nlune on main\n➜ make\nbuilt\n\nlune on main took 3s\n➜\n";
        let built = vec!["built", "", "lune on main took 3s"];
        assert_eq!(kept(dump, "make"), Printed::After(built));
    }

    #[test]
    fn a_job_brought_back_printed_after_its_fg() {
        let dump = "➜ cargo build\n   Compiling foo\n^Z\nfish: Job 1, 'cargo build' has stopped\n\
                    ➜ git status\nOn branch main\n➜ fg\nSend job 1 'cargo build' to foreground\n\
                    error[E0308]: mismatched types\n➜\n";
        let back = vec!["Send job 1 'cargo build' to foreground", "error[E0308]: mismatched types"];
        assert_eq!(kept(dump, "fg"), Printed::After(back));
        assert_eq!(kept("➜ fg %1\nerror\n➜\n", "fg"), Printed::After(vec!["error"]));
    }

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

    #[test]
    fn what_a_view_printed_is_kept_in_a_file_of_this_user_s_alone() {
        let root = tempfile::tempdir().expect("a folder");
        let runtime = root.path().join("lune");
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

    #[test]
    fn a_file_no_rule_names_alone_is_not_kept() {
        let root = tempfile::tempdir().expect("a folder");
        let tail = || Printed::After(Tail { lines: vec!["error"], left_out: 0 });
        for named in ["lune[ta", "lune*ta", "lune(ta)"] {
            let runtime = root.path().join(named).join("lune");
            fs::create_dir_all(runtime.parent().expect("a parent")).expect("made");
            let refused = keep(&runtime, tail(), SystemTime::now()).expect_err("refused");
            let says = "`, which a rule of Claude Code's reads as more than a path";
            assert!(refused.ends_with(says), "{refused}");
            let kept = fs::read_dir(runtime.join(FOLDER)).expect("the folder").count();
            assert_eq!(kept, 0, "{named}");
        }
    }
}
