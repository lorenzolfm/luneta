//! PROTOTYPE, throwaway (#331 step 2): does a server that emulates each pane,
//! and clients that draw only the grid it sends, keep Claude Code, nvim and
//! fish working? Never merged; kept on its own branch as the evidence.
//!
//! `proto-mux [NAME] [-- COMMAND…]` attaches to session NAME ("proto" by
//! default), starting the server and the session as needed. `proto-mux ls`,
//! `proto-mux kill NAME`. The socket and the log are in `$XDG_RUNTIME_DIR`, or
//! where `PROTO_MUX_DIR` says.

mod client;
#[path = "../../lunetad/src/bin/luneta/keys.rs"]
mod keys;
mod server;
mod wire;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let dir = std::env::var_os("PROTO_MUX_DIR")
        .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let socket = dir.join("proto-mux.sock");
    let log = dir.join("proto-mux.log");
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let command = match args.iter().position(|a| a == "--") {
        Some(at) => { let c = args.split_off(at + 1); args.pop(); c },
        None => Vec::new(),
    };
    let result = match args.first().map(String::as_str) {
        Some("server") => server::run(&socket, &log),
        Some("ls") => list(&socket, wire::ToServer::List),
        Some("kill") => list(&socket, wire::ToServer::Kill(args.get(1).cloned().unwrap_or_default())),
        name => client::attach(&socket, &log, name.unwrap_or("proto"), command),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("proto-mux: {error}");
            ExitCode::FAILURE
        },
    }
}

fn list(socket: &std::path::Path, ask: wire::ToServer) -> std::io::Result<()> {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        println!("no server");
        return Ok(());
    };
    ask.send(&mut stream)?;
    if let (wire::ToClient::Sessions(names), _) = wire::ToClient::receive(&mut stream)? {
        for name in names {
            println!("{name}");
        }
    }
    Ok(())
}
