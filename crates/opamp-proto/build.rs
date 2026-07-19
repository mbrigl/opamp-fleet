// Generates the OpAMP Protobuf types at build time from the vendored, pinned schema (ADR-0006).
//
// The schema is a copy of the OpAMP specification's own .proto, kept under proto/ so the build never
// reaches the network and the generated types cannot drift from the spec silently: changing the
// message wire format means changing a file in this repository. `protoc` is required (prost-build
// invokes it) and is provided by the Dev Container and CI.
fn main() {
    let protos = [
        "proto/opamp/v1/opamp.proto",
        "proto/opamp/v1/anyvalue.proto",
    ];

    prost_build::compile_protos(&protos, &["proto/"]).expect("compile OpAMP protobuf schema");

    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }
}
