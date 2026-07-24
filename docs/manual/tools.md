Server base URL› http://127.0.0.1:4321
      --out-dir ./artifacts --server http://127.0.0.1:4321
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

