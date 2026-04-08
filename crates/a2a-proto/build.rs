fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                &format!("{proto_root}/local.proto"),
                &format!("{proto_root}/peer.proto"),
            ],
            &[proto_root],
        )?;
    Ok(())
}
