use std::collections::VecDeque;

use crate::screen::Answers;

pub const TYPED_BYTES_MAX: usize = 1024 * 1024;

pub const ANSWERS_BYTES_MAX: usize = 64 * 1024;

const _: () = assert!(ANSWERS_BYTES_MAX <= TYPED_BYTES_MAX);

#[derive(Default)]
pub struct Unread {
    sending: Vec<u8>,
    sent_bytes: usize,
    typed: Vec<u8>,
    answers: Vec<u8>,
    answer_lengths: VecDeque<usize>,
    losing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full {
    pub typed_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    All,
    LostOldest,
    StillLosing,
}

impl Unread {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sending.is_empty() && self.typed.is_empty() && self.answers.is_empty()
    }

    pub fn type_in(&mut self, bytes: &[u8]) -> Result<(), Full> {
        if self.typed.len() + bytes.len() > TYPED_BYTES_MAX {
            return Err(Full { typed_bytes: self.typed.len() });
        }
        self.typed.extend_from_slice(bytes);
        self.assert_bounded();
        Ok(())
    }

    pub fn answer(&mut self, answers: &Answers) -> Kept {
        let mut lost = false;
        let mut taken = 0;
        for answer in answers.each() {
            assert!(!answer.is_empty(), "an answer of no bytes");
            taken += answer.len();
            if answer.len() > ANSWERS_BYTES_MAX {
                lost = true;
                continue;
            }
            self.answers.extend_from_slice(answer);
            self.answer_lengths.push_back(answer.len());
        }
        assert_eq!(taken, answers.bytes().len(), "answers left bytes over");
        let mut oldest = 0;
        while self.answers.len() - oldest > ANSWERS_BYTES_MAX {
            oldest += self.answer_lengths.pop_front().expect("bytes over the bound are answers");
            lost = true;
        }
        self.answers.drain(..oldest);
        self.assert_bounded();
        let was_losing = self.losing;
        self.losing |= lost;
        match (lost, was_losing) {
            (false, _) => Kept::All,
            (true, false) => Kept::LostOldest,
            (true, true) => Kept::StillLosing,
        }
    }

    pub fn line_up(&mut self) -> Option<&[u8]> {
        if self.sending.is_empty() {
            if !self.typed.is_empty() {
                std::mem::swap(&mut self.sending, &mut self.typed);
            } else if !self.answers.is_empty() {
                std::mem::swap(&mut self.sending, &mut self.answers);
                self.answer_lengths.clear();
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
        assert!(self.answers.len() <= ANSWERS_BYTES_MAX, "{} bytes of answers", self.answers.len());
        assert!(self.sending.len() <= TYPED_BYTES_MAX, "{} bytes in flight", self.sending.len());
        assert!(self.answer_lengths.len() <= self.answers.len(), "an answer of no bytes");
        assert_eq!(self.answer_lengths.is_empty(), self.answers.is_empty());
        if self.sending.is_empty() {
            assert_eq!(self.sent_bytes, 0, "bytes sent of nothing in flight");
        } else {
            assert!(self.sent_bytes < self.sending.len(), "all in flight is sent but kept");
        }
        debug_assert_eq!(self.answer_lengths.iter().sum::<usize>(), self.answers.len());
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

    #[test]
    fn what_is_typed_goes_before_answers_but_never_inside_one() {
        let mut unread = Unread::default();
        assert_eq!(unread.answer(&answers_to(b"\x1b[6n")), Kept::All);
        unread.type_in(b"a").unwrap();
        assert_eq!(unread.line_up(), Some(&b"a"[..]));
        unread.sent(1);
        assert_eq!(unread.line_up(), Some(&b"\x1b[1;1R"[..]));
        unread.sent(2);
        assert_eq!(unread.line_up(), Some(&b"1;1R"[..]));
        unread.type_in(b"\x03").unwrap();
        assert_eq!(unread.answer(&answers_to(b"\x1b[5n")), Kept::All);
        assert_eq!(sent_all(&mut unread), b"1;1R\x03\x1b[0n");
        assert!(unread.is_empty());
    }

    #[test]
    fn answers_past_their_bound_lose_the_oldest_whole() {
        let mut unread = Unread::default();
        let asked = b"\x1b[5n".repeat(ANSWERS_BYTES_MAX / 4 + 1);
        let answers = answers_to(&asked);
        assert_eq!(answers.bytes().len(), ANSWERS_BYTES_MAX + 4);
        assert_eq!(unread.answer(&answers), Kept::LostOldest);
        assert_eq!(unread.answer(&answers_to(b"\x1b[6n")), Kept::StillLosing);
        unread.type_in(b"typed").unwrap();
        let sent = sent_all(&mut unread);
        assert_eq!(&sent[..5], b"typed");
        assert_eq!(sent.len(), 5 + ANSWERS_BYTES_MAX - 2);
        assert!(sent[5..].starts_with(b"\x1b[0n\x1b[0n"));
        assert!(sent.ends_with(b"\x1b[0n\x1b[1;1R"));
        assert_eq!(unread.answer(&answers_to(b"\x1b[5n")), Kept::All);
    }

    #[test]
    fn what_is_typed_is_held_to_its_own_bound_whatever_the_answers() {
        let mut unread = Unread::default();
        let answers = answers_to(&b"\x1b[5n".repeat(ANSWERS_BYTES_MAX));
        assert_eq!(unread.answer(&answers), Kept::LostOldest);
        unread.type_in(&vec![b'x'; TYPED_BYTES_MAX]).unwrap();
        assert_eq!(unread.type_in(b"y"), Err(Full { typed_bytes: TYPED_BYTES_MAX }));
        assert_eq!(unread.answer(&answers), Kept::StillLosing);
        assert_eq!(unread.line_up().map(<[u8]>::len), Some(TYPED_BYTES_MAX));
        unread.type_in(b"y").unwrap();
        unread.clear();
        assert!(unread.is_empty());
        assert_eq!(unread.line_up(), None);
    }
}
