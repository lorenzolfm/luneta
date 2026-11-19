use std::num::{NonZeroU32, NonZeroUsize};

use unicode_width::UnicodeWidthStr;

use crate::sidebar::cut;

const SHOWN_CHARS_MAX: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Copied {
    Pull(NonZeroU32),
    Text(Text),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: NonZeroUsize,
    first: String,
}

impl Copied {
    pub fn text(text: &str) -> Copied {
        Copied::Text(Text::of(text))
    }

    pub fn said(&self, room: usize) -> String {
        match self {
            Copied::Pull(number) => format!("copied #{number}"),
            Copied::Text(text) => text.said(room),
        }
    }
}

impl Text {
    fn of(text: &str) -> Text {
        assert!(!text.is_empty(), "a sweep, a selection and a link each copy something");
        let lines = NonZeroUsize::new(text.lines().count()).expect("a text with something has a line");
        let first = text.lines().map(shown).find(|line| !line.is_empty()).unwrap_or_default();
        assert!(first.chars().count() <= SHOWN_CHARS_MAX, "a note keeps only what it can show");
        assert!(!first.chars().any(char::is_control), "a note draws no control character");
        Text { lines, first }
    }

    fn said(&self, room: usize) -> String {
        let lines = self.lines.get();
        let counted = match (lines, self.first.is_empty()) {
            (1, false) => "copied".to_owned(),
            (1, true) => "copied 1 blank line".to_owned(),
            (_, false) => format!("copied {lines} lines"),
            (_, true) => format!("copied {lines} blank lines"),
        };
        if self.first.is_empty() {
            return counted;
        }
        let quoted = |kept: &str| format!("{counted} \"{kept}\"");
        let Some(left) = room.checked_sub(quoted("").width()) else {
            return counted;
        };
        let kept = cut(&self.first, left);
        if quotes_some(&kept, &self.first) {
            let said = quoted(&kept);
            assert!(said.width() <= room, "a quote is cut to the room it has");
            said
        } else {
            counted
        }
    }
}

fn quotes_some(kept: &str, first: &str) -> bool {
    if kept == first {
        return true;
    }
    kept.strip_suffix('…').is_some_and(|before| !before.is_empty())
}

fn shown(line: &str) -> String {
    let spaced: String = line.chars().map(|it| if it.is_control() { ' ' } else { it }).collect();
    spaced.trim().chars().take(SHOWN_CHARS_MAX).collect()
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
    fn a_line_of_one_character_is_said_whole_where_it_fits() {
        let copied = Copied::text("a");
        assert_eq!(copied.said(10), "copied \"a\"");
        assert_eq!(copied.said(9), "copied");
    }

    #[test]
    fn more_lines_are_counted_and_the_first_with_words_is_said() {
        let copied = Copied::text("\n   \n  fn main() {\n      run();\n  }\n");
        assert_eq!(copied, Copied::Text(Text {
            lines: NonZeroUsize::new(5).unwrap(),
            first: "fn main() {".to_owned()
        }));
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
        let long = "a".repeat(SHOWN_CHARS_MAX * 2);
        let Copied::Text(Text { first, .. }) = Copied::text(&long) else {
            panic!("a text is copied as text");
        };
        assert_eq!(first, "a".repeat(SHOWN_CHARS_MAX));
    }

    #[test]
    fn a_wide_line_is_cut_by_the_columns_it_takes() {
        let copied = Copied::text("日本語のテキスト");
        assert_eq!(copied.said(20), "copied \"日本語のテ…\"");
        assert!(copied.said(20).width() <= 20);
    }

    #[test]
    fn a_wide_character_with_no_room_beside_its_ellipsis_leaves_only_the_count() {
        let copied = Copied::text("日本語のテキスト");
        assert_eq!(copied.said(11), "copied");
        assert_eq!(copied.said(12), "copied \"日…\"");
    }

    #[test]
    fn a_quote_never_runs_past_its_room() {
        let lines = [
            "\u{200b}",
            "\u{fe0f}x",
            "e\u{301}e\u{301}e\u{301}",
            "❤\u{fe0f} red",
            "👨\u{200d}👩\u{200d}👧 family",
            "لا لا لا",
            "ｶﾞｶﾞｶﾞ",
            "日本語のテキスト",
        ];
        for line in lines {
            let copied = Copied::text(line);
            for room in 0..40 {
                let said = copied.said(room);
                if said.contains('"') {
                    assert!(said.width() <= room, "{said:?} is past {room}");
                }
            }
        }
    }
}
