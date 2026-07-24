1. **Start the Server.** It serves two planes on two ports: the **Agent plane** on `4320` (the
   OpAMP endpoint at `/v1/opamp` and the package downloads), and the **Operator plane** on
   `127.0.0.1:4321` (the REST API under `/api/v1/`, the API docs at `/api/v1/docs`, and the bundled
   UI at `/`). The operator half is on loopback because nothing authenticates it yet.
3. **Open the UI** at <http://127.0.0.1:4321/>. The Agent is listed as *Connected*, with the
          http://127.0.0.1:4321/api/v1/configurations/base
   $ curl -X POST http://127.0.0.1:4321/api/v1/configurations/base/rollout
  its status report, that the Client dropped them. Mutual TLS itself *is* built: see
  [the Server](server.md#mutual-tls-proving-who-is-on-the-connection).
  renewal is what bounds an issued certificate.
