//! Writes the protocol out as Rust, from `proto/` at the root of the
//! repository. tonic-prost-build runs `protoc`, which the flake puts on the
//! `PATH`.

fn main() -> std::io::Result<()> {
    tonic_prost_build::configure()
        .compile_protos(&["../../proto/luneta/v1/daemon.proto"], &["../../proto"])
}
