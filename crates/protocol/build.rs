//! Writes the protocol's messages out as Rust, from `proto/` at the root of the
//! repository. prost-build runs `protoc`, which the flake puts on the `PATH`.

const PROTO: &str = "../../proto/luneta/v1/daemon.proto";

fn main() -> std::io::Result<()> {
    // The proto is outside the package, where cargo does not look for changes
    // on its own; prost-build says nothing of it either.
    println!("cargo:rerun-if-changed={PROTO}");
    prost_build::compile_protos(&[PROTO], &["../../proto"])
}
