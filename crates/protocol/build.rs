const PROTO: &str = "../../proto/lune/v1/daemon.proto";

const MUX: &str = "../../proto/lune/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO}");
    println!("cargo:rerun-if-changed={MUX}");
    prost_build::compile_protos(&[PROTO], &["../../proto"])
}
