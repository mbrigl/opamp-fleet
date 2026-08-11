| **[Rollout walkthrough](rollout.md)** | both ends at once, end to end: build an artifact, sign it, upload it, aim it, and watch a Foreign Agent be installed and configured entirely from the Server |
$ cargo run -p client -- --config config/supervisor.toml
1. **Start the Server.** It serves two planes on two ports: the **Agent plane** on `4320` (the
   OpAMP endpoint at `/v1/opamp` and the package downloads), and the **Operator plane** on
   `127.0.0.1:4321` (the REST API under `/api/v1/`, the API docs at `/api/v1/docs`, and the bundled
   UI at `/`). The operator half is on loopback because nothing authenticates it yet.
   $ cargo run -p client -- --config config/supervisor.toml
3. **Open the UI** at <http://127.0.0.1:4321/>. The Agent is listed as *Connected*, with the
4. **Create and roll out a Configuration.** In the UI, press **Configurations**, give it a name,
   then press **Roll out to all matching**, because saving only stores; the rollout act is what
   reaches the fleet. The same two steps over the API:
          http://127.0.0.1:4321/api/v1/configurations/base
   $ curl -X POST http://127.0.0.1:4321/api/v1/configurations/base/rollout
   effective configuration appears in the fleet table. Rolling the same Configuration out again
   sends nothing — every push is gated on a content hash. An Agent that connects *later* is not
   changed by the earlier act: its row on the Agents tab shows the Configuration waiting, with a
   **roll out** control of its own.
**Attributes.** Every Agent reports attributes — `service.name`, `service.instance.name`,
`service.version`, `service.instance.id`, `os.type`, `os.name`, `os.version`, `os.description`,
`host.name`, `host.arch`, `host.id` — and an operator can add more in `supervisor.toml`, plus
`service.namespace` where a deployment uses one. These are what Selectors match on. An attribute the
host cannot answer is absent rather than empty.

`service.name` is the Agent **type** — `otelcol-contrib`, `promtail`, `supervisor` for the Client's
own Agent — the
same value on every host running that kind of agent, while `service.instance.name` is **your** name
for one Agent, the `[[supervisor]]` block's `name`. Aim at the type to reach every Agent of a kind,
at the instance name to reach exactly one.
[`config/supervisor.toml`](../../config/supervisor.toml).
  its status report, that the Client dropped them. Mutual TLS itself *is* built: see
  [the Server](server.md#mutual-tls-proving-who-is-on-the-connection).
  renewal is what bounds an issued certificate.
- **Custom messages** (`CustomCapabilities` / `CustomMessage`) — planned, not implemented.
- **Other connection settings** (`AcceptsOtherConnectionSettings`) — deliberately not implemented:
  the protocol leaves their meaning entirely to the Agent, so honouring the capability would mean
  inventing semantics.
