use std::cell::RefCell;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::rc::Rc;

use libghostty_vt::{Error, Terminal};

use super::SCROLLBACK_BYTES_MAX;
use crate::asked::Size;

pub(super) struct Answering {
    terminal: Terminal<'static, 'static>,
    answers: Rc<RefCell<Answers>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Answers {
    bytes: Vec<u8>,
    lengths: VecDeque<NonZeroUsize>,
}

impl Answers {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        assert_eq!(
            self.lengths.is_empty(),
            self.bytes.is_empty(),
            "answers of no bytes"
        );
        self.lengths.is_empty()
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn each(&self) -> impl Iterator<Item = &[u8]> {
        let mut start = 0;
        self.lengths.iter().map(move |length| {
            let answer = &self.bytes[start..start + length.get()];
            start += length.get();
            answer
        })
    }

    pub fn push(&mut self, answer: &[u8]) {
        let Some(length) = NonZeroUsize::new(answer.len()) else {
            return;
        };
        self.bytes.extend_from_slice(answer);
        self.lengths.push_back(length);
    }

    pub fn keep_newest(&mut self, most: usize) -> bool {
        let mut oldest = 0;
        while self.bytes.len() - oldest > most {
            oldest += self
                .lengths
                .pop_front()
                .expect("bytes over the bound are answers")
                .get();
        }
        self.bytes.drain(..oldest);
        self.assert_whole();
        oldest > 0
    }

    pub fn move_into(&mut self, sending: &mut Vec<u8>) {
        assert!(
            sending.is_empty(),
            "answers moved behind {} bytes",
            sending.len()
        );
        std::mem::swap(sending, &mut self.bytes);
        self.lengths.clear();
    }

    fn assert_whole(&self) {
        assert_eq!(
            self.lengths.is_empty(),
            self.bytes.is_empty(),
            "answers of no bytes"
        );
        debug_assert_eq!(
            self.lengths
                .iter()
                .map(|length| length.get())
                .sum::<usize>(),
            self.bytes.len(),
            "answers' lengths and bytes differ"
        );
    }
}

impl Answering {
    pub(super) fn new(size: Size, scrollback_bytes: usize) -> Result<Answering, Error> {
        assert!(
            scrollback_bytes <= SCROLLBACK_BYTES_MAX,
            "{scrollback_bytes} of scrollback"
        );
        let (columns, rows) = size.get();
        let mut terminal = Terminal::new(columns, rows)?;
        terminal.set_scrollback_max_bytes(Some(scrollback_bytes))?;
        assert_eq!(terminal.scrollback_max_bytes()?, Some(scrollback_bytes));
        let answers = Rc::new(RefCell::new(Answers::default()));
        let written = Rc::clone(&answers);
        terminal.on_pty_write(move |_, bytes| written.borrow_mut().push(bytes))?;
        Ok(Answering { terminal, answers })
    }

    pub(super) fn drive<T>(
        &mut self,
        drive: impl FnOnce(&mut Terminal<'static, 'static>) -> T,
    ) -> (T, Answers) {
        assert!(
            self.answers.borrow().is_empty(),
            "an answer outside a drive"
        );
        let made = drive(&mut self.terminal);
        (made, self.answers.take())
    }

    pub(super) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.terminal
    }
}
