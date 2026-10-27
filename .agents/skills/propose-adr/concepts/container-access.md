# Container access

How the work inside the Dev Container reaches containers it needs, such as a database for an
integration test or Testcontainers, while the host's Docker socket stays out of reach as
[ADR-0004](../../../../docs/adr/0004-dev-container-runtime.md) decides. The template ships none:
the container has no engine and no socket.

## Signals

- The specification or the toolchain names a database, a broker, or another service the tests
  run against.
- The test framework starts containers itself (Testcontainers and its ports).
- Someone reaches for the `docker-outside-of-docker` Feature, which mounts the host socket.

## Fits when

- **No container access**, the template's state: unit tests and fakes cover the core, and
  integration tests run in CI only.
- **Compose with service containers**: the services are fixed and known (a database, a cache);
  `devcontainer.json` names a Compose file, the services run beside the workspace container, and
  no engine is reachable from inside.
- **An engine inside** (the Docker-in-Docker Feature, or Podman): the tests start containers
  themselves, and an engine runs inside the Dev Container, separate from the host's. ADR-0004
  leaves this to the project, and its check still refuses the host socket. The Docker-in-Docker
  Feature runs the container privileged; rootless Docker in Docker needs that too. Whether Podman
  runs nested without it depends on the host's runtime: try it before the ADR claims it.

## Does not fit

- `docker-outside-of-docker` or any socket mount from the host: it hands host-level control to
  everything in the container, the agent included; ADR-0004 forbids it.
- An engine inside where the container must not run privileged: the Docker-in-Docker Feature
  sets `privileged`.
- Compose for services the tests create and throw away per test: that is what an engine inside
  is for.

## The rule a test decides

No mount of the host's Docker socket: the Dev Container check of ADR-0004 decides it, whichever
option is chosen. For Compose, a test that the services the toolchain needs are named in the
Compose file; for an engine inside, a smoke test in CI that starts one container.

## Sensors

`scripts/check-devcontainer.sh`, which ships with the template.

## Alternatives

The three options above are each other's alternatives; the socket mount under *Does not fit* is
the one ADR-0004 rules out. A remote engine on another machine is a fourth, for teams that
already run one; it moves the trust boundary rather than removing it.

## Sources

- Dev Containers, *Docker-in-Docker Feature* —
  <https://github.com/devcontainers/features/tree/main/src/docker-in-docker>: an engine inside the
  container; its definition sets `privileged`.
- Dev Containers, *Docker-outside-of-Docker Feature* —
  <https://github.com/devcontainers/features/tree/main/src/docker-outside-of-docker>: the Docker
  CLI talking to the host engine through its socket.
- Dev Containers, *Using Images, Dockerfiles, and Docker Compose* —
  <https://containers.dev/guide/dockerfile>: a Compose file as the container's definition.
- Docker, *Docker daemon attack surface* —
  <https://docs.docker.com/engine/security/#docker-daemon-attack-surface>: why control of the
  daemon is control of the host.
- Docker, *Rootless mode* — <https://docs.docker.com/engine/security/rootless/>.
- Testcontainers, *Supported Docker environments* —
  <https://java.testcontainers.org/supported_docker_environment/>: the engines it runs against,
  Podman among them.
