There are **two listeners, split by audience** (ADR-0012): the one the fleet talks to, and the one
you talk to.

**The Agent plane** — `listen`, `0.0.0.0:4320` by default:

**The Operator plane** — `[rest] listen`, `127.0.0.1:4321` by default:

| Path | What it is |
|---|---|
| `/api/v1/openapi.json` | The OpenAPI document — the contract to generate a client from. It describes this plane, so the artifact download above is not in it. |
and re-package the whole fleet. Reach it from another host through an SSH tunnel

| `listen` | `"0.0.0.0:4320"` | The **Agent plane**, as `address:port`: the OpAMP endpoint and the package downloads. `4320` is the protocol's default port. |
| `advertised_url` | unset | The absolute base URL advertised for package downloads. Leave it unset in the ordinary case: the Client then resolves the offered path against its own OpAMP endpoint, which is exactly where the download is served. Set it only when downloads must go through a different host, such as a mirror. |

### `[rest]`

The Operator plane's listener. Absent means the default.

```toml
[rest]
listen = "127.0.0.1:4321"   # "0.0.0.0:4321" publishes the REST API and the UI to the network
```

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"127.0.0.1:4321"` | Where the REST API, the API docs, and the UI are served. It must differ from `listen` above — two equal addresses are refused at startup by name, rather than surfacing later as *address already in use*. |
Present means **both listeners** serve HTTPS and WSS instead of plain HTTP and WS, with the same
certificate and key. `cert_file` and `key_file` are required together; `client_ca_file` is optional,
belongs to the Agent plane alone, and turns on mutual TLS (see
[Mutual TLS](#mutual-tls-proving-who-is-on-the-connection)).
That is deliberate: the Agent plane also serves the package download, and a Client fetching an
artifact presents no certificate — the content hash and the signature are what protect those bytes. A certificate that *is* presented is always verified — rustls refuses one it cannot
       http://127.0.0.1:4321/api/v1/configurations/linux-prod
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/linux-prod/rollout
       http://127.0.0.1:4321/api/v1/configurations/ruleset
       http://127.0.0.1:4321/api/v1/agents/<instance-uid>/labels
`[tls]` turns **both listeners** into HTTPS/WSS listeners, with one certificate and key — there is
no plaintext port left open beside either of them. Clients then use `wss://` or `https://` endpoints, and
