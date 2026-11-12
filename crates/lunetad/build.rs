//! Writes the protocol's service out as Rust, from `proto/` at the root of the
//! repository. Its messages are `luneta-protocol`'s, and are not written out
//! again. And the client of `luneta-mux`'s protocol, which the daemon speaks
//! to the own sessions' server. tonic-prost-build runs `protoc`, which the
//! flake puts on the `PATH`.

const MUX: &str = "../../proto/luneta/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    tonic_prost_build::configure()
        .extern_path(".luneta.v1", "::luneta_protocol::v1")
        .compile_protos(&["../../proto/luneta/v1/daemon.proto"], &["../../proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&[MUX], &["../../proto"])
}
