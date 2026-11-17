use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

use tonic::Status;

use crate::asked::refused;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalId(NonZeroU32);

impl TerminalId {
    pub fn parse(field: &str, raw: u32) -> Result<TerminalId, Status> {
        NonZeroU32::new(raw).map(TerminalId).ok_or_else(|| refused(field, "is missing"))
    }

    pub fn next(last: &mut Option<TerminalId>) -> TerminalId {
        let next = last.map_or(NonZeroU32::MIN, |TerminalId(last)| {
            last.checked_add(1).expect("TerminalIds last")
        });
        *last = Some(TerminalId(next));
        TerminalId(next)
    }

    #[must_use]
    pub fn said(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(NonZeroU64);

impl ClientId {
    pub fn next(last: &mut Option<ClientId>) -> ClientId {
        let next = last.map_or(NonZeroU64::MIN, |ClientId(last)| {
            last.checked_add(1).expect("ClientIds last")
        });
        *last = Some(ClientId(next));
        ClientId(next)
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_never_0() {
        assert_eq!(TerminalId::parse("terminal", 7).unwrap().said(), 7);
        let missing = TerminalId::parse("terminal", 0).unwrap_err();
        assert_eq!(missing.message(), "terminal is missing");
    }

    #[test]
    fn ids_count_up_from_1() {
        let mut last = None;
        assert_eq!(TerminalId::next(&mut last).said(), 1);
        assert_eq!(TerminalId::next(&mut last).said(), 2);
        assert_eq!(last.map(TerminalId::said), Some(2));
        let mut clients = None;
        assert_eq!(ClientId::next(&mut clients).to_string(), "1");
        assert_eq!(ClientId::next(&mut clients).to_string(), "2");
    }
}
