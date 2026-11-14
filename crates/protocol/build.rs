const PROTO: &str = "../../proto/luneta/v1/daemon.proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO}");
    prost_build::compile_protos(&[PROTO], &["../../proto"])
}
