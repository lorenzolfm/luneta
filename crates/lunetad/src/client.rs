//! How a client reaches the daemon.

use std::io::{self, ErrorKind};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Error, Uri};
use tower::service_fn;

use crate::api::daemon_client::DaemonClient;
use crate::folder::Folder;

/// How long a client waits for the socket to take it. A daemon on the same
/// host takes it at once, or not at all.
const CONNECT: Duration = Duration::from_secs(1);

/// How long a client waits for an answer: a daemon that has none in this time
/// is stuck. The daemon gives up on a question as soon, too.
pub const ANSWER: Duration = Duration::from_secs(5);

/// A client of the daemon that listens in `folder`.
pub async fn connect(folder: &Folder) -> Result<DaemonClient<Channel>, Error> {
    let socket = folder.socket();
    // The URI goes into each request's `:authority`, and nowhere else: the
    // connector is what reaches the socket, so that a path is a path and never
    // has to survive being spelled as a URI.
    let channel = Endpoint::from_static("http://lunetad")
        .connect_timeout(CONNECT)
        .timeout(ANSWER)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { mine(UnixStream::connect(socket).await?).map(TokioIo::new) }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}

/// The pid of the process that listens in `folder`, as the kernel says: in
/// the namespace of pids that this process is in, and so none where that
/// process is in one this one cannot see. A daemon's own word for its pid is
/// of the namespace it is in.
pub async fn listening(folder: &Folder) -> io::Result<Option<u32>> {
    let stream = mine(UnixStream::connect(folder.socket()).await?)?;
    let pid = stream.peer_cred()?.pid().and_then(|pid| u32::try_from(pid).ok());
    // Linux says 0 for a process it cannot name in this namespace.
    Ok(pid.filter(|pid| *pid != 0))
}

/// `stream`, if this user's process is at its other end, as the kernel says:
/// whoever made the socket answers on it, whatever folder it turned up in,
/// and the daemon a client asks is told what it answers.
fn mine(stream: UnixStream) -> io::Result<UnixStream> {
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on lunetad's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}
