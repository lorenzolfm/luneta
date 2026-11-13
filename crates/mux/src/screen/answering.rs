//! A terminal that gives back what it answered its program from the call that
//! made it answer, and from no later one.

use std::cell::RefCell;
use std::num::NonZeroU16;
use std::rc::Rc;

use libghostty_vt::{Error, Terminal};

/// A terminal, which only [`Answering::drive`] can change.
pub(super) struct Answering {
    terminal: Terminal<'static, 'static>,
    /// What the terminal answered its program during the drive it is in, the
    /// bytes it writes back to it.
    answers: Rc<RefCell<Vec<u8>>>,
}

impl Answering {
    pub(super) fn new(
        columns: NonZeroU16,
        rows: NonZeroU16,
        scrollback_bytes: usize,
    ) -> Result<Answering, Error> {
        let mut terminal = Terminal::new(columns.get(), rows.get())?;
        terminal.set_scrollback_max_bytes(Some(scrollback_bytes))?;
        let answers = Rc::new(RefCell::new(Vec::new()));
        let written = Rc::clone(&answers);
        terminal.on_pty_write(move |_, bytes| written.borrow_mut().extend_from_slice(bytes))?;
        Ok(Answering { terminal, answers })
    }

    /// Runs `drive` on the terminal, and gives back what it made and what the
    /// terminal answered its program meanwhile.
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
