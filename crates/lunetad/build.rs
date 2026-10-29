//! Writes the protocol's service out as Rust, from `proto/` at the root of the
//! repository. Its messages are `luneta-protocol`'s, and are not written out
//! again. tonic-prost-build runs `protoc`, which the flake puts on the `PATH`.

fn main() -> std::io::Result<()> {
    tonic_prost_build::configure()
        .extern_path(".luneta.v1", "::luneta_protocol::v1")
        .compile_protos(&["../../proto/luneta/v1/daemon.proto"], &["../../proto"])
}
