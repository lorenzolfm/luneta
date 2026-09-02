use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar;

const SCRIPT: &str = r#"set -e
f=$(mktemp)
trap 'rm -f "$f"' EXIT
zellij --session "$1" action dump-screen --pane-id "$2" --ansi --path "$f" >&2
i=0
while [ ! -s "$f" ] && [ "$i" -lt 20 ]; do sleep 0.05; i=$((i+1)); done
cat "$f""#;

pub const DUMP: [&str; 4] = ["sh", "-c", SCRIPT, "luneta"];

pub const CONTEXT_VALUE: &str = "pane";
pub const PANE_KEY: &str = "luneta_pane";

pub fn key(session: &str, pane: u32) -> String {
    format!("{}\t{}", pane, session)
}

pub fn parse_key(key: &str) -> Option<(String, u32)> {
    let (pane, session) = key.split_once('\t')?;
    Some((session.to_string(), pane.parse().ok()?))
}

pub fn pane_id(pane: u32) -> String {
    format!("terminal_{}", pane)
}

pub enum Peek {
    Reading,
    Ready(Vec<String>),
    Failed(String),
}

const MAX_LINES: usize = 64;

const MAX_PEEKS: usize = 32;

#[derive(Default)]
pub struct Peeks {
    screens: BTreeMap<(String, u32), Peek>,
}

impl Peeks {
    pub fn get(&self, session: &str, pane: u32) -> Option<&Peek> {
        self.screens.get(&(session.to_string(), pane))
    }

    pub fn claim(&mut self, session: &str, pane: u32) -> bool {
        let key = (session.to_string(), pane);
        if self.screens.contains_key(&key) {
            return false;
        }
        if self.screens.len() >= MAX_PEEKS {
            self.screens.clear();
        }
        self.screens.insert(key, Peek::Reading);
        true
    }

    pub fn ingest(
        &mut self,
        key: (String, u32),
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.screens.insert(
                key,
                Peek::Failed(match reason.is_empty() {
                    true => "the pane could not be read".to_string(),
                    false => reason.to_string(),
                }),
            );
            return;
        }
        self.screens.insert(key, Peek::Ready(trim(&String::from_utf8_lossy(stdout))));
    }

    pub fn forget(&mut self) {
        self.screens.clear();
    }
}

fn trim(dump: &str) -> Vec<String> {
    let mut lines: Vec<String> = dump.lines().map(sgr_only).collect();
    while lines.last().is_some_and(|line| columns(line) == 0) {
        lines.pop();
    }
    let blank = lines.iter().take_while(|line| columns(line) == 0).count();
    lines.drain(..blank);
    if lines.len() > MAX_LINES {
        lines.drain(..lines.len() - MAX_LINES);
    }
    lines
}

const ESC: char = '\u{1b}';

enum Part<'a> {
    Ch(char),
    Escape(&'a str),
}

/// Walks a dumped line as visible characters and escape sequences. Borrows the
/// line rather than copying it: a screenful of these runs on every frame.
struct Parts<'a> {
    line: &'a str,
    chars: std::str::CharIndices<'a>,
}

impl<'a> Iterator for Parts<'a> {
    type Item = Part<'a>;

    fn next(&mut self) -> Option<Part<'a>> {
        loop {
            let (at, ch) = self.chars.next()?;
            match ch {
                ESC => {
                    let start = at + ESC.len_utf8();
                    return Some(Part::Escape(escape(self.line, &mut self.chars, start)));
                },
                ch if ch.is_control() => continue,
                ch => return Some(Part::Ch(ch)),
            }
        }
    }
}

fn parts(line: &str) -> Parts<'_> {
    Parts { line, chars: line.char_indices() }
}

/// Consumes one escape sequence and returns the span that names it — for CSI
/// the whole sequence, for OSC and DCS just the introducer, whose payload is
/// swallowed and never reproduced.
fn escape<'a>(line: &'a str, chars: &mut std::str::CharIndices<'a>, start: usize) -> &'a str {
    let mut end = start;
    match chars.next() {
        Some((at, '[')) => {
            end = at + 1;
            for (at, ch) in chars.by_ref() {
                end = at + ch.len_utf8();
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    break;
                }
            }
        },
        Some((at, ch @ (']' | 'P' | 'X' | '^' | '_'))) => {
            end = at + ch.len_utf8();
            while let Some((_, ch)) = chars.next() {
                if ch == '\u{7}' {
                    break;
                }
                if ch == ESC {
                    chars.next();
                    break;
                }
            }
        },
        Some((at, ch)) => {
            end = at + ch.len_utf8();
            if ('\u{20}'..='\u{2f}').contains(&ch) {
                for (at, next) in chars.by_ref() {
                    end = at + next.len_utf8();
                    if !('\u{20}'..='\u{2f}').contains(&next) {
                        break;
                    }
                }
            }
        },
        None => {},
    }
    &line[start..end]
}

pub fn sgr_only(line: &str) -> String {
    let mut kept: Vec<Part> = parts(line)
        .filter(|part| match part {
            Part::Escape(seq) => seq.starts_with('[') && seq.ends_with('m'),
            Part::Ch(_) => true,
        })
        .collect();
    while kept.last().is_some_and(|part| matches!(part, Part::Ch(' '))) {
        kept.pop();
    }
    let mut out = String::with_capacity(line.len());
    for part in &kept {
        write(part, &mut out);
    }
    out
}

pub fn columns(line: &str) -> usize {
    parts(line).map(|part| width(&part)).sum()
}

pub fn fit(line: &str, max: usize) -> String {
    let mut out = String::with_capacity(line.len());
    if columns(line) <= max {
        for part in parts(line) {
            write(&part, &mut out);
        }
        return out;
    }
    let mut used = 0;
    let mut cut = false;
    for part in parts(line) {
        if !cut && used + width(&part) > max.saturating_sub(1) {
            cut = true;
            out.push('…');
        }
        if cut && matches!(part, Part::Ch(_)) {
            continue;
        }
        used += width(&part);
        write(&part, &mut out);
    }
    out
}

fn width(part: &Part) -> usize {
    match part {
        Part::Ch(ch) => ch.width().unwrap_or(0),
        Part::Escape(_) => 0,
    }
}

fn write(part: &Part, out: &mut String) {
    match part {
        Part::Ch(ch) => out.push(*ch),
        Part::Escape(seq) => {
            out.push(ESC);
            out.push_str(seq);
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(peeks: &Peeks) -> Vec<String> {
        match peeks.get("s", 0) {
            Some(Peek::Ready(lines)) => lines.clone(),
            _ => panic!("no screen"),
        }
    }

    fn dumped(stdout: &[u8]) -> Peeks {
        let mut peeks = Peeks::default();
        peeks.ingest(("s".to_string(), 0), Some(0), stdout, b"");
        peeks
    }

    #[test]
    fn a_dump_is_trimmed_to_what_is_on_the_screen() {
        let peeks = dumped(b"\n\n> cargo test\nok\n   \n\n\n");
        assert_eq!(ready(&peeks), ["> cargo test", "ok"]);
        assert_eq!(ready(&dumped(b"")), Vec::<String>::new());
        assert_eq!(ready(&dumped(b"\n\n\n")), Vec::<String>::new());
    }

    #[test]
    fn a_gap_in_the_middle_is_screen_and_not_padding() {
        assert_eq!(ready(&dumped(b"one\n\ntwo\n")), ["one", "", "two"]);
    }

    #[test]
    fn an_overlong_dump_keeps_its_last_lines() {
        let dump: String = (0..MAX_LINES * 2).map(|i| format!("line-{}\n", i)).collect();
        let lines = ready(&dumped(dump.as_bytes()));
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[MAX_LINES - 1], format!("line-{}", MAX_LINES * 2 - 1));
    }

    #[test]
    fn a_dump_keeps_its_colours_and_nothing_else() {
        assert_eq!(ready(&dumped(b"a\x1b[31mred\x07\tb")), ["a\x1b[31mredb"]);
    }

    #[test]
    fn an_escape_that_is_not_a_colour_is_dropped_whole() {
        assert_eq!(ready(&dumped(b"\x1b[9;9Ha\x1b[2Jb\x1b[1;5rc\x1b[?25ld")), ["abcd"]);
        assert_eq!(ready(&dumped(b"\x1b]0;title\x07a\x1b]2;more\x1b\\b")), ["ab"]);
        assert_eq!(ready(&dumped(b"a\x1bPztext;1,2\x1b\\b")), ["ab"]);
        assert_eq!(ready(&dumped(b"a\x1b7b\x1b(Bc")), ["abc"]);
    }

    #[test]
    fn a_coloured_line_is_measured_by_what_can_be_seen() {
        let line = "\x1b[31mred\x1b[m";
        assert_eq!(columns(line), 3);
        assert_eq!(columns("日本"), 4);
        assert_eq!(fit(line, 3), line);
    }

    #[test]
    fn a_cut_line_keeps_its_colours() {
        assert_eq!(fit("\x1b[31mredder\x1b[m", 4), "\x1b[31mred…\x1b[m");
        assert_eq!(columns(&fit("\x1b[31mredder\x1b[m", 4)), 4);
        assert_eq!(fit("日本語", 4), "日…");
    }

    #[test]
    fn a_pane_is_only_ever_asked_about_once() {
        let mut peeks = Peeks::default();
        assert!(peeks.claim("s", 0));
        assert!(!peeks.claim("s", 0));
        peeks.ingest(("s".to_string(), 0), Some(1), b"", b"zellij: no such session\n");
        assert!(!peeks.claim("s", 0));
        match peeks.get("s", 0) {
            Some(Peek::Failed(reason)) => assert_eq!(reason, "zellij: no such session"),
            _ => panic!("expected a failure"),
        }
        peeks.forget();
        assert!(peeks.claim("s", 0));
    }

    #[test]
    fn the_cache_names_a_pane_and_not_a_session() {
        let mut peeks = Peeks::default();
        assert!(peeks.claim("misc", 1));
        assert!(peeks.claim("misc", 2));
        assert!(peeks.claim("notes", 1));
        assert!(!peeks.claim("misc", 1));
        assert_eq!(pane_id(7), "terminal_7");
    }

    #[test]
    fn a_wire_key_round_trips_whatever_the_session_is_called() {
        assert_eq!(parse_key(&key("misc", 7)), Some(("misc".to_string(), 7)));
        assert_eq!(parse_key(&key("a\tb", 7)), Some(("a\tb".to_string(), 7)));
        assert_eq!(parse_key(&key("", 0)), Some((String::new(), 0)));
        assert_eq!(parse_key("misc"), None);
        assert_eq!(parse_key("seven\tmisc"), None);
    }
}
