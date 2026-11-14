const PROTO: &str = "../../proto/luneta/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO}");
    tonic_prost_build::configure().compile_protos(&[PROTO], &["../../proto"])
}
