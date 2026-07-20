//! Build script for the `opamp` crate (ADR-0004).
//!
//! Compiles the vendored OpAMP protobuf definitions into Rust types. `protox` parses the `.proto`
//! files in pure Rust — so no system `protoc` is required — and hands prost-build a
//! `FileDescriptorSet` to generate from.

fn main() {
    // The include root is `proto/`; the entry file's import (`opamp/v1/anyvalue.proto`) resolves
    // relative to it.
    println!("cargo:rerun-if-changed=proto");

    let file_descriptors = protox::compile(["opamp/v1/opamp.proto"], ["proto"])
        .expect("failed to compile the vendored OpAMP protobuf definitions");

    prost_build::Config::new()
        .compile_fds(file_descriptors)
        .expect("failed to generate Rust types from the OpAMP protobuf definitions");
}
