fn main() -> std::io::Result<()> {
    tonic_prost_build::configure()
        .build_server(false)
        .extern_path(".lune.v1", "::lune_protocol::v1")
        .extern_path(".lune.mux.v1", "::lune_protocol::mux::v1")
        .compile_protos(&["../../proto/lune/v1/daemon.proto"], &["../../proto"])
}
