use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::{Error, Terminal};

use super::SCROLLBACK_BYTES_MAX;
use crate::asked::Size;

pub(super) struct Answering {
    terminal: Terminal<'static, 'static>,
    answers: Rc<RefCell<Vec<u8>>>,
}

impl Answering {
    pub(super) fn new(size: Size, scrollback_bytes: usize) -> Result<Answering, Error> {
        assert!(scrollback_bytes <= SCROLLBACK_BYTES_MAX, "{scrollback_bytes} of scrollback");
        let (columns, rows) = size.get();
        let mut terminal = Terminal::new(columns, rows)?;
        terminal.set_scrollback_max_bytes(Some(scrollback_bytes))?;
        assert_eq!(terminal.scrollback_max_bytes()?, Some(scrollback_bytes));
        let answers = Rc::new(RefCell::new(Vec::new()));
        let written = Rc::clone(&answers);
        terminal.on_pty_write(move |_, bytes| written.borrow_mut().extend_from_slice(bytes))?;
        Ok(Answering { terminal, answers })
    }

    pub(super) fn drive<T>(
        &mut self,
        drive: impl FnOnce(&mut Terminal<'static, 'static>) -> T,
    ) -> (T, Vec<u8>) {
        assert!(self.answers.borrow().is_empty(), "an answer outside a drive");
        let made = drive(&mut self.terminal);
        (made, self.answers.take())
    }

    pub(super) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.terminal
    }
}
