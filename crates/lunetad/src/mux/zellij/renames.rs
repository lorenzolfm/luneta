use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Instant;

use super::cli::{session_renamed, ANSWER};
use super::{SessionName, SocketFolder};

const RELIST: [u8; 7] = [3, 0, 0, 0, 0xca, 0x01, 0];

const CONN_STATUS: [u8; 6] = [2, 0, 0, 0, 0x6a, 0];

const CONNECTED: [u8; 2] = [0x22, 0];

pub fn renamed(session: &SessionName, to: &SessionName) -> Result<(), String> {
    relisted_in(&super::folder(), session)?;
    Ok(session_renamed(session, to)?)
}

fn relisted_in(folder: &SocketFolder, session: &SessionName) -> Result<(), String> {
    let socket = folder.socket_of(session);
    let failed = |error: io::Error| format!("zellij did not list its sessions again: {error}");
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failed(error)),
    };
    stream.set_read_timeout(Some(ANSWER)).map_err(failed)?;
    stream.write_all(&RELIST).map_err(failed)?;
    stream.write_all(&CONN_STATUS).map_err(failed)?;
    connected(&mut stream, Instant::now() + ANSWER).map_err(failed)
}

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

    use super::super::SPOKEN;
    use super::*;

    #[test]
    fn a_rename_lists_the_sessions_again_only_where_a_socket_of_the_contract_has_its_name() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let Ok(session) = SessionName::running("work") else {
            panic!("a name zellij takes");
        };
        assert_eq!(relisted_in(&SocketFolder::at(scratch.path()), &session), Ok(()));
        let contract = scratch.path().join(SPOKEN);
        assert!(fs::create_dir(&contract).is_ok());
        assert!(UnixListener::bind(contract.join("work")).is_ok());
        let refused = relisted_in(&SocketFolder::at(scratch.path()), &session);
        assert!(refused.as_ref().is_err_and(|why| why.contains("did not list")), "{refused:?}");
    }

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
        assert_eq!(relisted_in(&SocketFolder::at(scratch.path()), &session), Ok(()));
        let hung_up = relisted_in(&SocketFolder::at(scratch.path()), &session);
        assert!(hung_up.as_ref().is_err_and(|why| why.contains("it hung up")), "{hung_up:?}");
        assert!(server.join().is_ok());
    }
}
