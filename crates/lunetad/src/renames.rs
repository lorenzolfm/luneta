//! What zellij renames of the sessions of the machine it runs on, for
//! luneta's picker. luneta has it rename one of its own machine, and a
//! daemon has it rename one here for a luneta on another, whose picker
//! renames a session of this one.

use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

use crate::cli::{session_renamed, ANSWER};
use crate::zellij::{self, SessionName, SPOKEN};

/// What zellij's web client says to its server to have it read again which
/// sessions run and which ended, framed as zellij frames a message: four
/// bytes of length, low first, then a `ClientToServerMsg` of zellij-utils'
/// `client_to_server.proto` that holds an empty `RequestSessionList`, its
/// field 25.
const RELIST: [u8; 7] = [3, 0, 0, 0, 0xca, 0x01, 0];

/// What zellij's CLI says to the server of each session to learn that it
/// runs, so framed: a `ClientToServerMsg` that holds an empty `ConnStatus`,
/// its field 13.
const CONN_STATUS: [u8; 6] = [2, 0, 0, 0, 0x6a, 0];

/// What the server answers `CONN_STATUS` with, to the client that sent it
/// alone, without the length before it: a `ServerToClientMsg` that holds an
/// empty `Connected`, its field 4.
const CONNECTED: [u8; 2] = [0x22, 0];

/// Renames `session` to `to`, or says why zellij did not. A session with no
/// client takes the name as well as one with.
pub fn renamed(session: &SessionName, to: &SessionName) -> Result<(), String> {
    relisted_in(&zellij::folder(), session)?;
    Ok(session_renamed(session, to)?)
}

/// Has the server of `session`, where its socket is in `folder`, zellij's,
/// read again which sessions run and which ended, or says why it did not.
/// zellij 0.45.1 refuses to rename a session to a name among those it read
/// last, and reads them only as a web client or a plugin asks, so a session
/// renamed could not take its old name back: zellij still had it as its own.
/// The server takes a client's messages in turn, and answers `CONN_STATUS`,
/// sent after `RELIST`, to that client alone, so once it has read them and
/// handed them on, ahead of a rename asked after. What it sends the client
/// before that, as it sends some to every client it has, luneta reads past.
/// It skips `RELIST` where it cannot read it, though, and reads none before
/// its first client: then a name it had stays taken, and its refusal says
/// so, as it did before luneta asked this. Where no session of the contract
/// `RELIST` is of has the name, the rename is left to say why, as zellij's
/// CLI speaks its own.
fn relisted_in(folder: &Path, session: &SessionName) -> Result<(), String> {
    let socket = folder.join(SPOKEN).join(session.as_str());
    let failed = |error: io::Error| format!("zellij did not list its sessions again: {error}");
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        // One of a zellij of another contract, or one that ended.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failed(error)),
    };
    // macOS sets no option of a socket its peer closed, as the server closes
    // this one once it answered, though what it sent is still there to read.
    stream.set_read_timeout(Some(ANSWER)).map_err(failed)?;
    stream.write_all(&RELIST).map_err(failed)?;
    stream.write_all(&CONN_STATUS).map_err(failed)?;
    connected(&mut stream, Instant::now() + ANSWER).map_err(failed)
}

/// Reads the messages of the server on `stream`, each its length and then
/// itself, as zellij frames them, until it says `CONNECTED`, starting no
/// read past `deadline`; or says why it did not. Each read waits as long as
/// the stream's timeout at most.
fn connected(stream: &mut UnixStream, deadline: Instant) -> io::Result<()> {
    let mut part = [0; 4096];
    loop {
        let mut length = [0; 4];
        filled(stream, &mut length, deadline)?;
        let length = u32::from_le_bytes(length) as usize;
        let mut left = length;
        while left > 0 {
            let read = left.min(part.len());
            filled(stream, &mut part[..read], deadline)?;
            left -= read;
        }
        if length == CONNECTED.len() && part[..length] == CONNECTED {
            return Ok(());
        }
    }
}

/// Fills `buffer` with what comes next on `stream`, starting no read past
/// `deadline`, or says why it did not.
fn filled(stream: &mut UnixStream, buffer: &mut [u8], deadline: Instant) -> io::Result<()> {
    let late = || io::Error::new(io::ErrorKind::TimedOut, "it did not answer in time");
    let mut at = 0;
    while at < buffer.len() {
        if Instant::now() >= deadline {
            return Err(late());
        }
        match stream.read(&mut buffer[at..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "it hung up")),
            Ok(read) => at += read,
            // A signal luneta took on this thread: a read with a timeout is
            // not taken up again after one.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Err(late()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::thread;

    use super::*;

    /// Where no socket of zellij 0.45's contract has the name of the session
    /// a rename is of, the rename is left to say why: the session is of a
    /// zellij of another contract, or it ended. Where one has, and no server
    /// listens on it, luneta says so, and renames nothing.
    #[test]
    fn a_rename_lists_the_sessions_again_only_where_a_socket_of_the_contract_has_its_name() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let Ok(session) = SessionName::running("work") else {
            panic!("a name zellij takes");
        };
        assert_eq!(relisted_in(scratch.path(), &session), Ok(()));
        let contract = scratch.path().join(SPOKEN);
        assert!(fs::create_dir(&contract).is_ok());
        assert!(UnixListener::bind(contract.join("work")).is_ok());
        let refused = relisted_in(scratch.path(), &session);
        assert!(refused.as_ref().is_err_and(|why| why.contains("did not list")), "{refused:?}");
    }

    /// The rename goes on once the server answers the question zellij's CLI
    /// asks it, which it answers once it read again which sessions run and
    /// which ended, and to luneta alone: what it says before, to luneta among
    /// its other clients, luneta reads past. A server that hangs up first
    /// did not say.
    #[test]
    fn a_rename_goes_on_once_the_server_answers_past_what_else_it_says() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let Ok(session) = SessionName::running("work") else {
            panic!("a name zellij takes");
        };
        let contract = scratch.path().join(SPOKEN);
        assert!(fs::create_dir(&contract).is_ok());
        let Ok(listener) = UnixListener::bind(contract.join("work")) else {
            panic!("no socket");
        };
        let framed = |message: &[u8]| [&(message.len() as u32).to_le_bytes()[..], message].concat();
        // What the server answers each message with, and a screen to draw.
        let (unblocked, render) = (framed(&[0x12, 0]), framed(&[0; 10_000]));
        let answers = [vec![unblocked.clone(), render, framed(&CONNECTED)], vec![unblocked]];
        let server = thread::spawn(move || {
            for answer in answers {
                let Ok((mut client, _)) = listener.accept() else {
                    panic!("no client came");
                };
                let mut asked = [0; RELIST.len() + CONN_STATUS.len()];
                assert!(client.read_exact(&mut asked).is_ok());
                for message in answer {
                    assert!(client.write_all(&message).is_ok());
                }
            }
        });
        assert_eq!(relisted_in(scratch.path(), &session), Ok(()));
        let hung_up = relisted_in(scratch.path(), &session);
        assert!(hung_up.as_ref().is_err_and(|why| why.contains("it hung up")), "{hung_up:?}");
        assert!(server.join().is_ok());
    }
}
