use std::cmp::Ordering;

pub const SCREEN_LINES_MAX: usize = 40;

pub const CHOICES_MAX: usize = 16;

const CURSOR: &str = "❯ ";

const CURSOR_WIDTH: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices {
    labels: Vec<String>,
    at: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moves {
    Up(usize),
    Down(usize),
    Stay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Words,
    Menu(Choices),
    TooMany(usize),
}

impl Seen {
    #[must_use]
    pub fn of(screen: &str) -> Seen {
        let lines: Vec<&str> = screen.lines().collect();
        let tail = &lines[lines.len().saturating_sub(SCREEN_LINES_MAX)..];
        assert!(tail.len() <= SCREEN_LINES_MAX, "{} lines looked at", tail.len());
        for (at, line) in tail.iter().enumerate().rev() {
            let Some((indent, chosen)) = cursor_on(line) else {
                continue;
            };
            let sibling = indent + CURSOR_WIDTH;
            let above: Vec<&str> =
                tail[..at].iter().rev().map_while(|line| at_indent(line, sibling)).collect();
            let below: Vec<&str> =
                tail[at + 1..].iter().map_while(|line| at_indent(line, sibling)).collect();
            if above.is_empty() && below.is_empty() {
                continue;
            }
            let shown = above.iter().rev().chain([&chosen]).chain(below.iter());
            let labels: Vec<String> =
                shown.enumerate().map(|(index, label)| unnumbered(index, label)).collect();
            assert_eq!(labels.len(), above.len() + 1 + below.len());
            if labels.len() > CHOICES_MAX {
                return Seen::TooMany(labels.len());
            }
            let choices = Choices { labels, at: above.len() };
            assert!(choices.labels.len() >= 2, "a menu offers a choice besides the cursor's");
            assert!(choices.at < choices.labels.len(), "the cursor is on one of the choices");
            return Seen::Menu(choices);
        }
        Seen::Words
    }
}

impl Choices {
    #[must_use]
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    #[must_use]
    pub fn at(&self) -> usize {
        self.at
    }

    pub fn moves_to(&self, index: usize, label: &str) -> Result<Moves, String> {
        assert!(self.at < self.labels.len(), "the cursor is on one of the choices");
        assert!(self.labels.len() <= CHOICES_MAX, "{} choices", self.labels.len());
        let Some(found) = self.labels.get(index) else {
            let shown = self.labels.len();
            return Err(format!("choice {} is asked for, and the screen shows {shown}", index + 1));
        };
        if found != label {
            return Err(format!("choice {} is now {found:?}, not {label:?}", index + 1));
        }
        let moves = match index.cmp(&self.at) {
            Ordering::Less => Moves::Up(self.at - index),
            Ordering::Greater => Moves::Down(index - self.at),
            Ordering::Equal => Moves::Stay,
        };
        if let Moves::Up(steps) | Moves::Down(steps) = moves {
            assert!(steps < self.labels.len(), "{steps} moves among {}", self.labels.len());
        }
        Ok(moves)
    }
}

fn cursor_on(line: &str) -> Option<(usize, &str)> {
    let shown = line.trim_start_matches(' ');
    let chosen = shown.strip_prefix(CURSOR)?.trim();
    (!chosen.is_empty()).then_some((line.len() - shown.len(), chosen))
}

fn at_indent(line: &str, indent: usize) -> Option<&str> {
    let shown = line.trim_start_matches(' ');
    let text = shown.trim_end();
    (line.len() - shown.len() == indent && !text.is_empty()).then_some(text)
}

fn unnumbered(index: usize, label: &str) -> String {
    let number = format!("{}. ", index + 1);
    label.strip_prefix(&number).unwrap_or(label).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERMISSION: &str = r#"
 ▐▛███▛█   Claude Code v2.1.330
▝▜██████▀  Opus 5.5 with high effort · Claude Ultra
 ▝▝   ▝▝   /tmp/claude-1000/permprobe/work


❯ Run exactly this bash command and nothing else: touch ./probe-file

● Bash(touch ./probe-file)
  ⎿  Waiting…

────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose "switch to auto mode" below
 Create empty probe file
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 touch ./probe-file
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /tmp/claude-1000/permprobe/work from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No

 Esc to cancel · Tab to amend
"#;

    const TRUST: &str = r"
────────────────────────────────────────
 Accessing workspace:

 /tmp/claude-1000/permprobe/work

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a
 well-known open source project, or work from your team). If not, take a moment to review what's in
 this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

   No, exit
 ❯ Yes, I trust this folder

 Enter to confirm · Esc to cancel
";

    const PROMPT: &str = r"
● I ran touch ./probe-file and nothing else.

✻ Sautéed for 8s · done 3:38 PM

────────────────────────────────────────
❯
────────────────────────────────────────
  Opus 5.5  ░░░░░░░░░░  4%  ctx
  ⏵⏵ auto mode on (shift+tab to cycle)
";

    fn labels(choices: &Choices) -> Vec<&str> {
        choices.labels().iter().map(String::as_str).collect()
    }

    fn menu(screen: &str) -> Choices {
        match Seen::of(screen) {
            Seen::Menu(choices) => choices,
            seen => panic!("{seen:?} is no menu"),
        }
    }

    #[test]
    fn a_permission_prompt_reads_as_its_numbered_choices() {
        let choices = menu(PERMISSION);
        let always = "Yes, and always allow access to /tmp/claude-1000/permprobe/work from this project";
        let auto = "Yes, and switch to auto mode · auto mode handles these prompts for you";
        assert_eq!(labels(&choices), ["Yes", always, auto, "No"]);
        assert_eq!(choices.at(), 0);
    }

    #[test]
    fn an_unnumbered_dialog_reads_with_its_cursor_where_it_is() {
        let choices = menu(TRUST);
        assert_eq!(labels(&choices), ["No, exit", "Yes, I trust this folder"]);
        assert_eq!(choices.at(), 1);
    }

    #[test]
    fn the_prompt_and_what_was_asked_are_no_choices() {
        assert_eq!(Seen::of(PROMPT), Seen::Words);
        let asked = "\n❯ Run exactly this bash command\n\n● Bash(touch ./probe-file)\n";
        assert_eq!(Seen::of(asked), Seen::Words);
    }

    #[test]
    fn a_menu_of_more_choices_than_are_read_is_seen_as_one() {
        let numbered = |count: usize| {
            let rest = (2..=count).map(|number| format!("   {number}. Option {number}\n"));
            format!(" Pick one\n ❯ 1. Option 1\n{}", rest.collect::<String>())
        };
        assert!(matches!(Seen::of(&numbered(CHOICES_MAX)), Seen::Menu(_)));
        assert_eq!(Seen::of(&numbered(CHOICES_MAX + 1)), Seen::TooMany(CHOICES_MAX + 1));
    }

    #[test]
    fn a_choice_is_reached_by_moving_from_the_cursor() {
        let choices = menu(PERMISSION);
        assert_eq!(choices.moves_to(3, "No"), Ok(Moves::Down(3)));
        assert_eq!(choices.moves_to(0, "Yes"), Ok(Moves::Stay));
        let trust = menu(TRUST);
        assert_eq!(trust.moves_to(0, "No, exit"), Ok(Moves::Up(1)));
    }

    #[test]
    fn a_choice_that_changed_since_it_was_shown_is_refused() {
        let choices = menu(PERMISSION);
        assert_eq!(choices.moves_to(1, "No").unwrap_err(), r#"choice 2 is now "Yes, and always allow access to /tmp/claude-1000/permprobe/work from this project", not "No""#);
        assert_eq!(choices.moves_to(4, "No").unwrap_err(), "choice 5 is asked for, and the screen shows 4");
    }

    #[test]
    fn only_the_last_screen_is_looked_at() {
        let old = format!("{TRUST}{}", "\n".repeat(SCREEN_LINES_MAX));
        assert_eq!(Seen::of(&old), Seen::Words);
    }

    #[test]
    fn the_cursor_is_as_wide_as_its_characters() {
        assert_eq!(CURSOR.chars().count(), CURSOR_WIDTH);
    }
}
