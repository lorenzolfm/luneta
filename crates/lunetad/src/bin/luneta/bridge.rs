use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use lunetad::BRIDGED;
use tokio::time::timeout;

use crate::commands;
use crate::daemon;

const BUFFER: usize = 64 * 1024;

const REACHING: Duration = Duration::from_secs(6);

const _: () = assert!(daemon::START.as_millis() < REACHING.as_millis());
const _: () = assert!(
    REACHING.as_millis() + daemon::VERSION_LIMIT.as_millis() < lunetad::REACH.as_millis()
);

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

fn reached(path: &Path) -> Result<UnixStream, String> {
    let runtime = commands::runtime()?;
    let socket = runtime.block_on(async {
        let reaching = timeout(REACHING, daemon::current(path)).await;
        let late = |_| format!("no lunetad answered in {}s", REACHING.as_secs());
        let daemon::Current { folder, .. } = reaching.map_err(late)??;
        let socket = lunetad::stream(&folder).await;
        socket.map_err(|error| format!("{}: {error}", folder.socket().display()))
    })?;
    let socket = socket.into_std().and_then(|socket| {
        socket.set_nonblocking(false)?;
        Ok(socket)
    });
    socket.map_err(|error| format!("{}: {error}", path.display()))
}

fn handed_on(socket: UnixStream) -> io::Result<()> {
    let mut stdin = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut stdout = File::from(io::stdout().as_fd().try_clone_to_owned()?);
    if let Err(error) = stdout.write_all(BRIDGED) {
        return hung_up(error);
    }
    let mut up = socket.try_clone()?;
    let mut down = socket;
    let (ended, end) = mpsc::channel();
    let ending = ended.clone();
    thread::spawn(move || ending.send(copied(&mut stdin, &mut up)));
    thread::spawn(move || ended.send(copied(&mut down, &mut stdout)));
    end.recv().unwrap_or_else(|_| Err(io::Error::other("both ways of the bridge panicked")))
}

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

fn hung_up(error: io::Error) -> io::Result<()> {
    match error.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => Ok(()),
        _ => Err(error),
    }
}
