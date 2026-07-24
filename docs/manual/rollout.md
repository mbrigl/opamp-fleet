- **One file by default, a whole tree when you say so.** A statically linked single binary —
  Promtail, Vector, a Go or Rust agent — needs nothing extra. An agent that is an executable *plus*
  the shared objects it loads, such as Fluent Bit, needs `program_path` in its block, which unpacks
  [Agents that are more than one file](client.md#agents-that-are-more-than-one-file). Everything
  below applies to both.
```

Their values are what an Agent reports as `os.type` and `host.arch` — `linux`, `darwin`, `windows`
and `amd64`, `arm64`. The tokens off an upstream release file name work too (`macos` is `darwin`,
`x86_64` is `amd64`), and the response says which canonical pair was stored.


```console
$ curl -X PUT --data-binary @promtail-3.0.0-linux-arm64.tar.gz \
       http://127.0.0.1:4321/api/v1/agents/<instance_uid>/rollout
       -d '{"service_name": "promtail", "selector": {"env": "canary"}, "body": "server:\n  http_listen_port: 9080\n"}' \
       http://127.0.0.1:4321/api/v1/configurations/promtail-conf
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/promtail-conf/rollout
$ curl -s http://127.0.0.1:4321/api/v1/agents | jq '.[] | select(.service_name=="promtail")'
| `InstallFailed`, "holds no member at …" | A tree package whose `program_path` names nothing in the archive. The error lists what it holds — check the path from its end, not from the archive root. |
| `InstallFailed`, "matches N members" | `program_path` is ambiguous; write more of the path. |
| `InstallFailed`, "climbs out" / "is an absolute path" / "not a file or a directory" | The archive carries a member this Client will not write — a `..` path, an absolute one, or a link. Nothing was unpacked and the running tree is untouched. |
| An upload answers `400`, "invalid platform" | `os`/`arch` are required and must be file-name-safe: lowercase letters, digits and `_`, at most 16 characters. |
| An Agent that accepts packages is offered nothing, and there is no conflict | No artifact for its platform. Check its `os.type` and `host.arch` on its fleet row against the platforms the package holds — this is the case the whole mechanism exists to make visible rather than fatal. |
