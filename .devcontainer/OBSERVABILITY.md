# Development observability stack

OTLP in, Grafana out — logs, metrics, and traces from the Agents' own telemetry (ADR-0036), stored
in one place, and up already whenever the Dev Container is. This is a **development tool**: nothing
shipped depends on it, it holds no credentials worth having, and it retains 24 h of data on a local
volume.

```
Agent ──OTLP/HTTP:4318──▶ OpenTelemetry Collector ──▶ ClickHouse ──▶ Grafana :3000
                                                       otel_traces
                                                       otel_logs
                                                       otel_metrics_gauge
```

## Why one store

This stack used to run Tempo, Prometheus and Loki side by side — one backend per signal, which is
the conventional shape. It is now ClickHouse alone. What that changes:

- **One query language.** Every panel is SQL. Before, the same dashboard mixed PromQL, LogQL and
  TraceQL, and knowing one told you nothing about the others.
- **Cross-signal questions become joins.** "Which log lines belong to the operation that failed" is
  a join between `otel_logs` and `otel_traces` on `TraceId`. Across three stores that is not a
  query at all — it is a Grafana datasource link that jumps you elsewhere and loses the rest of
  your filter.
- **One retention setting.** `ttl: 24h` in [otel-collector.yaml](otel-collector.yaml), instead of
  Prometheus' `--storage.tsdb.retention`, Loki's `retention_period` and Tempo's `block_retention`.
- **One process to run.** Four containers became three, and one of them is Grafana.

What it costs, stated plainly rather than discovered later:

- **The service map is gone.** Tempo's metrics-generator produced `traces_spanmetrics_*` and a
  service graph for free. ClickHouse computes the RED metrics from the spans themselves — that part
  is a `count()` and a `quantile()` — but nothing draws a node graph. For a fleet Client the loss is
  small: its spans are *phases of one operation*, not services calling each other, so the panel
  that replaced it groups child spans by how often they fail, which is the question that actually
  gets asked. On a stack tracing a real service mesh, this would be a genuine loss.
- **The datasource is a plugin.** `grafana-clickhouse-datasource` is not built into Grafana, so the
  first `up` needs network access to install it. It also requires **Grafana ≥ 11.6.0**, which is why
  the image here is newer than the one the three-store stack ran.
- **No PromQL.** Recording rules, alerting expressions and any dashboard copied from elsewhere have
  to be rewritten. Nothing here needed them; a stack that does should think twice.
- **Metrics are stored as rows, not as a TSDB.** ClickHouse is very good at this and 24 h of a small
  fleet is nothing to it. At fleet-wide scale over months, a purpose-built TSDB still wins on
  storage per sample.

## Start

There is nothing to start. The Dev Container **is** this Compose project
([`docker-compose.yml`](docker-compose.yml)): the workspace container VS Code attaches to is one of
its four services and these three are the others, so they come up with the container and go down
with it.

Grafana is on <http://localhost:3000> — anonymous access is enabled, so there is nothing to log in
to (`admin` / `admin` if you want to edit and save). It opens on **Fleet Agents — Overview**.

The first start pulls the ClickHouse datasource plugin, so give Grafana a few seconds longer than
usual and make sure the machine has network access. Without it, Grafana starts with no datasource
and every panel reports one.

To leave the stack out of the next start, drop the names from `runServices` in
[`devcontainer.json`](devcontainer.json) — from inside the container there is no daemon to start
them by hand, so that list is the on/off switch.

### Reaching it

- **The Collector is on the workspace container's loopback.** It shares that container's network
  namespace, so `http://localhost:4318` inside the container is the same endpoint as on the host —
  which is what the refusal below requires. Its ports are published by the `workspace` service for
  the same reason; `otel-collector` is not a resolvable name from the other containers.
- **Grafana and ClickHouse are ordinary neighbours**, reached by service name: `grafana:3000`,
  `clickhouse:9000`. From the host's browser Grafana stays on <http://localhost:3000>.

### Driving Compose by hand

A host-side action, always (ADR-0002 — the container has no Docker daemon). From the repository root
on the host:

```sh
docker compose -f .devcontainer/docker-compose.yml restart otel-collector   # one service
docker compose -f .devcontainer/docker-compose.yml down                     # all four, data kept
```

`down -v` throws the ClickHouse and Grafana volumes away with it. Note that `up` here starts the
`workspace` container too — the stack has no separate compose file to bring up on its own any more,
and the Collector could not run without that container in any case, since it lives in its network
namespace.

## Pointing this Server's Agents at it

The destination is not a Client setting — the Server names it, and the Client reports to what it is
offered. Add to `server.toml`:

```toml
[telemetry_offer]
metrics_endpoint = "http://localhost:4318/v1/metrics"
traces_endpoint  = "http://localhost:4318/v1/traces"
logs_endpoint    = "http://localhost:4318/v1/logs"
```

Two things this stack is shaped around:

- **Full URLs with path.** The Server appends no `/v1/metrics` for you, and the Collector's OTLP/HTTP
  receiver routes on that path. An endpoint without it disappears into a 404.
- **`http://` only inside the private address space.** The Client refuses a cleartext destination
  outside loopback and the private ranges — `10/8`, `172.16/12`, `192.168/16`, `fc00::/7` — by
  design ([ADR-0088](../docs/adr/0088-cleartext-own-telemetry-reaches-the-private-address-space.md)).
  That is why every port here is published to the host: an Agent on the same machine reaches
  `http://localhost:4318` and is satisfied. An Agent elsewhere on the LAN reaches the Collector's
  host by address — `http://192.168.10.5:4318/v1/metrics` — and is satisfied too. What is refused,
  and reported rather than warned about, is a public address and a **host name**: the judgement is
  made on the address, since a name can be re-pointed after the offer was admitted. For anything
  beyond the LAN, terminate TLS in front of the Collector and offer the `https://` URL.

Agents receive the offer only if they declare the matching capability (`ReportsOwnMetrics`,
`ReportsOwnTraces`, `ReportsOwnLogs`), and each signal is independent — offer one, two, or all three.

**The Collector is not optional any more.** ClickHouse speaks no OTLP, so unlike Tempo, Loki and
Prometheus there is no backend to point an Agent at directly when the Collector is the broken thing.
If it is down, nothing is stored.

## The schema

The Collector's ClickHouse exporter owns it — `create_schema: true` runs `CREATE TABLE IF NOT
EXISTS` at startup, so no `.sql` file here has to be kept in step with the exporter's column list.
Three tables matter:

| Table | Holds | Time column |
| ----- | ----- | ----------- |
| `otel.otel_metrics_gauge` | Every process metric the Client samples — all three are gauges | `TimeUnix` |
| `otel.otel_logs` | The Client's bridged `tracing` output | `Timestamp` |
| `otel.otel_traces` | Root and child spans, `Duration` in nanoseconds | `Timestamp` |

**Where the Agent's identity lives, and why it matters.** The OTLP Resource belongs to the
*Client* — `ResourceAttributes['service.instance.id']` is the Client that sent the sample. The Agent
each sample is *about* is on the data point: `Attributes['service.instance.id']`. That is what
separates a Managed Process from the Client that sampled it, and every "which Agent" panel reads the
latter. Metric names are stored unmodified — `process.memory.usage`, `process.cpu.utilization`,
`process.uptime` — with no Prometheus-style normalisation to unpick.

## What is already there

Three dashboards are provisioned from `grafana/dashboards/` into the **OpAMP** folder:

| Dashboard | Shows |
| --------- | ----- |
| **Fleet Agents — Overview** | All three signals on one page: how many agents report, their memory / CPU / uptime, log volume by level with a live tail, and operation rate with the recent ones. Filterable by client type and agent. |
| **Fleet Agents — Logs** | The log stream on its own — volume by level and by client, with service / level / substring filters. |
| **Fleet Agents — Traces** | Operation rate, error rate, p50/p95/p99 duration, which phase fails most, and a trace detail view. |

**The traces dashboard stays empty for this Client's own telemetry.** The exporter and tracer
provider are built and registered ([`telemetry.rs`](../crates/client/src/telemetry.rs)), but no
code creates a span yet — ADR-0036 specifies one span per control-loop operation (a configuration
applied, a package installed, a self-update) and that instrumentation is not written. The dashboard
is wired for when it is, and `send-test-telemetry.py` below sends traces in exactly that shape so it
can be seen working today.

Drop any further dashboard JSON into `grafana/dashboards/` — it is picked up within 30 s, no restart.

## Seeding test data

`send-test-telemetry.py` fills the dashboards without a Client, which is also how you tell a broken
pipeline apart from an idle one:

```sh
python3 .devcontainer/send-test-telemetry.py            # defaults to http://localhost:4318
```

It backfills half an hour: three agents' process metrics (one of them restarting mid-window), 120
log records across the severities, and 24 traces in the shape ADR-0036 specifies, a fifth of the log
records carrying the trace id of one of them. Standard library only, no dependencies.
`--dump-dir` writes the request bodies instead of sending them, which is the fastest way to see
exactly what the Client's own exporters would put on the wire.

Re-running describes the same three Agents rather than inventing new ones — the identities and the
RNG seed are fixed. It does insert the rows a second time, though: ClickHouse deduplicates nothing,
so gauges and quantiles read the same but anything counted doubles. To start clean:

```sh
curl -s 'http://localhost:8123/?user=otel&password=otel' --data-binary \
  "TRUNCATE TABLE otel.otel_logs; TRUNCATE TABLE otel.otel_traces; TRUNCATE TABLE otel.otel_metrics_gauge"
```

## Checking that data arrives

```sh
# Does the Collector accept OTLP at all?
curl -i -X POST http://localhost:4318/v1/traces \
     -H 'Content-Type: application/json' --data '{"resourceSpans":[]}'

# What is the Collector itself doing?
docker compose -f .devcontainer/docker-compose.yml logs -f otel-collector

# What made it into ClickHouse? One query, all three signals — which is the point of one store.
curl -s 'http://localhost:8123/?user=otel&password=otel' --data-binary "
  SELECT 'metrics' AS signal, count() AS rows, max(TimeUnix)  AS newest FROM otel.otel_metrics_gauge
  UNION ALL
  SELECT 'logs',              count(),        max(Timestamp)         FROM otel.otel_logs
  UNION ALL
  SELECT 'traces',            count(),        max(Timestamp)         FROM otel.otel_traces"
```

If a dashboard stays empty but the counts above are non-zero, the fault is in the panel, not the
pipeline. If the counts are zero, the Collector is the place to look — uncomment `debug` in the
relevant pipeline in [otel-collector.yaml](otel-collector.yaml) and restart that one service, and it
prints what it receives.

A join that the three-store stack could not do at all — every log line written during a failed
operation:

```sql
SELECT t.SpanName, t.StatusMessage, l.Timestamp, l.SeverityText, l.Body
FROM otel.otel_traces AS t
INNER JOIN otel.otel_logs AS l ON l.TraceId = t.TraceId
WHERE t.ParentSpanId = '' AND t.StatusCode = 'Error'
ORDER BY l.Timestamp
```

## Ports

| Port | Service | What for |
| ---- | ------- | -------- |
| 4318 | Collector | OTLP/HTTP — what the Client exports to |
| 4317 | Collector | OTLP/gRPC — for anything else you want to point here |
| 8888 | Collector | The Collector's own metrics |
| 3000 | Grafana | The UI |
| 8123 | ClickHouse | HTTP interface — `curl`-able, and what the checks above use |
| 9000 | ClickHouse | Native protocol — what the Collector and Grafana speak |
