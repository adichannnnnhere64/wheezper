//! Codegen for the versioned wheezper.v1 contract.
//!
//! `protoc-bin-vendored` keeps the build self-contained: no system protoc and
//! no Android/NDK toolchain needed just to compile the Rust client or server.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // prost-build shells out to `protoc`; point it at the vendored binary so a
    // plain `cargo build` works on machines without protobuf installed.
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let protos = [
        "wheezper/v1/session.proto",
        "wheezper/v1/transcript.proto",
        "wheezper/v1/assistant.proto",
    ]
    .iter()
    .map(|p| root.join(p))
    .collect::<Vec<_>>();

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        // Android gets its own generated Java/Kotlin stubs from the same files.
        .compile_protos(&protos, std::slice::from_ref(&root))?;

    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
