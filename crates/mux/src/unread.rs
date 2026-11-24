use crate::screen::Answers;

pub const TYPED_BYTES_MAX: usize = 1024 * 1024;

pub const ANSWERS_BYTES_MAX: usize = 64 * 1024;

const _: () = assert!(ANSWERS_BYTES_MAX <= TYPED_BYTES_MAX);

#[derive(Default)]
pub struct Unread {
    sending: Vec<u8>,
    sent_bytes: usize,
    typed: Vec<u8>,
    answers: Answers,
    losing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Behind { typed_bytes: usize },
    Larger { bytes: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept {
    pub too_long: usize,
    pub oldest: Oldest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Oldest {
    Kept,
    Lost,
    StillLost,
}

impl Unread {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sending.is_empty() && self.typed.is_empty() && self.answers.is_empty()
    }

    pub fn type_in(&mut self, bytes: &[u8]) -> Result<(), Refused> {
        if bytes.len() > TYPED_BYTES_MAX {
            return Err(Refused::Larger { bytes: bytes.len() });
        }
        if self.typed.len() + bytes.len() > TYPED_BYTES_MAX {
            return Err(Refused::Behind { typed_bytes: self.typed.len() });
        }
        self.typed.extend_from_slice(bytes);
        self.assert_bounded();
        Ok(())
    }

    pub fn answer(&mut self, answers: &Answers) -> Kept {
        let mut too_long = 0;
        for answer in answers.each() {
            if answer.len() > ANSWERS_BYTES_MAX {
                too_long += 1;
            } else {
                self.answers.push(answer);
            }
        }
        let lost = self.answers.keep_newest(ANSWERS_BYTES_MAX);
        let oldest = match (lost, self.losing) {
            (false, _) => Oldest::Kept,
            (true, false) => Oldest::Lost,
            (true, true) => Oldest::StillLost,
        };
        self.losing |= lost;
        self.assert_bounded();
        Kept { too_long, oldest }
    }

    pub fn line_up(&mut self) -> Option<&[u8]> {
        if self.sending.is_empty() {
            if !self.typed.is_empty() {
                std::mem::swap(&mut self.sending, &mut self.typed);
            } else if !self.answers.is_empty() {
                self.answers.move_into(&mut self.sending);
                self.losing = false;
            }
        }
        self.assert_bounded();
        if self.sending.is_empty() {
            assert!(self.is_empty(), "input waits behind nothing lined up");
            return None;
        }
        Some(&self.sending[self.sent_bytes..])
    }

    pub fn sent(&mut self, count: usize) {
        let lined_up = self.sending.len() - self.sent_bytes;
        assert!(count > 0, "none of {lined_up} bytes sent");
        assert!(count <= lined_up, "{count} of {lined_up} bytes sent");
        self.sent_bytes += count;
        if self.sent_bytes == self.sending.len() {
            self.sending.clear();
            self.sent_bytes = 0;
        }
        self.assert_bounded();
    }

    pub fn clear(&mut self) {
        *self = Unread::default();
    }

    fn assert_bounded(&self) {
        assert!(self.typed.len() <= TYPED_BYTES_MAX, "{} typed bytes", self.typed.len());
        let answered = self.answers.bytes().len();
        assert!(answered <= ANSWERS_BYTES_MAX, "{answered} bytes of answers");
        assert!(self.sending.len() <= TYPED_BYTES_MAX, "{} bytes in flight", self.sending.len());
        assert!(!self.losing || !self.answers.is_empty(), "answers lost with none waiting");
        if self.sending.is_empty() {
            assert_eq!(self.sent_bytes, 0, "bytes sent of nothing in flight");
        } else {
            assert!(self.sent_bytes < self.sending.len(), "all in flight is sent but kept");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::*;
    use crate::asked::Size;
    use crate::screen::Screen;

    fn answers_to(asked: &[u8]) -> Answers {
        let columns = NonZeroU16::new(80).unwrap();
        let rows = NonZeroU16::new(24).unwrap();
        let mut screen = Screen::new(Size { columns, rows }, 0).unwrap();
        screen.write(asked).unwrap().answers
    }

    fn sent_all(unread: &mut Unread) -> Vec<u8> {
        let mut sent = Vec::new();
        while let Some(lined_up) = unread.line_up() {
            let count = lined_up.len().min(3);
            sent.extend_from_slice(&lined_up[..count]);
            unread.sent(count);
        }
        sent
    }

    fn kept(oldest: Oldest) -> Kept {
        Kept { too_long: 0, oldest }
    }

    #[test]
    fn what_is_typed_goes_before_answers_but_never_inside_one() {
        let mut unread = Unread::default();
        assert_eq!(unread.answer(&answers_to(b"\x1b[6n")), kept(Oldest::Kept));
        unread.type_in(b"a").unwrap();
        assert_eq!(unread.line_up(), Some(&b"a"[..]));
        unread.sent(1);
        assert_eq!(unread.line_up(), Some(&b"\x1b[1;1R"[..]));
        unread.sent(2);
        assert_eq!(unread.line_up(), Some(&b"1;1R"[..]));
        unread.type_in(b"\x03").unwrap();
        assert_eq!(unread.answer(&answers_to(b"\x1b[5n")), kept(Oldest::Kept));
        assert_eq!(sent_all(&mut unread), b"1;1R\x03\x1b[0n");
        assert!(unread.is_empty());
    }

    #[test]
    fn answers_past_their_bound_lose_the_oldest_whole() {
        let mut unread = Unread::default();
        let asked = b"\x1b[5n".repeat(ANSWERS_BYTES_MAX / 4 + 1);
        let answers = answers_to(&asked);
        assert_eq!(answers.bytes().len(), ANSWERS_BYTES_MAX + 4);
        assert_eq!(unread.answer(&answers), kept(Oldest::Lost));
        assert_eq!(unread.answer(&answers_to(b"\x1b[6n")), kept(Oldest::StillLost));
        unread.type_in(b"typed").unwrap();
        let sent = sent_all(&mut unread);
        assert_eq!(&sent[..5], b"typed");
        assert_eq!(sent.len(), 5 + ANSWERS_BYTES_MAX - 2);
        assert!(sent[5..].starts_with(b"\x1b[0n\x1b[0n"));
        assert!(sent.ends_with(b"\x1b[0n\x1b[1;1R"));
        assert_eq!(unread.answer(&answers_to(b"\x1b[5n")), kept(Oldest::Kept));
    }

    #[test]
    fn an_answer_longer_than_the_bound_is_lost_alone_and_leaves_no_loss_behind() {
        let mut unread = Unread::default();
        let mut answers = Answers::default();
        answers.push(&vec![b'x'; ANSWERS_BYTES_MAX + 1]);
        answers.push(b"\x1b[0n");
        assert_eq!(unread.answer(&answers), Kept { too_long: 1, oldest: Oldest::Kept });
        assert_eq!(sent_all(&mut unread), b"\x1b[0n");
        let mut answers = Answers::default();
        answers.push(&vec![b'x'; ANSWERS_BYTES_MAX + 1]);
        assert_eq!(unread.answer(&answers), Kept { too_long: 1, oldest: Oldest::Kept });
        assert!(unread.is_empty());
        let flood = answers_to(&b"\x1b[5n".repeat(ANSWERS_BYTES_MAX / 4 + 1));
        assert_eq!(unread.answer(&flood), kept(Oldest::Lost));
    }

    #[test]
    fn what_is_typed_is_held_to_its_own_bound_whatever_the_answers() {
        let mut unread = Unread::default();
        let answers = answers_to(&b"\x1b[5n".repeat(ANSWERS_BYTES_MAX));
        assert_eq!(unread.answer(&answers), kept(Oldest::Lost));
        unread.type_in(&vec![b'x'; TYPED_BYTES_MAX]).unwrap();
        assert_eq!(unread.type_in(b"y"), Err(Refused::Behind { typed_bytes: TYPED_BYTES_MAX }));
        assert_eq!(unread.answer(&answers), kept(Oldest::StillLost));
        assert_eq!(unread.line_up().map(<[u8]>::len), Some(TYPED_BYTES_MAX));
        unread.type_in(b"y").unwrap();
        unread.clear();
        assert!(unread.is_empty());
        assert_eq!(unread.line_up(), None);
    }

    #[test]
    fn what_is_typed_past_the_bound_alone_is_refused_as_larger_even_when_nothing_waits() {
        let mut unread = Unread::default();
        let bytes = TYPED_BYTES_MAX + 1;
        assert_eq!(unread.type_in(&vec![b'x'; bytes]), Err(Refused::Larger { bytes }));
        assert!(unread.is_empty());
    }
}
