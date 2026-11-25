use libghostty_vt::terminal::{Boundary, StringEnd};
use libghostty_vt::{Error, Terminal};
use percent_encoding::percent_decode;

const MARK_BYTES_MAX: usize = 16 * 1024;

const MARK: &[u8] = b"133;";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Prompt,
    Command(Option<String>),
    Done(Option<i32>),
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
        mut on_prompt: impl FnMut(),
    ) -> Result<(), Error> {
        let mut at = 0;
        while let Some(stop) =
            terminal.vt_write_until(&bytes[at..], Boundary::OSC_START | Boundary::OSC_END)?
        {
            assert!(stop.consumed > 0, "a stop at {at} took no byte");
            let from = at;
            at += stop.consumed;
            if stop.reached.contains(Boundary::OSC_END) {
                let mut osc = self.osc.take().expect("an OSC that ends started, as Ghostty said");
                osc.keep(&bytes[from..at - 1]);
                if let Some(mark) = osc.end(stop.end == Some(StringEnd::Cancelled)) {
                    if mark == Mark::Prompt {
                        on_prompt();
                    }
                    marks.push(mark);
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

impl Osc {
    fn keep(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let markable = self.text.starts_with(MARK) || MARK.starts_with(&self.text);
            if !markable {
                break;
            }
            if self.text.len() < MARK_BYTES_MAX {
                self.text.push(byte);
            } else {
                self.over = true;
                break;
            }
        }
        assert!(self.text.len() <= MARK_BYTES_MAX);
        if self.over {
            assert_eq!(self.text.len(), MARK_BYTES_MAX);
        }
    }

    fn end(self, cancelled: bool) -> Option<Mark> {
        if cancelled {
            return None;
        }
        mark(self.text.strip_prefix(MARK)?, self.over)
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
        },
        b"D" => {
            let status = fields.next().and_then(|status| std::str::from_utf8(status).ok());
            Some(Mark::Done(status.and_then(|status| status.parse().ok())))
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(chunks: &[&[u8]]) -> Vec<Mark> {
        let mut terminal = Terminal::new(10, 2).unwrap();
        let mut scanner = Marks::default();
        let mut marks = Vec::new();
        for chunk in chunks {
            scanner.write(&mut terminal, chunk, &mut marks, || {}).unwrap();
        }
        marks
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
        assert_eq!(marks, [Mark::Prompt, command("false; echo 'a b'"), Mark::Done(Some(1))]);
    }

    #[test]
    fn a_mark_reads_the_same_split_at_any_byte() {
        let written: &[u8] = b"x\x1b]133;C;cmdline_url=ls%20-l\x07y\x1b]133;D;0\x1b\\";
        let whole = read(&[written]);
        assert_eq!(whole, [command("ls -l"), Mark::Done(Some(0))]);
        for at in 0..=written.len() {
            assert_eq!(read(&[&written[..at], &written[at..]]), whole, "split at {at}");
        }
        let bytes: Vec<&[u8]> = written.chunks(1).collect();
        assert_eq!(read(&bytes), whole);
    }

    #[test]
    fn a_command_line_reads_raw_or_decoded() {
        assert_eq!(read(&[b"\x1b]133;C;cmdline=a; b\x07"]), [command("a; b")]);
        assert_eq!(read(&[b"\x1b]133;C;cmdline_url=caf%C3%A9\x07"]), [command("café")]);
        assert_eq!(read(&[b"\x1b]133;C;cmdline_url=%FF\x07"]), [command("\u{fffd}")]);
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
        assert_eq!(read(&[b"\x1b]133;A\x1b\x1b]133;D;2\x07"]), [Mark::Prompt, Mark::Done(Some(2))]);
        assert_eq!(read(&[b"\x1b_x\x9d133;A\x07"]), [Mark::Prompt]);
        assert_eq!(read(&[b"\x1b]133;A\x9d133;D;1\x07"]), []);
        assert_eq!(read(&[b"\x1b]\x1b]\x9d\x9d\x07\x07"]), []);
    }

    #[test]
    fn a_command_line_too_long_reads_as_none() {
        let line = "a".repeat(MARK_BYTES_MAX);
        let long = format!("\x1b]133;C;cmdline_url={line}\x07\x1b]133;D;0\x07");
        assert_eq!(read(&[long.as_bytes()]), [Mark::Command(None), Mark::Done(Some(0))]);
        let fits = format!("\x1b]133;C;cmdline_url={}\x07", "a".repeat(MARK_BYTES_MAX - 20));
        assert_eq!(read(&[fits.as_bytes()]), [command(&"a".repeat(MARK_BYTES_MAX - 20))]);
    }
}
