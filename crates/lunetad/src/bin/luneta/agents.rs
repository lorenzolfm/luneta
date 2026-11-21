use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use lunetad::api;
use lunetad::projects::Argv;
pub use lunetad::Harness;

use notify::RecommendedWatcher;

use crate::host::Event;
use crate::state;

const FILE: &str = "agent";

pub const fn name(harness: Harness) -> &'static str {
    harness.program()
}

pub fn named(program: &str) -> Option<Harness> {
    match program {
        "claude" => Some(Harness::ClaudeCode),
        "pi" => Some(Harness::Pi),
        _ => None,
    }
}

pub const fn next(harness: Harness) -> Harness {
    match harness {
        Harness::ClaudeCode => Harness::Pi,
        Harness::Pi => Harness::ClaudeCode,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    ByView(Harness),
    ByYou(Harness),
}

impl Picked {
    pub const fn harness(self) -> Harness {
        match self {
            Picked::ByView(harness) | Picked::ByYou(harness) => harness,
        }
    }

    pub const fn next(self) -> Picked {
        Picked::ByYou(next(self.harness()))
    }
}

pub fn spelled(harness: Harness, skill: &str) -> String {
    match harness {
        Harness::ClaudeCode => format!("/{skill}"),
        Harness::Pi => format!("/skill:{skill}"),
    }
}

pub fn of(agent: &api::Agent) -> Option<Harness> {
    match agent.runs_in() {
        api::Harness::ClaudeCode => Some(Harness::ClaudeCode),
        api::Harness::Pi => Some(Harness::Pi),
        api::Harness::Unspecified => None,
    }
}

pub fn on_skill(harness: Harness, skill: &str, given: Option<&str>) -> Result<Argv, String> {
    let spelled = spelled(harness, skill);
    let prompt = match given {
        Some(given) => format!("{spelled} {given}"),
        None => spelled,
    };
    Argv::parse(vec![name(harness).to_owned(), prompt])
}

pub fn on_words(harness: Harness, words: String, files: &[&str]) -> Result<Argv, String> {
    let mut command = vec![name(harness).to_owned()];
    if harness == Harness::ClaudeCode {
        let rules: Vec<String> = files.iter().filter_map(|file| read_rule(file)).collect();
        if !rules.is_empty() {
            command.push("--allowedTools".to_owned());
            command.extend(rules);
            command.push("--".to_owned());
        }
    }
    command.push(words);
    Argv::parse(command)
}

fn read_rule(file: &str) -> Option<String> {
    let mut rule = String::from("Read(/");
    let mut inside = true;
    for c in file.chars() {
        match c {
            '?' => return None,
            ' ' | ',' if !inside => return None,
            c if c.is_control() => return None,
            '(' => inside = true,
            ')' => inside = false,
            '*' | '[' | ']' => rule.push('\\'),
            '\\' => rule.push_str(r"\\\"),
            _ => {},
        }
        rule.push(c);
    }
    rule.push(')');
    Some(rule)
}

#[derive(Debug)]
pub struct Kept {
    harness: Harness,
    file: Option<PathBuf>,
}

impl Default for Kept {
    fn default() -> Self {
        Kept { harness: Harness::ClaudeCode, file: None }
    }
}

impl Kept {
    pub fn shared() -> (Self, Option<String>) {
        match state::path(FILE) {
            Ok(file) => Kept::within(file),
            Err(unset) => (Kept::default(), Some(unset.to_string())),
        }
    }

    pub fn within(file: PathBuf) -> (Self, Option<String>) {
        if let Err(error) = file.parent().map_or(Ok(()), fs::create_dir_all) {
            return (Kept::default(), Some(format!("{}: {error}", file.display())));
        }
        let mut kept = Kept { file: Some(file), ..Kept::default() };
        let unread = kept.reload().err();
        (kept, unread)
    }

    pub fn harness(&self) -> Harness {
        self.harness
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    pub fn reload(&mut self) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("{}: {error}", file.display())),
        };
        let program = text.trim();
        match named(program) {
            Some(harness) => {
                self.harness = harness;
                Ok(())
            },
            None => Err(format!("{}: {program:?} is no harness luneta knows", file.display())),
        }
    }

    pub fn keep(&mut self, harness: Harness) -> Result<(), String> {
        self.harness = harness;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let folder = file.parent().expect("a file of `state::path` is in a folder");
        let unfinished = folder.join(format!("{FILE}.{}", state::unfinished()));
        let written = fs::create_dir_all(folder)
            .and_then(|()| fs::write(&unfinished, format!("{}\n", name(harness))))
            .and_then(|()| fs::rename(&unfinished, file));
        if written.is_err() {
            let _ = fs::remove_file(&unfinished);
        }
        written.map_err(|error| format!("{}: {error}", file.display()))
    }
}

pub fn watch(file: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch_file(file, events, || Event::Agent)
}

#[cfg(test)]
mod tests {
    use lunetad::projects::LINE_BYTES_MAX;

    use super::*;

    #[test]
    fn a_skill_starts_an_unnamed_agent_in_its_harness_s_spelling() {
        assert_eq!(
            on_skill(Harness::ClaudeCode, "tigerstyle", None).unwrap().words(),
            ["claude", "/tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::ClaudeCode, "code-review", Some("since main")).unwrap().words(),
            ["claude", "/code-review since main"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "tigerstyle", None).unwrap().words(),
            ["pi", "/skill:tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "code-review", Some("since main")).unwrap().words(),
            ["pi", "/skill:code-review since main"]
        );
    }

    #[test]
    fn an_agent_is_let_read_the_file_by_a_rule_that_names_it_alone() {
        for (file, rule) in [
            ("/w/plan.md", "Read(//w/plan.md)"),
            ("/w/My Plan (v2).md", "Read(//w/My Plan (v2).md)"),
            ("/w/a, b.md", "Read(//w/a, b.md)"),
            ("/w/b{a,c}.md", "Read(//w/b{a,c}.md)"),
            ("/w/[draft] a*.md", r"Read(//w/\[draft\] a\*.md)"),
            (r"/w/back\slash.md", r"Read(//w/back\\\\slash.md)"),
        ] {
            let prompt = format!("Tackle {file}");
            let command = ["claude", "--allowedTools", rule, "--", prompt.as_str()];
            assert_eq!(
                on_words(Harness::ClaudeCode, prompt.clone(), &[file]).unwrap().words(),
                command.map(str::to_owned).to_vec(),
                "{file}"
            );
            assert_eq!(
                on_words(Harness::Pi, prompt.clone(), &[file]).unwrap().words(),
                ["pi".to_owned(), prompt],
                "{file}"
            );
        }
        for file in [
            "/w/what?.md",
            "/w/Plan (v2) final.md",
            "/w/Plan (v2), final.md",
            "/w/two\nlines.md",
        ] {
            let prompt = format!("Tackle {file}");
            assert_eq!(
                on_words(Harness::ClaudeCode, prompt.clone(), &[file]).unwrap().words(),
                ["claude".to_owned(), prompt],
                "{file:?}"
            );
        }
    }

    #[test]
    fn an_agent_is_refused_words_that_no_command_line_holds() {
        let refused = Err(format!("a command line is longer than {LINE_BYTES_MAX} bytes"));
        assert_eq!(on_words(Harness::Pi, "x".repeat(LINE_BYTES_MAX), &[]), refused);
        assert_eq!(on_skill(Harness::Pi, "tdd", Some(&"x".repeat(LINE_BYTES_MAX))), refused);
    }

    #[test]
    fn a_harness_is_known_by_its_program_s_name() {
        for harness in [Harness::ClaudeCode, Harness::Pi] {
            assert_eq!(named(name(harness)), Some(harness));
        }
        assert_eq!(named("codex"), None);
    }

    #[test]
    fn the_harness_picked_is_kept_for_every_luneta() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join(FILE);
        let (mut one, unread) = Kept::within(file.clone());
        assert_eq!((one.harness(), unread), (Harness::ClaudeCode, None));
        let (mut two, _) = Kept::within(file.clone());

        one.keep(Harness::Pi).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "pi\n");
        assert_eq!(Kept::within(file.clone()).0.harness(), Harness::Pi);
        two.reload().unwrap();
        assert_eq!(two.harness(), Harness::Pi);
    }

    #[test]
    fn a_file_that_names_no_harness_says_why() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(FILE);
        fs::write(&file, "codex\n").unwrap();
        let (mut kept, unread) = Kept::within(file.clone());
        assert_eq!(kept.harness(), Harness::ClaudeCode);
        assert!(unread.is_some_and(|why| why.contains("\"codex\" is no harness")));
        kept.keep(Harness::Pi).unwrap();
        assert_eq!(Kept::within(file).0.harness(), Harness::Pi);

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let (mut kept, unread) = Kept::within(blocker.join(FILE));
        assert!(unread.is_some_and(|why| why.contains("not a folder")));
        assert_eq!(kept.file(), None);
        assert_eq!(kept.keep(Harness::Pi), Ok(()));
        assert_eq!(kept.harness(), Harness::Pi);
    }
}
