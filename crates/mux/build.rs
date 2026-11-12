//! Writes the server's protocol out as Rust, from `proto/` at the root of the
//! repository. tonic-prost-build runs `protoc`, which the flake puts on the
//! `PATH`.

const PROTO: &str = "../../proto/luneta/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO}");
    tonic_prost_build::configure().compile_protos(&[PROTO], &["../../proto"])
}
