Build it **on** the distribution you are building for — the tree carries the libraries the build
host resolves, so the build host is the decision, not a flag (see the two rules below). This
project's Dev Container is that host: it is pinned to Debian 12 and carries Icinga's runtime
libraries for exactly this reason
([ADR-0029](../adr/0029-icinga-2.md)), so the
Debian 12 artifact is built in it directly:
$ cargo run --bin opamp-package-fetch -- --agent icinga2 --version 2.16.5 --distro bookworm \
      --platform linux/amd64 --server http://127.0.0.1:4321
  reading https://packages.icinga.com/debian/dists/icinga-bookworm/main/binary-amd64/Packages.gz …
  reading https://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages.gz …

linux/amd64
  downloading …/icinga2-bin_2.16.5-1+debian12_amd64.deb …
  verified against upstream's SHA-256
  …
  bundled 28 shared libraries
  repacked  sha256 d60ee0e6…
Two things about that command line, each of which costs an attempt to discover:
  is and says so; named, it refuses when the host is not that distribution. In a recipe meant to be
  copied, the refusal is the point — the wrong build host then fails loudly instead of quietly
- **`--server` uploads as it goes.** Leave it out with `--no-upload` and upload the artifacts
  afterwards; the tool prints the two `curl` calls that do it.

Add `--platform windows/amd64` to build the Windows artifact in the same run. It is repacked from
the MSI and verified by Icinga's own Authenticode signature
([ADR-0029](../adr/0029-icinga-2.md)) rather than by a
digest, so it needs no particular build host and no glibc floor applies to it.

To build for a **different** reach — an older distribution than the Dev Container, for hosts it does
not cover — run the same tool in a container of that distribution, which is then the build host:

```console
$ docker run --rm -v "$PWD:/src" -w /src --network host rust:bullseye bash -lc \
    'cargo run --bin opamp-package-fetch -- --agent icinga2 --version 2.16.5 --distro bullseye \
       --platform linux/amd64 --no-upload'
```

`cargo run` rather than the binary from this checkout: the tool is glibc-bound like everything it
builds, so one compiled in the Dev Container will not start under an older distribution at all.
Build it where it runs. `--network host` is only needed for `--server`, and only on Linux. That
container also needs Icinga's runtime libraries installed once — see the refusal below.
(`monitoring-plugins`, 47 of them, with the libraries they need), and the vendor copyright files.
For Icinga 2 2.16.5 on Debian 12 that is 140 files and 75 MB unpacked — well inside the limits a
package tree is held to ([ADR-0019](../adr/0019-package-delivery-on-the-agent.md)).

The plugins come from the distribution rather than from Icinga, and one of them needs a word: Debian
ships `check_http` through `update-alternatives`, so it exists only after a package is *installed* —
the repack applies that same rule to the payload, highest priority winning, which is why
`check_http` is in the tree and is the implementation Debian would have chosen.
  build for a distribution this host is not — which is why the second recipe above is a container
  and not a flag.
  decision this step really carries, and for this project it has been made once, as the Dev
  Container's image pin (ADR-0029). Bumping that pin narrows every artifact built afterwards.
packing an incomplete tree — and prints the `apt-get install` line that fixes it, naming the
packages that provide the libraries it just listed. **The Dev Container already carries them**; a
container you started for a different reach needs the line once, inside that same container and
without `sudo`, since a container's shell is already root:
error: the build host is missing libraries the package needs: libboost_coroutine.so.1.74.0, …
  install the vendor package's own dependencies first:
      sudo apt-get install -y --no-install-recommends libboost-coroutine1.74.0 …

$ apt-get update && apt-get install -y --no-install-recommends libboost-coroutine1.74.0 …
The names carry the distribution's own versions, so they differ per container — which is why the
tool reads them out of that distribution's package index rather than this page listing them.

