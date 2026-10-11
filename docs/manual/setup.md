# Setting up a fleet

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md)

This page takes you from nothing to a running fleet, in order. You make the certificates,
configure and start the Server, configure and install a Client, and enrol it. The
[Server](server.md) and [Client](client.md) pages describe every key; this page says which ones a
real deployment needs, and why.

The quick start in the [manual's index](README.md#quick-start-a-closed-loop-on-one-machine) runs
both ends on one machine with throwaway certificates. Use this page for anything that crosses a
network.

- [The certificates, at a glance](#the-certificates-at-a-glance)
- [1. Make the certificates](#1-make-the-certificates)
- [2. Configure and start the Server](#2-configure-and-start-the-server)
- [3. Make the package signing key](#3-make-the-package-signing-key)
- [4. Configure and install a Client](#4-configure-and-install-a-client)
- [5. Enrol the Client](#5-enrol-the-client)
- [6. Living with the certificates](#6-living-with-the-certificates)
- [When a connection is refused](#when-a-connection-is-refused)

## The certificates, at a glance

Every connection is TLS 1.3, and a Client proves it belongs to the fleet with a client
certificate in the handshake and nothing else. That takes three certificate authorities, each
with one job:

| CA | Signs | Its key lives | Its certificate is named in |
|---|---|---|---|
| **Server CA** | the Server's own certificate | offline, with you | the Client's `[tls] ca_file`, and your browser's trust store |
| **Client CA** | every host's certificate, by the Server, automatically | on the Server | the Server's `[tls] client_ca_file` and `[client_ca]` |
| **Bootstrap CA** | one bootstrap certificate, which a new host enrols with | offline, with you | the Server's `[enrolment] bootstrap_ca_file` |

How they work together:

1. A new host gets the **bootstrap certificate**. It is the same file on every host. On its own it
   opens nothing: the Server lets it ask for a certificate only while an operator holds the
   enrolment window open, and it answers only once the operator approves the request.
2. The host generates its own key, which never leaves it, and sends a certificate request. You
   approve it by comparing the key's fingerprint, which the host logs, with the one the Server
   lists.
3. The Server signs the request with the **client CA**. From then on the host connects with that
   certificate, and renews it automatically two thirds of the way through its life. With the
   default of 30 days, that is every 20 days.

The **server CA** is independent of the other two. If the Server's address has a certificate from a
public CA, you can use that one instead and skip the server CA. The Client then trusts the
built-in roots, and its `ca_file` stays unset.

Keep the three apart. The client CA and the bootstrap CA must have **different subject names**:
the Server refuses to start when the bootstrap CA shares one with `client_ca_file`. Do not let the
server CA sign host certificates either. Whoever holds a CA's key can mint members of the fleet, so
the bootstrap CA's key and the server CA's key stay off the Server.

## 1. Make the certificates

Nothing in the fleet creates these for you. The commands below use the `openssl` command-line tool
and P-256 keys. Run them on an operator's machine, in a directory only you can read. Replace
`fleet.example.com` with the name your Clients will connect to.

```console
$ mkdir -m 700 fleet-pki && cd fleet-pki
```

Two helpers keep the commands short. `new_ca` makes a CA, valid for ten years. `issue` makes a
leaf certificate signed by one of them:

```bash
new_ca() {   # new_ca <name> <subject CN>
  openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1-key.pem"
  openssl req -x509 -new -key "$1-key.pem" -subj "/CN=$2" -days 3650 -sha256 \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" -out "$1.pem"
}
issue() {    # issue <name> <subject CN> <signing CA> <serverAuth|clientAuth> <days> [subjectAltName]
  openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1-key.pem"
  openssl req -new -key "$1-key.pem" -subj "/CN=$2" -out "$1.csr"
  printf '%s\n' "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" \
    "extendedKeyUsage=$4" ${6:+"subjectAltName=$6"} > "$1.ext"
  openssl x509 -req -in "$1.csr" -CA "$3.pem" -CAkey "$3-key.pem" -CAcreateserial \
    -days "$5" -sha256 -extfile "$1.ext" -out "$1.pem"
  rm -f "$1.csr" "$1.ext"
}
```

Then make the set:

```bash
new_ca server-ca    "Example fleet server CA"
new_ca client-ca    "Example fleet client CA"
new_ca bootstrap-ca "Example fleet bootstrap CA"

issue server    "fleet.example.com"        server-ca    serverAuth 397 "DNS:fleet.example.com,IP:127.0.0.1"
issue bootstrap "Example fleet bootstrap"  bootstrap-ca clientAuth 30
chmod 600 ./*-key.pem
rm -f ./*.srl
```

That leaves these files, and where each one goes:

| File | Goes to | Secret? |
|---|---|---|
| `server-ca.pem` | every Client (`[tls] ca_file`), and your browser | no |
| `server-ca-key.pem` | nowhere: keep it offline, to sign the next server certificate | **yes** |
| `server.pem`, `server-key.pem` | the Server (`[tls] cert_file`, `key_file`) | the key |
| `client-ca.pem`, `client-ca-key.pem` | the Server (`[tls] client_ca_file`, `[client_ca]`) | the key |
| `bootstrap-ca.pem` | the Server (`[enrolment] bootstrap_ca_file`) | no |
| `bootstrap-ca-key.pem` | nowhere: keep it offline, to sign the next bootstrap certificate | **yes** |
| `bootstrap.pem`, `bootstrap-key.pem` | every new Client (`[tls] cert_file`, `key_file`) | the key, but it opens nothing alone (see below) |

**The server certificate must name the address the Clients use.** Clients check the name in
`endpoint` against the certificate's subject alternative names. For a Server reached by IP
address, add it as well, for example `"DNS:fleet.example.com,IP:10.0.0.5,IP:127.0.0.1"`. The
`IP:127.0.0.1` entry is for you: it lets your browser and `curl` reach the Operator plane through
an SSH tunnel (step 2). 397 days keeps the certificate within every browser's limit.

**The bootstrap certificate is short-lived on purpose.** Anyone who copies it can ask for a
certificate. What stops them is that a request also needs an open window and your approval. Make a
fresh one with `issue bootstrap …` whenever you set up hosts after it has expired.

**Formats the Server accepts.** The listener's key may be PKCS#8, SEC1 or PKCS#1, and `cert_file`
may hold the certificate followed by its intermediates. The **client CA's key must be PKCS#8**,
which is what `openssl genpkey` writes. An older key in another format converts with
`openssl pkcs8 -topk8 -nocrypt -in old-key.pem -out client-ca-key.pem`. The keys may be ECDSA
P-256 or P-384, Ed25519, or RSA, with one exception: the server certificate also serves the UI to
your browser, and browsers do not accept Ed25519 there, so give the server certificate and the
server CA an ECDSA or RSA key.

## 2. Configure and start the Server

A release ships the Client, not the Server. Build the Server from a checkout, on a Linux machine,
copy the binary to the Server host, and give it an account of its own:

```console
$ cargo build --release -p fleet-server     # writes target/release/server
$ sudo install -m 755 server /usr/local/bin/server    # on the Server host, after copying it there
$ sudo useradd --system --no-create-home --shell /usr/sbin/nologin opamp-fleet-server
```

Copy the Server's five files to the Server host, for example to `/etc/opamp-fleet-server/tls/`,
readable only by the account the Server runs as. Then write `/etc/opamp-fleet-server/server.toml`.
Give every path absolutely:

```toml
# The Agent plane: where the fleet connects. 4320 is the protocol's default port.
listen = "0.0.0.0:4320"

config_dir = "/var/lib/opamp-fleet-server/configs"
packages_dir = "/var/lib/opamp-fleet-server/packages"

[tls]
cert_file = "/etc/opamp-fleet-server/tls/server.pem"
key_file = "/etc/opamp-fleet-server/tls/server-key.pem"
client_ca_file = "/etc/opamp-fleet-server/tls/client-ca.pem"   # the CA a host's certificate must come from

[client_ca]                                                       # the Server signs host certificates
cert_file = "/etc/opamp-fleet-server/tls/client-ca.pem"           # the same CA as client_ca_file
key_file = "/etc/opamp-fleet-server/tls/client-ca-key.pem"
validity_days = 30

[enrolment]                                                       # new hosts enrol with the bootstrap certificate
bootstrap_ca_file = "/etc/opamp-fleet-server/tls/bootstrap-ca.pem"

# The Operator plane: REST API, API docs and UI. On the loopback it needs no password.
[rest]
listen = "127.0.0.1:4321"
```

Point `client_ca_file` and `[client_ca] cert_file` at the **same** certificate. The Server does
not check that they match. If they differ, it signs certificates that its own handshake refuses.

**The Operator plane stays on the loopback** unless you decide otherwise. Reach it from your machine
through an SSH tunnel:

```console
$ ssh -L 4321:127.0.0.1:4321 fleet.example.com
```

To publish it instead, set `[rest] listen = "0.0.0.0:4321"` and add a password; the Server refuses
to start with a published plane and no password. Make the password's hash with the Server itself,
which reads the password without echoing it:

```console
$ server hash-credential --basic
secret:
$argon2id$v=19$m=19456,t=2,p=1$…
```

```toml
[rest.auth.basic_users]
fleet-admin = "$argon2id$v=19$m=19456,t=2,p=1$…"
```

Then add `-u fleet-admin` to every `curl` below; `curl` asks for the password.

**Start it:**

```console
$ server --config /etc/opamp-fleet-server/server.toml
```

The Server runs in the foreground, logs to stderr, and stops on `SIGTERM`. It does not install
itself as a service, so let your service manager run it. A systemd unit for an account named
`opamp-fleet-server` might look like this:

```ini
# /etc/systemd/system/opamp-fleet-server.service
[Unit]
Description=OpAMP Fleet Server
After=network-online.target
Wants=network-online.target

[Service]
User=opamp-fleet-server
ExecStart=/usr/local/bin/server --config /etc/opamp-fleet-server/server.toml
StateDirectory=opamp-fleet-server
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

The Server refuses an unsafe or incomplete configuration at startup and names the key, for
example `[enrolment] needs [client_ca]: an approved request is signed by it`. Fix what it names and
start it again.

**Check it from your machine**, through the tunnel:

```console
$ curl --cacert server-ca.pem https://127.0.0.1:4321/api/v1/agents
[]
```

The UI is at <https://127.0.0.1:4321/>. Import `server-ca.pem` into your browser's trust store
first.

## 3. Make the package signing key

A Client installs software only when an operator's key has signed it, and that includes its own
updates. Make that key once, with the `opamp-package-sign` tool. Like the Server, it is built from a
checkout ([Command-line tools](tools.md)):

```console
$ opamp-package-sign keygen --out fleet-signing.pk8
3b6a27bc…        # the public key: every Client's [packages] verification_key
```

Keep `fleet-signing.pk8` with you, not on the Server. A Client without the public key runs and
reports normally, but takes no package. You can add the key later.

## 4. Configure and install a Client

Each managed host needs three files from step 1: `server-ca.pem`, `bootstrap.pem` and
`bootstrap-key.pem`. Copy them to the host, for example to `/etc/opamp/`, with the key readable by
root only.

**Install the Client** from the release's `.deb`, `.rpm` or `.msi`. The package registers the
service and leaves it stopped (see [Installing from a native package](client.md#installing-from-a-native-package)).
Then let it write its configuration by asking you:

```console
$ sudo opamp-fleet service install --interactive
Server OpAMP endpoint [wss://127.0.0.1:4320/v1/opamp]: wss://fleet.example.com:4320/v1/opamp
This Agent's name (service.instance.name) [Supervisor Agent]: host-01
Does the Server present a certificate from a private CA? [y/N]: y
PEM CA bundle path: /etc/opamp/server-ca.pem
Client certificate — a bootstrap certificate to enrol with, or an issued one (PEM path): /etc/opamp/bootstrap.pem
Its private key (PEM path): /etc/opamp/bootstrap-key.pem
Package verification key (hex Ed25519 public key; empty takes no packages): 3b6a27bc…
Allow the Server to update this Client's own binary? [Y/n]: y
Name of the package that carries this Client [supervisor]:
wrote /var/lib/opamp-fleet/supervisor.toml
```

Or write the file yourself, which suits configuration management. This is what matters of what
the questions above write, at `/var/lib/opamp-fleet/supervisor.toml` on Linux:

```toml
endpoint = "wss://fleet.example.com:4320/v1/opamp"   # wss:// pushes changes; https:// polls
name = "host-01"                                      # your name for this host, unique in the fleet

[tls]
ca_file = "/etc/opamp/server-ca.pem"     # trust the server CA, and only it
cert_file = "/etc/opamp/bootstrap.pem"   # the bootstrap certificate, to enrol with
key_file = "/etc/opamp/bootstrap-key.pem"

[packages]
verification_key = "3b6a27bc…"
```

A few things worth knowing about these keys:

- **`ca_file` replaces the built-in trust.** With it set, the Client trusts the server CA and no
  other CA. Leave it out only when the Server's certificate comes from a public CA.
- **`name` becomes the certificate's subject.** The certificate the Server issues carries it as its
  common name, and the fleet view shows it.
- **The bootstrap pair stays in the file after enrolment.** Once the host holds an issued
  certificate, it stores the certificate in its state directory as `client-cert.pem` and
  `client-key.pem` and uses that pair instead. The bootstrap pair is what it falls back to if you
  delete them.
- **A host that should run agents** gets `[[supervisor]]` blocks: see
  [Supervisors](client.md#supervisors-putting-a-process-under-management). Enrol it first, then add
  them.

Do not start the service yet: the enrolment window is still closed.

## 5. Enrol the Client

**Open the window**, for as long as you need to set up hosts, up to 24 hours:

```console
$ curl --cacert server-ca.pem -X POST -H 'Content-Type: application/json' \
       -d '{"open_for_secs": 3600}' https://127.0.0.1:4321/api/v1/enrolment/window
{"open":true,"until_ms":…}
```

**Start the Client** and read the key fingerprint it logs:

```console
$ sudo systemctl start opamp-fleet
$ journalctl -u opamp-fleet | grep key_fingerprint
INFO certificate request generated; an enrolling host waits for an operator's approval key_fingerprint=3f9a…
```

**Approve it.** Open the UI's **Enrolments** tab: it lists each waiting host with its full key
fingerprint, its peer address and the name it asked for. Compare the fingerprint with the one the
host logged, then press **Approve**. The same over the API:

```console
$ curl --cacert server-ca.pem https://127.0.0.1:4321/api/v1/enrolments
[{"id":"3f9a…","arrived_ms":…,"peer":"10.0.4.17","subject":"CN=host-01","key_fingerprint":"3f9a…",…}]
$ curl --cacert server-ca.pem -X POST https://127.0.0.1:4321/api/v1/enrolments/3f9a…/approve
```

Approve only a fingerprint you can match to a host you are setting up. A request you do not
recognise is rejected with `…/reject`.

The host receives its certificate within seconds, checks it by connecting with it, stores it, and
appears on the **Agents** tab as connected. Its certificate is listed under
`GET /api/v1/certificates`.

**Close the window** when you are done. It also closes on its own when its time runs out, and on a
Server restart:

```console
$ curl --cacert server-ca.pem -X DELETE https://127.0.0.1:4321/api/v1/enrolment/window
```

**A host behind a Gateway cannot enrol through it**, because a Gateway trusts only the client CA,
never the bootstrap CA. Let the host reach the Server directly once to enrol, then point it at the
Gateway. Gateways are described in [Gateway Mode](client.md#gateway-mode-carrying-other-clients).

## 6. Living with the certificates

**Host certificates renew themselves.** At two thirds of its life the Client generates a new key,
asks for a certificate with its current one, and switches once it has connected with the new one.
No operator is involved, and no window needs to be open.

**A host that stays offline longer than its certificate's validity is locked out.** Its stored
certificate has expired, and the Client keeps presenting it. To enrol it again:

1. On the host, stop the service and delete `client-cert.pem` and `client-key.pem` from its state
   directory (`/var/lib/opamp-fleet/state/` on Linux), and `client-key.pending.pem` and
   `client-csr.pending.pem` if they are there. The Client then falls back to the bootstrap pair in
   `supervisor.toml`, and logs the fingerprint of a fresh key once it reaches the Server with the
   window open.
2. If that bootstrap certificate has expired too, issue a fresh one (step 1) and replace it.
3. Open the window, start the service, and approve the fingerprint, as in step 5.

**To shut a host out**, revoke its certificate by serial. The Server closes its sessions at once,
refuses it from then on, and no renewal of that certificate is accepted either. A host that has
renewed has several certificates listed; revoke the oldest, the one it enrolled with, and the
revocation reaches every renewal of it:

```console
$ curl --cacert server-ca.pem https://127.0.0.1:4321/api/v1/certificates
[{"authority":"client","serial":"5c0f…","subject":"CN=host-01",…}]
$ curl --cacert server-ca.pem -X POST -H 'Content-Type: application/json' \
       -d '{"certificate": {"authority": "client", "serial": "5c0f…"}}' \
       https://127.0.0.1:4321/api/v1/revocations
```

A bootstrap certificate is revoked the same way, with `"authority": "bootstrap"`; read its serial
with `openssl x509 -noout -serial -in bootstrap.pem | cut -d= -f2`. See
[Revocation](server.md#revocation-withdrawing-a-certificate) for Gateways and for lifting a
revocation.

**The server certificate is yours to renew.** Before it expires, issue a new one with the same
names (`issue server …`), replace `server.pem` and `server-key.pem` on the Server, and restart it.
The Clients reconnect on their own. They trust the server CA, not one particular certificate, so
nothing changes on the hosts.

**The CAs outlast everything else**, so give them a long life. Replacing the server CA means
giving every host the new `ca_file`. Replacing the client CA means every host enrols again: the
Server renews only a certificate its current client CA issued.

## When a connection is refused

| What you see | Where | What it means |
|---|---|---|
| `cannot connect … invalid peer certificate: UnknownIssuer` | Client | `ca_file` does not hold the CA that signed the server certificate, or `ca_file` is unset and the certificate comes from a private CA. |
| `cannot connect … invalid peer certificate: certificate not valid for name …` | Client | The host name or address in `endpoint` is not among the server certificate's alternative names. |
| The Client refuses to start: `[tls] cert_file and key_file are required` | Client | The host has no client identity yet. Name the bootstrap pair in `[tls]`. |
| The handshake fails, and the Server never lists the host | both | The Client's certificate comes from none of the Server's CAs, or `[enrolment]` is not configured and the host holds only a bootstrap certificate. |
| `cannot connect … 503 Service Unavailable`, and no request is listed | Client | The enrolment window is closed. Open it; the Client retries by itself. |
| The request is listed, but the host never connects as a member | Server | Nobody approved it yet. The host waits and sends the same request again until you do. |
| `admission failed: the server refused this client (HTTP 401)` | Client | The Server refused the certificate after the handshake: it is revoked. |
| `cannot connect … received fatal alert …` on a host that was connected before | Client | Its certificate expired while it was offline. Enrol it again, as [Living with the certificates](#6-living-with-the-certificates) describes. |
| A `429` | Client | The host's address failed admission too often, and the Server is backing it off. Fix the certificate; the back-off ends by itself. |
| The Server refuses to start, naming `shares a CA with [tls] client_ca_file` | Server | The bootstrap CA and the client CA have the same subject name. Make a bootstrap CA with a name of its own. |
| The Server refuses to start, naming `[client_ca] … does not exist` | Server | A file of `[client_ca]` is missing. |
| The Server refuses to start with `cannot read <key file>: …` | Server | The account the Server runs as cannot read the client CA's key, or, when the message ends in `Could not parse key pair`, the key is not PKCS#8. Convert it as [step 1](#1-make-the-certificates) shows. |
| Hosts enrol, but their certificates are refused at once | both | `[client_ca]` holds a key that does not belong to its certificate, or `client_ca_file` names another CA. The Server checks neither at startup. |
