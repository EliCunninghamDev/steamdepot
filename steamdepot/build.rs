use std::path::PathBuf;

const PROTOS: &[&str] = &[
    "proto/steam/steammessages_base.proto",
    "proto/steam/steammessages_clientserver_login.proto",
    "proto/steam/steammessages_clientserver_appinfo.proto",
    "proto/steam/steammessages_clientserver_2.proto",
    "proto/steam/steammessages_unified_base.steamclient.proto",
    "proto/steam/steammessages_contentsystem.steamclient.proto",
    "proto/steam/steammessages_auth.steamclient.proto",
    "proto/steam/content_manifest.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a pinned, vendored protoc so builds do not depend on a system
    // `protobuf-compiler` being installed (or on its version).
    let protoc: PathBuf = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=proto/steam");
    for proto in PROTOS {
        println!("cargo:rerun-if-changed={proto}");
    }

    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    config.compile_protos(PROTOS, &["proto/steam"])?;

    Ok(())
}
