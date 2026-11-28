use std::collections::VecDeque;

use libghostty_vt::terminal::{Boundary, StringEnd};
use libghostty_vt::{Error, Terminal};
use lune_protocol::program_resume::{
    BODY_BYTES_MAX as RESUME_BODY_BYTES_MAX, OSC_PREFIX as RESUME,
};
use percent_encoding::percent_decode;

pub const RESUME_REPORTS_PER_TURN_MAX: usize = 64;

const MARK_BYTES_MAX: usize = 16 * 1024;

const RESUME_BYTES_MAX: usize = RESUME.len() + RESUME_BODY_BYTES_MAX;

const MARK: &[u8] = b"133;";

const _: () = assert!(RESUME_REPORTS_PER_TURN_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Prompt,
    Command(Option<String>),
    Done(Option<i32>),
}

enum ParsedOsc {
    Mark(Mark),
    Resume(Vec<u8>),
}

#[derive(Default)]
pub struct Marks {
    osc: Option<Osc>,
}

#[derive(Default)]
struct Osc {
    text: Vec<u8>,
    over: bool,
}

impl Marks {
    pub fn write(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        bytes: &[u8],
        marks: &mut Vec<Mark>,
        resume_reports: &mut VecDeque<Vec<u8>>,
        mut on_mark: impl FnMut(&Mark),
    ) -> Result<(), Error> {
        let mut at = 0;
        while let Some(stop) =
            terminal.vt_write_until(&bytes[at..], Boundary::OSC_START | Boundary::OSC_END)?
        {
            assert!(stop.consumed > 0, "a stop at {at} took no byte");
            let from = at;
            at += stop.consumed;
            if stop.reached.contains(Boundary::OSC_END) {
                let mut osc = self
                    .osc
                    .take()
                    .expect("an OSC that ends started, as Ghostty said");
                osc.keep(&bytes[from..at - 1]);
                match osc.end(stop.end == Some(StringEnd::Cancelled)) {
                    Some(ParsedOsc::Mark(mark)) => {
                        on_mark(&mark);
                        marks.push(mark);
                    }
                    Some(ParsedOsc::Resume(body)) => {
                        push_bounded_resume_report(resume_reports, body);
                    }
                    None => {}
                }
            }
            if stop.reached.contains(Boundary::OSC_START) {
                assert!(self.osc.is_none(), "an OSC starts at {at} in another");
                self.osc = Some(Osc::default());
            }
        }
        if let Some(osc) = &mut self.osc {
            osc.keep(&bytes[at..]);
        }
        Ok(())
    }
}

pub fn push_bounded_resume_report(reports: &mut VecDeque<Vec<u8>>, body: Vec<u8>) {
    assert!(reports.len() <= RESUME_REPORTS_PER_TURN_MAX);
    if reports.len() == RESUME_REPORTS_PER_TURN_MAX {
        reports.pop_front();
    }
    reports.push_back(body);
    assert!(reports.len() <= RESUME_REPORTS_PER_TURN_MAX);
}

impl Osc {
    fn keep(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let matches_mark = self.text.starts_with(MARK) || MARK.starts_with(&self.text);
            let matches_resume = self.text.starts_with(RESUME) || RESUME.starts_with(&self.text);
            if !matches_mark && !matches_resume {
                break;
            }
            let limit = self.limit();
            if self.text.len() < limit {
                self.text.push(byte);
            } else {
                self.over = true;
                break;
            }
        }
        let limit = self.limit();
        assert!(self.text.len() <= limit);
        if self.over {
            assert_eq!(self.text.len(), limit);
        }
    }

    fn limit(&self) -> usize {
        if self.text.starts_with(RESUME) {
            RESUME_BYTES_MAX
        } else {
            MARK_BYTES_MAX
        }
    }

    fn end(self, cancelled: bool) -> Option<ParsedOsc> {
        if cancelled {
            return None;
        }
        if let Some(body) = self.text.strip_prefix(RESUME) {
            if self.over {
                return None;
            }
            return Some(ParsedOsc::Resume(body.to_vec()));
        }
        mark(self.text.strip_prefix(MARK)?, self.over).map(ParsedOsc::Mark)
    }
}

fn mark(text: &[u8], over: bool) -> Option<Mark> {
    let mut fields = text.split(|byte| *byte == b';');
    let kind = fields.next()?;
    match kind {
        b"A" => Some(Mark::Prompt),
        b"C" => {
            const RAW: &[u8] = b"cmdline=";
            let mut line = None;
            let mut start = kind.len() + 1;
            for field in fields {
                if let Some(encoded) = field.strip_prefix(b"cmdline_url=") {
                    line = Some(percent_decode(encoded).decode_utf8_lossy().into_owned());
                    break;
                }
                if field.starts_with(RAW) {
                    line = Some(String::from_utf8_lossy(&text[start + RAW.len()..]).into_owned());
                    break;
                }
                start += field.len() + 1;
            }
            Some(Mark::Command(line.filter(|_| !over)))
        }
        b"D" => {
            let status = fields
                .next()
                .and_then(|status| std::str::from_utf8(status).ok());
            Some(Mark::Done(status.and_then(|status| status.parse().ok())))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(chunks: &[&[u8]]) -> Vec<Mark> {
        read_both(chunks).0
    }

    fn read_both(chunks: &[&[u8]]) -> (Vec<Mark>, Vec<Vec<u8>>) {
        let mut terminal = Terminal::new(10, 2).unwrap();
        let mut scanner = Marks::default();
        let mut marks = Vec::new();
        let mut resumes = VecDeque::new();
        for chunk in chunks {
            scanner
                .write(&mut terminal, chunk, &mut marks, &mut resumes, |_| {})
                .unwrap();
        }
        (marks, resumes.into_iter().collect())
    }

    fn command(line: &str) -> Mark {
        Mark::Command(Some(line.to_owned()))
    }

    #[test]
    fn fish_s_marks_read_as_its_prompt_command_and_status() {
        let written: &[u8] = b"\x1b]133;A;click_events=1\x1b\\$ \x1b]133;B\x1b\\\
            \x1b]133;C;cmdline_url=false%3B%20echo%20%27a%20b%27\x1b\\\
            \x1b]0;title\x1b\\a b\r\n\x1b]133;D;1\x1b\\";
        let marks = read(&[written]);
        assert_eq!(
            marks,
            [
                Mark::Prompt,
                command("false; echo 'a b'"),
                Mark::Done(Some(1))
            ]
        );
    }

    #[test]
    fn a_mark_reads_the_same_split_at_any_byte() {
        let written: &[u8] = b"x\x1b]133;C;cmdline_url=ls%20-l\x07y\x1b]133;D;0\x1b\\";
        let whole = read(&[written]);
        assert_eq!(whole, [command("ls -l"), Mark::Done(Some(0))]);
        for at in 0..=written.len() {
            assert_eq!(
                read(&[&written[..at], &written[at..]]),
                whole,
                "split at {at}"
            );
        }
        let bytes: Vec<&[u8]> = written.chunks(1).collect();
        assert_eq!(read(&bytes), whole);
    }

    #[test]
    fn a_command_line_reads_raw_or_decoded() {
        assert_eq!(read(&[b"\x1b]133;C;cmdline=a; b\x07"]), [command("a; b")]);
        assert_eq!(
            read(&[b"\x1b]133;C;cmdline_url=caf%C3%A9\x07"]),
            [command("café")]
        );
        assert_eq!(
            read(&[b"\x1b]133;C;cmdline_url=%FF\x07"]),
            [command("\u{fffd}")]
        );
        assert_eq!(read(&[b"\x1b]133;C\x07"]), [Mark::Command(None)]);
        assert_eq!(read(&[b"\x1b]133;C;aid=3\x07"]), [Mark::Command(None)]);
    }

    #[test]
    fn a_status_reads_where_it_is_a_number() {
        assert_eq!(read(&[b"\x1b]133;D;-1\x07"]), [Mark::Done(Some(-1))]);
        assert_eq!(read(&[b"\x1b]133;D\x07"]), [Mark::Done(None)]);
        assert_eq!(read(&[b"\x1b]133;D;x\x07"]), [Mark::Done(None)]);
        assert_eq!(read(&[b"\x1b]133;D;99999999999\x07"]), [Mark::Done(None)]);
    }

    #[test]
    fn what_is_no_mark_reads_as_none() {
        assert_eq!(read(&[b"\x1b]0;133;A\x07\x1b]1330;A\x07\x1b[133;Am"]), []);
        assert_eq!(read(&[b"\x1b]133;B\x07\x1b]133;Z\x07\x1b]133\x07"]), []);
        assert_eq!(read(&[b"\x1b]133;A\x18\x07"]), []);
    }

    #[test]
    fn a_mark_starts_and_ends_where_ghostty_says() {
        assert_eq!(read(&[b"\x1b]133;A\x1b[m"]), [Mark::Prompt]);
        assert_eq!(
            read(&[b"\x1b]133;A\x1b\x1b]133;D;2\x07"]),
            [Mark::Prompt, Mark::Done(Some(2))]
        );
        assert_eq!(read(&[b"\x1b_x\x9d133;A\x07"]), [Mark::Prompt]);
        assert_eq!(read(&[b"\x1b]133;A\x9d133;D;1\x07"]), []);
        assert_eq!(read(&[b"\x1b]\x1b]\x9d\x9d\x07\x07"]), []);
    }

    #[test]
    fn a_command_line_too_long_reads_as_none() {
        let line = "a".repeat(MARK_BYTES_MAX);
        let long = format!("\x1b]133;C;cmdline_url={line}\x07\x1b]133;D;0\x07");
        assert_eq!(
            read(&[long.as_bytes()]),
            [Mark::Command(None), Mark::Done(Some(0))]
        );
        let fits = format!(
            "\x1b]133;C;cmdline_url={}\x07",
            "a".repeat(MARK_BYTES_MAX - 20)
        );
        assert_eq!(
            read(&[fits.as_bytes()]),
            [command(&"a".repeat(MARK_BYTES_MAX - 20))]
        );
    }

    #[test]
    fn osc_7577_reads_across_splits_and_ignores_cancelled_and_oversized() {
        let body = b"pid=42:time=1795543200:clear=1:mac=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut wire = Vec::new();
        wire.extend_from_slice(b"hi\x1b]7577;");
        wire.extend_from_slice(body);
        wire.extend_from_slice(b"\x1b\\\x1b]133;A\x07\x1b]7577;");
        wire.extend_from_slice(body);
        wire.extend_from_slice(b"\x07");

        let whole = read_both(&[&wire]);
        assert_eq!(whole.0, [Mark::Prompt]);
        assert_eq!(whole.1, [body.to_vec(), body.to_vec()]);
        for at in 0..=wire.len() {
            assert_eq!(
                read_both(&[&wire[..at], &wire[at..]]),
                whole,
                "split at {at}"
            );
        }
        let singles: Vec<&[u8]> = wire.chunks(1).collect();
        assert_eq!(read_both(&singles), whole);

        assert_eq!(
            read_both(&[b"\x1b]7577;pid=1\x18\x07"]).1,
            Vec::<Vec<u8>>::new()
        );
        let too_long = format!("\x1b]7577;{}\x07", "a".repeat(RESUME_BODY_BYTES_MAX + 1));
        assert_eq!(read_both(&[too_long.as_bytes()]).1, Vec::<Vec<u8>>::new());
        let exact = "a".repeat(RESUME_BODY_BYTES_MAX);
        let fits = format!("\x1b]7577;{exact}\x07");
        assert_eq!(read_both(&[fits.as_bytes()]).1, [exact.into_bytes()]);
    }

    #[test]
    fn osc_7577_caps_reports_per_turn_and_keeps_latest() {
        let mut wire = Vec::new();
        let total = RESUME_REPORTS_PER_TURN_MAX + 25;
        for index in 0..total {
            wire.extend_from_slice(format!("\x1b]7577;seq={index}\x07").as_bytes());
        }
        let (_, reports) = read_both(&[&wire]);
        assert_eq!(reports.len(), RESUME_REPORTS_PER_TURN_MAX);
        assert_eq!(
            reports.first().map(Vec::as_slice),
            Some(b"seq=25".as_slice())
        );
        assert_eq!(
            reports.last().map(Vec::as_slice),
            Some(format!("seq={}", total - 1).as_bytes())
        );
    }
}
