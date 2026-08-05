// Generates the OpAMP protobuf types at build time from the vendored, pinned schema (ADR-0006).
//
// The schema files are byte-identical copies of the upstream `opamp-spec` tag named by BASELINE —
// the Protocol Baseline of docs/CONFORMANCE.md. Compilation is pure Rust (protox feeding
// prost-build), so no system `protoc` exists anywhere in the build chain, and the build never
// reaches the network: changing the wire format means changing a file in this repository.

/// The Protocol Baseline. The single place the proto path derives from, which is what kept
/// upstream's relocation of the files (`proto/` to `proto/opamp/v1/`, adopted with `v0.19.0`) a
/// change to this file alone — docs/CONFORMANCE.md requires it stay that way.
const BASELINE: &str = "v0.19.0";

fn main() {
    // The include root: `opamp.proto` imports its sibling as `opamp/v1/anyvalue.proto`, so the
    // import only resolves when the root is the directory *above* `opamp/v1`, never the directory
    // holding the files.
    let root = format!("proto/{BASELINE}");
    let files = [
        format!("{root}/opamp/v1/opamp.proto"),
        format!("{root}/opamp/v1/anyvalue.proto"),
    ];

    let descriptors = protox::compile(&files, [&root]).expect("compile OpAMP protobuf schema");
    prost_build::Config::new()
        .compile_fds(descriptors)
        .expect("generate Rust types from the OpAMP schema");

    for file in files {
        println!("cargo:rerun-if-changed={file}");
    }
}
