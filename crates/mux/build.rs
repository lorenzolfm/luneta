const PROTO: &str = "../../proto/lune/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO}");
    tonic_prost_build::configure()
        .extern_path(".lune.mux.v1", "::lune_protocol::mux::v1")
        .compile_protos(&[PROTO], &["../../proto"])
}
