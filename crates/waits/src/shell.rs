macro_rules! prompt {
    () => {
        "sh-ready> "
    };
}

pub const PROMPT: &str = prompt!();

pub const SCRIPT: &str = concat!("export PS1='", prompt!(), "'; exec sh");

pub const AT_A_PROMPT: [&str; 3] = ["sh", "-c", SCRIPT];

const PROMPT_COLUMNS: usize = PROMPT.len();

const _: () = assert!(PROMPT.is_ascii(), "the prompt's columns are its bytes");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    rows: Vec<String>,
    cursor: Option<Cursor>,
}

impl Screen {
    pub fn new(rows: Vec<String>, cursor: Option<Cursor>) -> Result<Screen, String> {
        match cursor {
            Some(cursor) if cursor.row >= rows.len() => Err(format!(
                "the cursor at {cursor:?} is off the screen's {} rows",
                rows.len()
            )),
            _ => Ok(Screen { rows, cursor }),
        }
    }

    #[must_use]
    pub fn rows(&self) -> &[String] {
        &self.rows
    }

    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    fn prompt_row(&self) -> Option<usize> {
        let cursor = self.cursor?;
        let at_the_prompt = self.rows[cursor.row].trim_end() == PROMPT.trim_end();
        let after_the_prompt = cursor.column == PROMPT_COLUMNS;
        (at_the_prompt && after_the_prompt).then_some(cursor.row)
    }

    fn said(&self, line: &str) -> Option<&[String]> {
        let next = self.prompt_row()?;
        let typed = format!("{PROMPT}{line}");
        let typed = typed.trim_end();
        let at = self.rows[..next]
            .iter()
            .rposition(|row| row.trim_end() == typed)?;
        Some(&self.rows[at + 1..next])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompted(Screen);

#[must_use]
pub fn prompted(screen: &Screen) -> Option<Prompted> {
    screen.prompt_row().map(|_| Prompted(screen.clone()))
}

impl Prompted {
    #[must_use]
    pub fn typed(&self, line: &str) -> Typed {
        let twice = "a line run twice reads its first run, so wait on what this run prints";
        assert!(
            self.0.said(line).is_none(),
            "the screen at the prompt already says {line:?}: {twice}"
        );
        Typed {
            line: line.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typed {
    line: String,
}

impl Typed {
    #[must_use]
    pub fn line(&self) -> &str {
        &self.line
    }

    #[must_use]
    pub fn said<'a>(&self, now: &'a Screen) -> Option<&'a [String]> {
        now.said(&self.line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(texts: &[&str], cursor: Option<Cursor>) -> Screen {
        Screen::new(texts.iter().map(|&text| text.to_owned()).collect(), cursor).unwrap()
    }

    const fn at(row: usize, column: usize) -> Cursor {
        Cursor { row, column }
    }

    fn typed(line: &str) -> Typed {
        prompted(&screen(&["sh-ready>"], Some(at(0, PROMPT_COLUMNS))))
            .unwrap()
            .typed(line)
    }

    #[test]
    fn a_shell_is_prompted_once_its_prompt_holds_the_cursor_right_after_it() {
        let started = |cursor| screen(&["sh-ready>", ""], cursor);
        assert!(prompted(&started(Some(at(0, PROMPT_COLUMNS)))).is_some());
        assert!(
            prompted(&started(Some(at(1, 0)))).is_none(),
            "the cursor is past the prompt"
        );
        assert!(
            prompted(&started(None)).is_none(),
            "a screen with no cursor reads nothing"
        );
        assert!(
            prompted(&screen(&["", ""], Some(at(0, 0)))).is_none(),
            "a blank screen is no prompt"
        );
        let typing = screen(&["sh-ready> echo", ""], Some(at(0, PROMPT_COLUMNS + 4)));
        assert!(prompted(&typing).is_none(), "a line typed at the prompt");
    }

    #[test]
    fn a_cursor_off_the_screen_is_a_broken_screen_not_a_shell_yet_to_prompt() {
        let rows = vec!["sh-ready>".to_owned(), String::new()];
        let off = Screen::new(rows, Some(at(2, PROMPT_COLUMNS)));
        assert_eq!(
            off.unwrap_err(),
            "the cursor at Cursor { row: 2, column: 10 } is off the screen's 2 rows"
        );
    }

    #[test]
    fn a_line_says_the_rows_between_it_and_the_next_prompt() {
        let ran = screen(
            &[
                "sh-ready>",
                "sh-ready> echo a; echo b",
                "a",
                "b",
                "sh-ready>",
                "",
            ],
            Some(at(4, 10)),
        );
        assert_eq!(typed("echo a; echo b").said(&ran), Some(&ran.rows()[2..4]));
        let quiet = screen(
            &["sh-ready> true", "sh-ready>"],
            Some(at(1, PROMPT_COLUMNS)),
        );
        assert_eq!(typed("true").said(&quiet), Some(&[][..]));
    }

    #[test]
    fn a_line_said_before_the_next_prompt_has_said_nothing_yet() {
        let running = screen(&["sh-ready> echo a", "a", ""], Some(at(2, 0)));
        assert_eq!(typed("echo a").said(&running), None);
        let other = screen(
            &["sh-ready> echo b", "b", "sh-ready>"],
            Some(at(2, PROMPT_COLUMNS)),
        );
        assert_eq!(typed("echo a").said(&other), None);
    }

    #[test]
    fn a_line_typed_before_the_shell_read_it_is_never_said() {
        let typed_ahead = screen(
            &["echo right-$((40 + 2))", "sh-ready> right-42", "sh-ready>"],
            Some(at(2, 10)),
        );
        assert_eq!(typed("echo right-$((40 + 2))").said(&typed_ahead), None);
    }

    #[test]
    #[should_panic(expected = "the screen at the prompt already says \"echo a\"")]
    fn a_line_typed_again_would_read_its_first_run_so_typing_it_is_refused() {
        let ran = screen(
            &["sh-ready> echo a", "a", "sh-ready>"],
            Some(at(2, PROMPT_COLUMNS)),
        );
        let _ = prompted(&ran).unwrap().typed("echo a");
    }
}
