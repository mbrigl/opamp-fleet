# opamp

The wire layer of the [Open Agent Management Protocol](https://github.com/open-telemetry/opamp-spec)
(OpAMP) for Rust — what any OpAMP server or agent needs before it writes a line of its own:

- `opamp::proto` — the protobuf messages (`AgentToServer`, `ServerToAgent`, …), generated with
  [prost](https://crates.io/crates/prost) from the specification's own schema, which ships inside
  this crate. No system `protoc` is needed: the schema is compiled with
  [protox](https://crates.io/crates/protox).
- `opamp::frame` — the WebSocket message format (a varint header, then the protobuf message) and
  the specification's default message size limit.
- `opamp::endpoint` — the endpoint's path and media type, and decoding a plain-HTTP body: gzip
  as the specification requires, with the size limit applied after decompression.
- `opamp::uid` — the 16-byte `instance_uid`.
- `opamp::attributes` — the attribute keys an `AgentDescription` is read by, and accessors over
  them.

It does no I/O and carries no transport: WebSocket and HTTP machinery stay with the program that
uses it.

## Versions

The crate's version follows the specification's: `0.20.x` is generated from opamp-spec `v0.20.0`,
and `opamp::BASELINE` says so at run time. The patch number is the crate's own. A breaking change
— including a new `prost` minor version, since `prost` types are part of this API — comes only
with the next specification release.

This crate is developed as part of [OpAMP Fleet](https://github.com/mbrigl/opamp-fleet), whose
Server and Client are built on it.

## License

Apache-2.0. The vendored schema is the OpenTelemetry Authors', under the same licence; see
`NOTICE`.
