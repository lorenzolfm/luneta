use lune_domain::mux::TerminalKey;

use crate::mux::own::api::Dumped;
use crate::mux::own::server::Terminals;

pub fn terminal(key: TerminalKey) -> Result<String, String> {
    Ok(Terminals.dumped(key, Dumped::Screen)?)
}
