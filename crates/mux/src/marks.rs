//! The marks a shell puts in what it writes (OSC 133, as `FinalTerm` named
//! them): where its prompt starts, where a command it runs starts, with its
//! command line, and where that ends, with its exit status. fish 4 writes
//! them of itself, and zsh and bash with a terminal's integration.
//!
//! ```text
//! ESC ] 133 ; A [; <option>]… ST               a prompt
//! ESC ] 133 ; C [; cmdline_url=<percent-encoded>] ST
//! ESC ] 133 ; C [; cmdline=<text>] ST           a command starts
//! ESC ] 133 ; D [; <status>] ST                 it ended
//! ```
//!
//! with ST either `ESC \` or BEL. A mark may come split over two reads.
//! Where an OSC starts and ends is Ghostty's to say, as it reads the same
//! bytes for the screen: the scanner keeps only the text between.

use libghostty_vt::terminal::{Boundary, StringEnd};
use libghostty_vt::{Error, Terminal};
use percent_encoding::percent_decode;

/// The most bytes of a mark the scanner keeps: one past them says no command
/// line, as it may hold no more than the start of one.
const MARK_BYTES_MAX: usize = 16 * 1024;

/// What starts a mark's text.
const MARK: &[u8] = b"133;";

/// A mark, as the shell wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Prompt,
    /// A command starts, with its line, where the shell said it.
    Command(Option<String>),
    /// The command ended, with its exit status, where the shell said it.
    Done(Option<i32>),
}

/// The marks in what a program writes, read as it writes it.
#[derive(Default)]
pub struct Marks {
    /// The OSC being read, where one is.
    osc: Option<Osc>,
}

/// The text so far of an OSC, as far as it may be a mark.
#[derive(Default)]
struct Osc {
    text: Vec<u8>,
    /// Whether the OSC is past `MARK_BYTES_MAX`.
    over: bool,
}

impl Marks {
    /// Writes `bytes`, which follow those written before, to `terminal`,
    /// and puts each mark they end in `marks`.
    pub fn write(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        bytes: &[u8],
        marks: &mut Vec<Mark>,
    ) -> Result<(), Error> {
        let mut at = 0;
        while let Some(stop) =
            terminal.vt_write_until(&bytes[at..], Boundary::OSC_START | Boundary::OSC_END)?
        {
            assert!(stop.consumed > 0, "a stop at {at} took no byte");
            let from = at;
            at += stop.consumed;
            if stop.reached.contains(Boundary::OSC_END) {
                self.keep(&bytes[from..at - 1]);
                if let Some(mark) = self.end(stop.end == Some(StringEnd::Cancelled)) {
                    marks.push(mark);
                }
            }
            if stop.reached.contains(Boundary::OSC_START) {
                self.osc = Some(Osc::default());
            }
        }
        self.keep(&bytes[at..]);
        Ok(())
    }

    /// Keeps `bytes` of the text of the OSC being read, as far as it may be
    /// a mark.
    fn keep(&mut self, bytes: &[u8]) {
        let Some(osc) = &mut self.osc else {
            return;
        };
        for &byte in bytes {
            if !(osc.text.starts_with(MARK) || MARK.starts_with(&osc.text)) {
                return;
            }
            if osc.text.len() == MARK_BYTES_MAX {
                osc.over = true;
                return;
            }
            osc.text.push(byte);
        }
        assert!(osc.text.len() <= MARK_BYTES_MAX);
    }

    /// Ends the OSC read, `cancelled` by CAN or SUB or not, and gives its
    /// mark where it is one.
    fn end(&mut self, cancelled: bool) -> Option<Mark> {
        let osc = self.osc.take()?;
        if osc.over {
            assert_eq!(osc.text.len(), MARK_BYTES_MAX);
        }
        if cancelled {
            return None;
        }
        mark(osc.text.strip_prefix(MARK)?, osc.over)
    }
}

/// The mark `text` says, past its `133;`, where it is one this reads: none
/// for `B`, where the prompt ends and what you type starts, or another. A
/// mark `over` the bytes kept says no command line.
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
            scanner.write(&mut terminal, chunk, &mut marks).unwrap();
        }
        marks
    }

    fn command(line: &str) -> Mark {
        Mark::Command(Some(line.to_owned()))
    }

    /// What fish 4.9 writes around `false; echo 'a b'`, a mark at a time.
    #[test]
    fn fish_s_marks_read_as_its_prompt_command_and_status() {
        let written: &[u8] = b"\x1b]133;A;click_events=1\x1b\\$ \x1b]133;B\x1b\\\
            \x1b]133;C;cmdline_url=false%3B%20echo%20%27a%20b%27\x1b\\\
            \x1b]0;title\x1b\\a b\r\n\x1b]133;D;1\x1b\\";
        let marks = read(&[written]);
        assert_eq!(marks, [Mark::Prompt, command("false; echo 'a b'"), Mark::Done(Some(1))]);
    }

    /// A mark split at each byte reads as it does whole, and so does one that
    /// ends with BEL.
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

    /// `cmdline=` takes the rest of the mark, its `;` with it, and the URL
    /// form decodes as UTF-8, a byte that is none of it taken as U+FFFD.
    #[test]
    fn a_command_line_reads_raw_or_decoded() {
        assert_eq!(read(&[b"\x1b]133;C;cmdline=a; b\x07"]), [command("a; b")]);
        assert_eq!(read(&[b"\x1b]133;C;cmdline_url=caf%C3%A9\x07"]), [command("café")]);
        assert_eq!(read(&[b"\x1b]133;C;cmdline_url=%FF\x07"]), [command("\u{fffd}")]);
        assert_eq!(read(&[b"\x1b]133;C\x07"]), [Mark::Command(None)]);
        assert_eq!(read(&[b"\x1b]133;C;aid=3\x07"]), [Mark::Command(None)]);
    }

    /// A status that is no number reads as none, and so does none.
    #[test]
    fn a_status_reads_where_it_is_a_number() {
        assert_eq!(read(&[b"\x1b]133;D;-1\x07"]), [Mark::Done(Some(-1))]);
        assert_eq!(read(&[b"\x1b]133;D\x07"]), [Mark::Done(None)]);
        assert_eq!(read(&[b"\x1b]133;D;x\x07"]), [Mark::Done(None)]);
        assert_eq!(read(&[b"\x1b]133;D;99999999999\x07"]), [Mark::Done(None)]);
    }

    /// Other OSCs, other escapes, `B`, and a mark called off by CAN are
    /// none.
    #[test]
    fn what_is_no_mark_reads_as_none() {
        assert_eq!(read(&[b"\x1b]0;133;A\x07\x1b]1330;A\x07\x1b[133;Am"]), []);
        assert_eq!(read(&[b"\x1b]133;B\x07\x1b]133;Z\x07\x1b]133\x07"]), []);
        assert_eq!(read(&[b"\x1b]133;A\x18\x07"]), []);
    }

    /// A mark starts and ends where Ghostty's parse says: an ESC ends it, as
    /// another escape follows, and a 0x9D in an APC starts one.
    #[test]
    fn a_mark_starts_and_ends_where_ghostty_says() {
        assert_eq!(read(&[b"\x1b]133;A\x1b[m"]), [Mark::Prompt]);
        assert_eq!(read(&[b"\x1b]133;A\x1b\x1b]133;D;2\x07"]), [Mark::Prompt, Mark::Done(Some(2))]);
        assert_eq!(read(&[b"\x1b_x\x9d133;A\x07"]), [Mark::Prompt]);
    }

    /// A command line past what the scanner keeps reads as none, and the
    /// marks after it as they are.
    #[test]
    fn a_command_line_too_long_reads_as_none() {
        let line = "a".repeat(MARK_BYTES_MAX);
        let long = format!("\x1b]133;C;cmdline_url={line}\x07\x1b]133;D;0\x07");
        assert_eq!(read(&[long.as_bytes()]), [Mark::Command(None), Mark::Done(Some(0))]);
        let fits = format!("\x1b]133;C;cmdline_url={}\x07", "a".repeat(MARK_BYTES_MAX - 20));
        assert_eq!(read(&[fits.as_bytes()]), [command(&"a".repeat(MARK_BYTES_MAX - 20))]);
    }
}
