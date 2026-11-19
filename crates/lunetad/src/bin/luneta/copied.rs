use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::ops::Deref;

use base64::Engine as _;
use lunetad::links::{Kind, Link};

use crate::sidebar::cut;
use crate::sweep::cells;

const SHOWN_CHARS_MAX: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excerpt(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clip {
    Pull(Link),
    Filed(Link),
    Excerpt(Excerpt),
}

#[derive(Debug, Default)]
pub struct Copies {
    excerpt: Option<Excerpt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Copied {
    Pull(NonZeroU32),
    Text(Text),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: NonZeroUsize,
    first: Option<String>,
}

impl Excerpt {
    pub fn of(text: String) -> Option<Excerpt> {
        (!text.is_empty()).then_some(Excerpt(text))
    }
}

impl Deref for Excerpt {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl Copies {
    pub fn clip(&mut self, out: &mut impl io::Write, clip: Clip) -> io::Result<Option<Copied>> {
        match clip {
            Clip::Pull(link) => {
                assert_eq!(link.kind(), Kind::Pull, "a row keeps only its pull request's link");
                write(out, &link.url())?;
                Ok(Some(Copied::Pull(link.number())))
            },
            Clip::Filed(link) => {
                assert_eq!(link.kind(), Kind::Issue, "an issue is filed only with an issue's link");
                write(out, &link.url())?;
                Ok(None)
            },
            Clip::Excerpt(excerpt) => {
                write(out, &excerpt)?;
                let copied = Copied::text(&excerpt);
                self.excerpt = Some(excerpt);
                Ok(Some(copied))
            },
        }
    }

    pub fn excerpt(&self) -> Option<&str> {
        self.excerpt.as_deref()
    }
}

fn write(out: &mut impl io::Write, text: &str) -> io::Result<()> {
    out.write_all(osc52(text).as_bytes())?;
    out.flush()
}

fn osc52(text: &str) -> String {
    let text = base64::engine::general_purpose::STANDARD.encode(text);
    let spelled = |byte: u8| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte);
    assert!(text.bytes().all(spelled), "a copy can't end its escape early");
    format!("\x1b]52;c;{text}\x07")
}

impl Copied {
    pub fn text(excerpt: &Excerpt) -> Copied {
        Copied::Text(Text::of(excerpt))
    }

    pub fn said(&self, room: usize) -> String {
        match self {
            Copied::Pull(number) => format!("copied #{number}"),
            Copied::Text(text) => text.said(room),
        }
    }
}

impl Text {
    fn of(excerpt: &Excerpt) -> Text {
        let lines = NonZeroUsize::new(excerpt.lines().count()).expect("an excerpt has a line");
        let first = excerpt.lines().map(shown).find(|line| !line.is_empty());
        let fits = |first: &str| first.chars().count() <= SHOWN_CHARS_MAX;
        assert!(first.as_deref().is_none_or(fits), "a note keeps only what it can show");
        let drawn = |first: &str| !first.chars().any(char::is_control);
        assert!(first.as_deref().is_none_or(drawn), "a note draws no control character");
        Text { lines, first }
    }

    fn said(&self, room: usize) -> String {
        let lines = self.lines.get();
        let Some(first) = &self.first else {
            return match lines {
                1 => "copied 1 blank line".to_owned(),
                _ => format!("copied {lines} blank lines"),
            };
        };
        let counted = match lines {
            1 => "copied".to_owned(),
            _ => format!("copied {lines} lines"),
        };
        let quoted = |kept: &str| format!("{counted} \"{kept}\"");
        let Some(left) = room.checked_sub(cells(&quoted(""))) else {
            return counted;
        };
        let kept = cut(first, left);
        if quotes_some(&kept, first) {
            let said = quoted(&kept);
            assert!(cells(&said) <= room, "a quote is cut to the cells it has");
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
    use ratatui::style::Style;

    use super::*;
    use crate::sidebar::wrapped;

    fn excerpt(text: &str) -> Excerpt {
        Excerpt::of(text.to_owned()).expect("a test excerpts some text")
    }

    fn noted(text: &str) -> Copied {
        Copied::text(&excerpt(text))
    }

    fn link(kind: &str) -> Link {
        Link::parse(&format!("https://github.com/o/r/{kind}/246")).expect("a link on github.com")
    }

    #[test]
    fn what_to_copy_goes_out_as_osc_52() {
        let mut out = Vec::new();
        let mut copies = Copies::default();
        let note = copies.clip(&mut out, Clip::Pull(link("pull"))).unwrap();
        assert_eq!(note, Some(Copied::Pull(NonZeroU32::new(246).unwrap())));
        let spelled = "aHR0cHM6Ly9naXRodWIuY29tL28vci9wdWxsLzI0Ng==";
        assert_eq!(String::from_utf8(out).unwrap(), format!("\x1b]52;c;{spelled}\x07"));
    }

    #[test]
    fn alt_u_reads_only_an_excerpt_of_the_screen() {
        let mut copies = Copies::default();
        let mut out = Vec::new();
        let pull = Some(Copied::Pull(NonZeroU32::new(246).unwrap()));
        let mut clip = |copies: &mut Copies, clip| copies.clip(&mut out, clip).unwrap();
        assert_eq!(clip(&mut copies, Clip::Pull(link("pull"))), pull);
        assert_eq!(copies.excerpt(), None, "a link luneta made is not read");
        assert_eq!(clip(&mut copies, Clip::Excerpt(excerpt("swept"))), Some(noted("swept")));
        assert_eq!(copies.excerpt(), Some("swept"));
        assert_eq!(clip(&mut copies, Clip::Pull(link("pull"))), pull);
        assert_eq!(copies.excerpt(), Some("swept"), "a pull request's link leaves it");
        let filed = clip(&mut copies, Clip::Filed(link("issues")));
        assert_eq!(filed, None, "a filed issue says so in its own note");
        assert_eq!(copies.excerpt(), Some("swept"), "a filed issue's link leaves it");
        let selected = clip(&mut copies, Clip::Excerpt(excerpt("selected")));
        assert_eq!(selected, Some(noted("selected")));
        assert_eq!(copies.excerpt(), Some("selected"), "the next excerpt replaces it");
        let written = String::from_utf8(out).unwrap();
        assert_eq!(written.matches("\x1b]52;c;").count(), 5, "every copy goes to the clipboard");
    }

    #[test]
    fn an_empty_text_is_no_excerpt() {
        assert_eq!(Excerpt::of(String::new()), None);
        assert_eq!(Excerpt::of(" ".to_owned()).as_deref(), Some(" "));
    }

    #[test]
    fn a_pull_request_link_is_said_by_its_number() {
        let copied = Copied::Pull(NonZeroU32::new(246).unwrap());
        assert_eq!(copied.said(30), "copied #246");
    }

    #[test]
    fn a_line_is_said_in_quotes_and_cut_to_fit() {
        let copied = noted("cargo nextest run");
        assert_eq!(copied.said(30), "copied \"cargo nextest run\"");
        assert_eq!(copied.said(26), "copied \"cargo nextest run\"");
        assert_eq!(copied.said(25), "copied \"cargo nextest r…\"");
        assert_eq!(cells(&copied.said(25)), 25);
        assert_eq!(copied.said(11), "copied \"c…\"");
        assert_eq!(copied.said(10), "copied");
        assert_eq!(copied.said(0), "copied");
    }

    #[test]
    fn a_line_of_one_character_is_said_whole_where_it_fits() {
        let copied = noted("a");
        assert_eq!(copied.said(10), "copied \"a\"");
        assert_eq!(copied.said(9), "copied");
    }

    #[test]
    fn more_lines_are_counted_and_the_first_with_words_is_said() {
        let copied = noted("\n   \n  fn main() {\n      run();\n  }\n");
        assert_eq!(copied, Copied::Text(Text {
            lines: NonZeroUsize::new(5).unwrap(),
            first: Some("fn main() {".to_owned())
        }));
        assert_eq!(copied.said(40), "copied 5 lines \"fn main() {\"");
        assert_eq!(copied.said(24), "copied 5 lines \"fn mai…\"");
        assert_eq!(copied.said(18), "copied 5 lines");
    }

    #[test]
    fn blanks_are_said_as_blank_lines() {
        assert_eq!(noted("   ").said(30), "copied 1 blank line");
        assert_eq!(noted(" \n\t\n").said(30), "copied 2 blank lines");
    }

    #[test]
    fn control_characters_are_said_as_spaces() {
        assert_eq!(noted("\x1b[1mbold\x07\tafter").said(40), "copied \"[1mbold  after\"");
    }

    #[test]
    fn a_long_line_keeps_only_what_a_note_can_show() {
        let long = "a".repeat(SHOWN_CHARS_MAX * 2);
        let Copied::Text(Text { first, .. }) = noted(&long) else {
            panic!("a text is copied as text");
        };
        assert_eq!(first, Some("a".repeat(SHOWN_CHARS_MAX)));
    }

    #[test]
    fn a_wide_line_is_cut_by_the_columns_it_takes() {
        let copied = noted("日本語のテキスト");
        assert_eq!(copied.said(20), "copied \"日本語のテ…\"");
        assert!(cells(&copied.said(20)) <= 20);
    }

    #[test]
    fn a_wide_character_with_no_room_beside_its_ellipsis_leaves_only_the_count() {
        let copied = noted("日本語のテキスト");
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
            "🏻\u{200d}🧑",
            "🇦🇦\u{200d}🏴\u{ad}",
            "ᨀ……🏴\u{200d}#\u{fe0f}",
        ];
        for line in lines {
            let copied = noted(line);
            for room in 0..40 {
                let said = copied.said(room);
                if said.contains('"') {
                    assert!(cells(&said) <= room, "{said:?} is past {room}");
                    let drawn = wrapped(&said, room + 2, Style::new());
                    assert_eq!(drawn.len(), 1, "{said:?} wraps in {room}");
                }
            }
        }
    }
}
