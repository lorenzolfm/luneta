const PROTO: &str = "../../proto/luneta/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO}");
    tonic_prost_build::configure()
        .extern_path(".luneta.mux.v1", "::luneta_protocol::mux::v1")
        .compile_protos(&[PROTO], &["../../proto"])
}
