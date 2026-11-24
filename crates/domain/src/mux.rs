use std::num::NonZeroU32;

pub use lune_protocol::made::Made as OwnMade;

use lune_protocol::mux::v1 as mux;
use lune_protocol::v1 as api;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalKey {
    pub id: NonZeroU32,
    pub made: OwnMade,
}

impl TerminalKey {
    pub fn read(key: Option<mux::TerminalKey>) -> Result<Self, String> {
        let key = key.ok_or("a terminal with no key")?;
        let id = NonZeroU32::new(key.id).ok_or("0 is no terminal's id")?;
        let made = key.made_at.ok_or_else(|| format!("terminal {id} has no made_at"))?;
        let made = OwnMade::read(made).map_err(|why| format!("terminal {id}'s made_at: {why}"))?;
        Ok(TerminalKey { id, made })
    }

    pub fn of(key: &api::TerminalKey) -> Result<Self, String> {
        let id = NonZeroU32::new(key.id).ok_or("0 is no terminal's id")?;
        let made = key.made_at.ok_or_else(|| format!("terminal {id} has no made_at"))?;
        let made = OwnMade::read(made).map_err(|why| format!("terminal {id}'s made_at: {why}"))?;
        Ok(TerminalKey { id, made })
    }

    #[must_use]
    pub fn said(self) -> mux::TerminalKey {
        mux::TerminalKey { id: self.id.get(), made_at: Some(self.made.into()) }
    }
}

impl From<TerminalKey> for api::TerminalKey {
    fn from(key: TerminalKey) -> Self {
        api::TerminalKey { id: key.id.get(), made_at: Some(key.made.into()) }
    }
}
