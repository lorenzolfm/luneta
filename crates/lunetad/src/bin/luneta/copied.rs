use std::num::{NonZeroU32, NonZeroUsize};

use unicode_width::UnicodeWidthStr;

use crate::sidebar::cut;

const SHOWN_MAX: usize = 512;

const QUOTES: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Copied {
    Pull(NonZeroU32),
    Text { lines: NonZeroUsize, first: String },
}

impl Copied {
    pub fn text(text: &str) -> Copied {
        let lines = NonZeroUsize::new(text.lines().count()).unwrap_or(NonZeroUsize::MIN);
        let first = text.lines().map(shown).find(|line| !line.is_empty()).unwrap_or_default();
        assert!(first.chars().count() <= SHOWN_MAX);
        Copied::Text { lines, first }
    }

    pub fn said(&self, room: usize) -> String {
        let (lines, first) = match self {
            Copied::Pull(number) => return format!("copied #{number}"),
            Copied::Text { lines, first } => (lines.get(), first),
        };
        let counted = match (lines, first.is_empty()) {
            (1, false) => "copied".to_owned(),
            (1, true) => "copied 1 blank line".to_owned(),
            (_, false) => format!("copied {lines} lines"),
            (_, true) => format!("copied {lines} blank lines"),
        };
        let left = room.saturating_sub(counted.width() + 1 + QUOTES);
        if first.is_empty() || left < 2 {
            return counted;
        }
        format!("{counted} \"{}\"", cut(first, left))
    }
}

fn shown(line: &str) -> String {
    let spaced: String = line.chars().map(|it| if it.is_control() { ' ' } else { it }).collect();
    spaced.trim().chars().take(SHOWN_MAX).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_request_link_is_said_by_its_number() {
        let copied = Copied::Pull(NonZeroU32::new(246).unwrap());
        assert_eq!(copied.said(30), "copied #246");
    }

    #[test]
    fn a_line_is_said_in_quotes_and_cut_to_fit() {
        let copied = Copied::text("cargo nextest run");
        assert_eq!(copied.said(30), "copied \"cargo nextest run\"");
        assert_eq!(copied.said(26), "copied \"cargo nextest run\"");
        assert_eq!(copied.said(25), "copied \"cargo nextest r…\"");
        assert_eq!(copied.said(25).width(), 25);
        assert_eq!(copied.said(11), "copied \"c…\"");
        assert_eq!(copied.said(10), "copied");
        assert_eq!(copied.said(0), "copied");
    }

    #[test]
    fn more_lines_are_counted_and_the_first_with_words_is_said() {
        let copied = Copied::text("\n   \n  fn main() {\n      run();\n  }\n");
        assert_eq!(copied, Copied::Text {
            lines: NonZeroUsize::new(5).unwrap(),
            first: "fn main() {".to_owned()
        });
        assert_eq!(copied.said(40), "copied 5 lines \"fn main() {\"");
        assert_eq!(copied.said(24), "copied 5 lines \"fn mai…\"");
        assert_eq!(copied.said(18), "copied 5 lines");
    }

    #[test]
    fn blanks_are_said_as_blank_lines() {
        assert_eq!(Copied::text("   ").said(30), "copied 1 blank line");
        assert_eq!(Copied::text(" \n\t\n").said(30), "copied 2 blank lines");
    }

    #[test]
    fn control_characters_are_said_as_spaces() {
        assert_eq!(Copied::text("\x1b[1mbold\x07\tafter").said(40), "copied \"[1mbold  after\"");
    }

    #[test]
    fn a_long_line_keeps_only_what_a_note_can_show() {
        let long = "a".repeat(SHOWN_MAX * 2);
        let Copied::Text { first, .. } = Copied::text(&long) else {
            panic!("a text is copied as text");
        };
        assert_eq!(first, "a".repeat(SHOWN_MAX));
    }

    #[test]
    fn a_wide_line_is_cut_by_the_columns_it_takes() {
        let copied = Copied::text("日本語のテキスト");
        assert_eq!(copied.said(20), "copied \"日本語のテ…\"");
        assert!(copied.said(20).width() <= 20);
    }
}
