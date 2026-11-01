//! A way to the daemon of this host for a client on another, as
//! `ssh <host> luneta bridge`, with ssh to let it in and nothing that listens
//! on the network. luneta reaches the daemon as `luneta watch` does, started
//! where none runs and in the place of an older one, and then hands on what
//! comes on its stdin to the daemon's socket, and what comes on the socket to
//! its stdout, as it comes. It reads none of it, so that a client and a daemon
//! of two builds speak to each other as they would on the socket.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

use lunetad::folder;

use crate::commands;
use crate::daemon::{self, Reach};

/// How much luneta hands on at once, at the most.
const BUFFER: usize = 64 * 1024;

/// Reaches the daemon in `folder`, and hands on between it and luneta's stdin
/// and stdout until either end hangs up. What goes wrong goes to stderr, as
/// stdout is the client's.
pub fn run(folder: &Path) -> ExitCode {
    let bridged = reached(folder).and_then(|socket| {
        handed_on(socket).map_err(|error| format!("{}: {error}", folder.display()))
    });
    match bridged {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("luneta: {why}");
            ExitCode::FAILURE
        },
    }
}

/// A connection to the daemon in the folder at `path`, started where none
/// runs, or in the place of an older one.
fn reached(path: &Path) -> Result<UnixStream, String> {
    let runtime = commands::runtime()?;
    let socket = runtime.block_on(async {
        daemon::reach(path, Reach::Current).await?;
        // The daemon made the folder as it started, if luneta had not.
        let folder = folder::open(path).map_err(|refusal| refusal.to_string())?;
        let folder = folder.ok_or_else(|| format!("{} went as lunetad answered", path.display()))?;
        let socket = lunetad::stream(&folder).await;
        socket.map_err(|error| format!("{}: {error}", folder.socket().display()))
    })?;
    // The runtime goes, and the socket is read and written on threads that
    // wait on it.
    let socket = socket.into_std().and_then(|socket| {
        socket.set_nonblocking(false)?;
        Ok(socket)
    });
    socket.map_err(|error| format!("{}: {error}", path.display()))
}

/// Hands on what comes from either end to the other, until either hangs up.
/// What the other has not taken yet then goes nowhere: the one that hung up
/// would have had it.
fn handed_on(socket: UnixStream) -> io::Result<()> {
    let mut stdin = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut stdout = File::from(io::stdout().as_fd().try_clone_to_owned()?);
    let mut up = socket.try_clone()?;
    let mut down = socket;
    let (ended, end) = mpsc::channel();
    let ending = ended.clone();
    thread::spawn(move || ending.send(copied(&mut stdin, &mut up)));
    thread::spawn(move || ended.send(copied(&mut down, &mut stdout)));
    // A thread that panicked sent nothing, and the other one ends.
    end.recv().unwrap_or(Ok(()))
}

/// Copies what `from` gives to `to`, until either hangs up: `from` with its
/// end, `to` by closing its own.
fn copied(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buffer = vec![0; BUFFER];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return hung_up(error),
        };
        if let Err(error) = to.write_all(&buffer[..read]) {
            return hung_up(error);
        }
    }
}

/// `error`, unless it says that the other end hung up.
fn hung_up(error: io::Error) -> io::Result<()> {
    match error.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => Ok(()),
        _ => Err(error),
    }
}
