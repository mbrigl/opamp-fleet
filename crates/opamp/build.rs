// Generates the OpAMP protobuf types at build time from the vendored, pinned schema (ADR-0006).
//
// The schema files are byte-identical copies of the upstream `opamp-spec` tag named by BASELINE —
// the Protocol Baseline of docs/CONFORMANCE.md. Compilation is pure Rust (protox feeding
// prost-build), so no system `protoc` exists anywhere in the build chain, and the build never
// reaches the network: changing the wire format means changing a file in this repository.

/// The Protocol Baseline. The single place the proto path derives from — upstream relocated the
/// files after this tag, and docs/CONFORMANCE.md requires that adopting such a move stays a
/// one-line change here.
const BASELINE: &str = "v0.18.0";

fn main() {
    let root = format!("proto/{BASELINE}");
    let files = [
        format!("{root}/opamp.proto"),
        format!("{root}/anyvalue.proto"),
    ];

    let descriptors = protox::compile(&files, [&root]).expect("compile OpAMP protobuf schema");
    prost_build::Config::new()
        .compile_fds(descriptors)
        .expect("generate Rust types from the OpAMP schema");

    for file in files {
        println!("cargo:rerun-if-changed={file}");
    }
}
