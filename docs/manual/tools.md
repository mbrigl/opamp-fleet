| **[`opamp-package-fetch`](#opamp-package-fetch)** | fetch a release of a known agent — the OpenTelemetry Collector, the GLPI Agent, Telegraf, Icinga 2, or this fleet's own Client — verify it, and hand it to the Server with its default configuration |
**A release does not ship them.** The `.tar.gz` artifacts and the `.deb`/`.rpm`/`.msi` installers
For six agent types it knows where the release lives, what the assets are called, and which
| `supervisor` | `mbrigl/opamp-fleet` — this project's own releases | the `.tar.gz` this fleet's Client is released as, one per platform, as published. It is the package a Client updates *itself* from ([ADR-0020](../adr/0020-the-client-updates-itself-from-a-signed-package.md)); the `.deb`, `.rpm` and `.msi` beside it are for installing a Client by hand and are passed over |
  supervisor       linux/amd64+arm64 darwin/arm64+amd64 windows/amd64
Server base URL› http://127.0.0.1:4321
The version list is the **three most recent release series**, newest first — a series being a
`major.minor` line, of which only its newest patch is offered. That is the difference between a
list you can go back through and one you cannot: Icinga 2 published five 2.16 patches, and offering
tags would have filled the whole list with that one line while hiding 2.15 and 2.14 entirely. An
older patch of a series already on the list is not a choice anyone makes, so it is not offered;
where an agent versions in two parts and has no patch to collapse (GLPI's `1.15`), the list is
simply the last three versions. Release candidates and the tags a repository keeps for other things
(the Collector repository tags its builder alongside) are filtered out.
      --out-dir ./artifacts --server http://127.0.0.1:4321
| `--agent <name>` | `otelcol`, `otelcol-contrib`, `glpi-agent`, `telegraf`, `icinga2`, or `supervisor` (this fleet's own Client). |
For `--agent supervisor` there is no block to print — nothing supervises a Client — so the hint is
the consent that lets it take the package over itself, which is also its default
([ADR-0020](../adr/0020-the-client-updates-itself-from-a-signed-package.md)):

```
Done. What a Client needs to take these:
  linux/amd64  ./artifacts/supervisor_1.2.3_linux_amd64.tar.gz
      [self_update] package = "supervisor"  (the default)
```

| `supervisor` | none | — a Client is configured by `supervisor.toml` on its own host, and the fleet owns only its `[[supervisor]]` blocks ([ADR-0032](../adr/0032-a-host-can-keep-its-supervisor-set-from-the-server.md)) |
### When an upload is refused

The Server decides some uploads before it reads a byte of the artifact, and says which:

| What it answers | What to do |
|---|---|
| `413 …` | The artifact is past `max_package_size_bytes`. |

Because the refusal arrives while the artifact is still being sent, the connection can reset before
the answer is read; the tool then asks the Server once more with an empty body to recover the reason,
so what you see is the status and message above rather than a bare connection error. A message that
does still begin `cannot reach` is what it says — the Server was not answering — and it carries the
underlying cause (DNS, refused connection, TLS) rather than only the request that failed.

$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
```

