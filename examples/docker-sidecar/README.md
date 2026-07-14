# Docker Sidecar Example

> **Not verified end-to-end in the environment this was authored in** (no
> working Docker daemon was available there). Please run through the steps
> below yourself and confirm before relying on this pattern. `docker compose
> config` (no daemon required) was used to confirm `compose.yaml`'s syntax
> and structure resolve correctly.

## The pattern

phantom normally spawns and traces a process itself (`phantom run -- node app.js`).
This example instead runs phantom as a **sidecar container** on the same
Docker network as a target app container that phantom does not spawn or
manage — the target container is entirely yours (its own image, its own
`Dockerfile`/compose entry). You only need to:

1. Point the target container's `HTTP_PROXY`/`HTTPS_PROXY` at phantom's
   Compose service name (Docker's embedded DNS resolves it), e.g.
   `http://phantom:8080`.
2. Trust phantom's MITM CA certificate in the target container for HTTPS
   capture (see below — this step is client/language-specific).

This is the same "manual" proxy-configuration mode phantom already supports
on a single host (`HTTP_PROXY=http://127.0.0.1:8080 your-app`), just crossing
a Docker network boundary instead of staying on loopback.

**Scope**: this covers the HTTP_PROXY-sidecar approach only. Transparent
traffic interception (iptables/eBPF, no target container changes needed) and
tracing across container boundaries via the `ldpreload` backend are both out
of scope here — possible future work, not implemented.

## Running it

```sh
cd examples/docker-sidecar
docker compose up --build
docker compose logs -f phantom | jq .   # view captured traces (JSONL)
```

`compose.yaml` reuses the repository's existing root `Dockerfile` (a debug
build bundled with test tooling, built for phantom's own CI integration
tests) rather than a dedicated slim release image — deliberate, to keep this
example minimal. A smaller/optimized image is possible future work if
needed.

The `app` service here is a disposable `curlimages/curl` container making
periodic requests to `httpbin.org` (needs outbound internet) purely so the
example is runnable standalone. Swap it out for your own service.

## `--bind 0.0.0.0` and security

By default phantom's proxy only binds `127.0.0.1` (unreachable from other
containers). `--bind 0.0.0.0` is required for a sidecar to be reachable from
other containers on the same Docker network. **The proxy has no
authentication** — only bind `0.0.0.0` on a trusted/private network (an
internal Docker network, never anything internet-facing).

## Trusting the CA certificate

phantom writes its MITM CA certificate to `<data_dir>/ca.pem` (here: `/data/ca.pem`
inside the `phantom` container, shared via the `phantom-data` named volume and
mounted read-only into `app` at `/ca/ca.pem`) every time it starts. **There is
no single environment variable that every HTTP client honors** — this is the
same nuance already documented for phantom's Node.js/PHP auto-injection, just
applied manually here since phantom isn't spawning the target process. Some
common cases:

| Client / language | How to trust `ca.pem` |
|---|---|
| curl / libcurl | `CURLOPT_CAINFO` or `curl --cacert /ca/ca.pem` (used in `compose.yaml`'s example) |
| Node.js | `NODE_EXTRA_CA_CERTS=/ca/ca.pem` |
| PHP curl extension | `-d curl.cainfo=/ca/ca.pem` (same mechanism phantom auto-injects when it spawns a PHP child directly) |
| Java / JVM | Import into a truststore (`keytool -importcert -file /ca/ca.pem ...`) or `-Djavax.net.ssl.trustStore=...` |
| Python `requests` | `REQUESTS_CA_BUNDLE=/ca/ca.pem` |
| Debian/Ubuntu-based images (system-wide) | Copy to `/usr/local/share/ca-certificates/`, then run `update-ca-certificates` |

If your target app doesn't support any of these, it will fail HTTPS requests
with a certificate error once traffic is routed through phantom — that's
expected until CA trust is configured; plain HTTP capture works regardless.

## Viewing traces

- **JSONL (used here, recommended for a non-interactive sidecar)**:
  `docker compose logs -f phantom | jq .` streams one JSON trace object per
  line as requests happen.
- **TUI**: requires an interactive terminal, so `docker compose up -d` won't
  work for it. Use `docker compose run --rm phantom run --bind 0.0.0.0 --port 8080`
  instead (drop `--output jsonl` from the command, add `tty: true` /
  `stdin_open: true` to the service definition — mirrors the `test-ldpreload`
  service in the repository root's `compose.yaml`).
