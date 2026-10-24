//! How a client reaches the daemon.

use std::path::Path;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Error, Uri};
use tower::service_fn;

use crate::api::daemon_client::DaemonClient;

/// How long a client waits for the socket to take it. A daemon on the same
/// host takes it at once, or not at all.
const CONNECT: Duration = Duration::from_secs(1);

/// How long a client waits for an answer: a daemon that has none in this time
/// is stuck.
const ANSWER: Duration = Duration::from_secs(5);

/// A client of the daemon that listens on `socket`.
pub async fn connect(socket: &Path) -> Result<DaemonClient<Channel>, Error> {
    let socket = socket.to_owned();
    // The URI goes into each request's `:authority`, and nowhere else: the
    // connector is what reaches the socket, so that a path is a path and never
    // has to survive being spelled as a URI.
    let channel = Endpoint::from_static("http://lunetad")
        .connect_timeout(CONNECT)
        .timeout(ANSWER)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { UnixStream::connect(socket).await.map(TokioIo::new) }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}
