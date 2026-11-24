use std::cell::RefCell;
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
    lengths: Vec<usize>,
}

impl Answers {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        assert_eq!(self.lengths.is_empty(), self.bytes.is_empty(), "answers of no bytes");
        self.lengths.is_empty()
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn each(&self) -> impl Iterator<Item = &[u8]> {
        let mut start = 0;
        self.lengths.iter().map(move |&length| {
            let answer = &self.bytes[start..start + length];
            start += length;
            answer
        })
    }

    fn push(&mut self, answer: &[u8]) {
        assert!(!answer.is_empty(), "an answer of no bytes");
        self.bytes.extend_from_slice(answer);
        self.lengths.push(answer.len());
    }
}

impl Answering {
    pub(super) fn new(size: Size, scrollback_bytes: usize) -> Result<Answering, Error> {
        assert!(scrollback_bytes <= SCROLLBACK_BYTES_MAX, "{scrollback_bytes} of scrollback");
        let (columns, rows) = size.get();
        let mut terminal = Terminal::new(columns, rows)?;
        terminal.set_scrollback_max_bytes(Some(scrollback_bytes))?;
        assert_eq!(terminal.scrollback_max_bytes()?, Some(scrollback_bytes));
        let answers = Rc::new(RefCell::new(Answers::default()));
        let written = Rc::clone(&answers);
        terminal.on_pty_write(move |_, bytes| {
            if !bytes.is_empty() {
                written.borrow_mut().push(bytes);
            }
        })?;
        Ok(Answering { terminal, answers })
    }

    pub(super) fn drive<T>(
        &mut self,
        drive: impl FnOnce(&mut Terminal<'static, 'static>) -> T,
    ) -> (T, Answers) {
        assert!(self.answers.borrow().is_empty(), "an answer outside a drive");
        let made = drive(&mut self.terminal);
        (made, self.answers.take())
    }

    pub(super) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.terminal
    }
}
