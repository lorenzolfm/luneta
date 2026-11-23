use crate::mux::own::api::Dumped;
use crate::mux::own::server::Terminals;
use crate::mux::TerminalKey;

pub fn terminal(key: TerminalKey) -> Result<String, String> {
    Ok(Terminals.dumped(key, Dumped::Screen)?)
}
