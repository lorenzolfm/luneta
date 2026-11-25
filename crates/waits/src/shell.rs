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

#[must_use]
pub fn prompted(rows: &[String], cursor: Option<Cursor>) -> bool {
    cursor.is_some_and(|cursor| {
        let at_the_prompt = rows.get(cursor.row).is_some_and(|row| row.trim_end() == PROMPT.trim_end());
        at_the_prompt && cursor.column == PROMPT_COLUMNS
    })
}

#[must_use]
pub fn said<'a>(rows: &'a [String], cursor: Option<Cursor>, line: &str) -> Option<&'a [String]> {
    if !prompted(rows, cursor) {
        return None;
    }
    let next = cursor?.row;
    let typed = format!("{PROMPT}{line}");
    let typed = typed.trim_end();
    let at = rows[..next].iter().rposition(|row| row.trim_end() == typed)?;
    Some(&rows[at + 1..next])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|&text| text.to_owned()).collect()
    }

    const fn at(row: usize, column: usize) -> Cursor {
        Cursor { row, column }
    }

    #[test]
    fn a_shell_is_prompted_once_its_prompt_holds_the_cursor_right_after_it() {
        let started = rows(&["sh-ready>", ""]);
        assert!(prompted(&started, Some(at(0, PROMPT_COLUMNS))));
        assert!(!prompted(&started, Some(at(1, 0))), "the cursor is past the prompt");
        assert!(!prompted(&started, None), "a screen with no cursor reads nothing");
        assert!(!prompted(&rows(&["", ""]), Some(at(0, 0))), "a blank screen is no prompt");
        let typing = rows(&["sh-ready> echo", ""]);
        assert!(!prompted(&typing, Some(at(0, PROMPT_COLUMNS + 4))), "a line typed at the prompt");
    }

    #[test]
    fn a_line_says_the_rows_between_it_and_the_next_prompt() {
        let ran = rows(&["sh-ready>", "sh-ready> echo a; echo b", "a", "b", "sh-ready>", ""]);
        let said = said(&ran, Some(at(4, PROMPT_COLUMNS)), "echo a; echo b");
        assert_eq!(said, Some(&ran[2..4]));
        let quiet = rows(&["sh-ready> true", "sh-ready>"]);
        assert_eq!(super::said(&quiet, Some(at(1, PROMPT_COLUMNS)), "true"), Some(&[][..]));
    }

    #[test]
    fn a_line_said_before_the_next_prompt_has_said_nothing_yet() {
        let running = rows(&["sh-ready> echo a", "a", ""]);
        assert_eq!(said(&running, Some(at(2, 0)), "echo a"), None);
        let other = rows(&["sh-ready> echo b", "b", "sh-ready>"]);
        assert_eq!(said(&other, Some(at(2, PROMPT_COLUMNS)), "echo a"), None);
    }

    #[test]
    fn a_line_typed_before_the_shell_read_it_is_never_said() {
        let typed_ahead = rows(&["echo right-$((40 + 2))", "sh-ready> right-42", "sh-ready>"]);
        assert_eq!(said(&typed_ahead, Some(at(2, PROMPT_COLUMNS)), "echo right-$((40 + 2))"), None);
    }
}
