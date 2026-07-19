# The OpAMP Supervisor sidecar: the supervisor plus the collector process it owns (ADR-0003).
#
# Neither upstream image can do this alone. The supervisor image ships *only* the supervisor binary
# (~15 MB) — but `agent.executable` must point at a collector binary that actually exists on the same
# filesystem. The collector image is FROM scratch, so it cannot host the supervisor either. Hence this
# image: take both binaries, put them on a base with a shell so the sidecar stays debuggable.
ARG OTEL_VERSION=0.156.0

FROM otel/opentelemetry-collector-opampsupervisor:${OTEL_VERSION} AS supervisor
FROM otel/opentelemetry-collector-contrib:${OTEL_VERSION} AS collector

FROM alpine:3.24

RUN apk add --no-cache ca-certificates \
    && addgroup -g 10001 -S otel \
    && adduser -u 10001 -S -G otel otel

COPY --from=supervisor /usr/local/bin/opampsupervisor /usr/local/bin/opampsupervisor
COPY --from=collector /otelcol-contrib /usr/local/bin/otelcol-contrib

COPY supervisor.yaml /etc/otel/supervisor.yaml
COPY fallback.yaml /etc/otel/fallback.yaml

# The supervisor writes its state and the collector's effective config here. A fresh named volume
# mounted at this path inherits the ownership set here, which is what lets the unprivileged user write.
RUN mkdir -p /var/lib/otelcol/supervisor && chown -R otel:otel /var/lib/otelcol

USER 10001:10001

ENTRYPOINT ["/usr/local/bin/opampsupervisor"]
CMD ["--config", "/etc/otel/supervisor.yaml"]
