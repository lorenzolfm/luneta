const MUX: &str = "../../proto/luneta/mux/v1/mux.proto";

fn main() -> std::io::Result<()> {
    tonic_prost_build::configure()
        .extern_path(".luneta.v1", "::luneta_protocol::v1")
        .compile_protos(&["../../proto/luneta/v1/daemon.proto"], &["../../proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&[MUX], &["../../proto"])
}
