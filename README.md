# Phantom

[![CI](https://github.com/epli2/phantom/actions/workflows/ci.yml/badge.svg)](https://github.com/epli2/phantom/actions/workflows/ci.yml)

**Zero-instrumentation API observability toolbox for modern web development** — observe, perturb, record, and spec your app's HTTP traffic without changing a line of code.

Phantom wraps any command in a local MITM proxy, captures every HTTP/HTTPS request and response the process makes, and shows them in an interactive terminal UI — or streams them as JSON Lines for scripts and AI coding agents.

日本語のガイドは [docs/how-to-use.ja.md](docs/how-to-use.ja.md) を参照してください。

## 30-second demo

```sh
# See every request your app makes — HTTP and HTTPS, zero app changes:
phantom -- node app.js

# Stream traces as JSONL for scripting or AI analysis:
phantom --output jsonl -- npm test | jq 'select(.status_code >= 400)'

# Chaos-test your error handling: 500ms delay on every /api call:
phantom --fault delay:500ms:/api -- node app.js
```

## Features

| | |
|---|---|
| **Zero instrumentation** | No SDK, no code changes. Phantom spawns your command with proxy + CA trust environment pre-configured. |
| **HTTPS interception** | Persistent local CA (`phantom cert`). Spawned processes trust it automatically via `CURL_CA_BUNDLE`, `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, `DENO_CERT` — no `--insecure` flags needed. |
| **HTTP/1.1 + HTTP/2** | Both captured through the MITM proxy. |
| **Node.js deep integration** | `phantom -- node app.js` auto-injects a preload that routes `http`, `https`, `axios`, `undici`, and `fetch()` through the proxy. |
| **Interactive TUI** | Ratatui-based two-pane viewer with filtering. |
| **JSONL output** | One JSON object per line on stdout; exits when the child exits. Built for `jq` pipelines and AI agents. |
| **Fault injection** | `--fault delay:100ms-500ms`, `--fault error:503:0.5:/api` — verify timeout and error handling during development. |
| **Content-Encoding decoding** | gzip/brotli/zstd/deflate response and request bodies are transparently decoded for storage/display; the wire is never altered. |
| **Redaction** | `--redact` masks common secrets (`authorization`, `cookie`, `password`, `token`, …) in headers and JSON bodies before they ever hit disk. Off by default; opt in before sharing traces. |
| **Local persistence** | Traces stored in an embedded [Fjall](https://github.com/fjall-rs/fjall) LSM-tree store. |
| **LD_PRELOAD backend** (Linux) | Socket-level capture for proxy-unaware tools (plain HTTP). |

## Installation

Requires Rust (stable):

```sh
git clone https://github.com/epli2/phantom
cd phantom
cargo build --release
# binary at target/release/phantom
```

Pre-built binaries and Homebrew are planned — see the [roadmap](ROADMAP.md).

## How it works

1. **Proxy backend (default):** Phantom starts a MITM proxy on `127.0.0.1:<port>`, then spawns your command with `HTTP_PROXY`/`HTTPS_PROXY` pointing at it and CA trust variables pointing at a combined bundle (your existing roots + the phantom CA). TLS is re-signed on the fly by a CA persisted under the data directory.
2. **Node.js:** for `node` commands a preload script is additionally injected via `--require`, covering clients that ignore proxy environment variables.
3. **LD_PRELOAD backend (Linux):** `libphantom_agent.so` hooks `send`/`recv` at the libc level for plain-HTTP capture without any proxy configuration.

Captured traces flow into the TUI or JSONL stream and are persisted locally. Everything binds to `127.0.0.1` only; nothing leaves your machine.

### Trusting the CA outside phantom

Processes spawned by phantom trust the CA automatically. For anything else (e.g. a browser):

```sh
phantom cert export          # writes phantom-ca.cert.pem + prints OS trust instructions
phantom cert path            # print the PEM path (for scripts)
```

## JSONL schema

Each line is one completed request/response pair:

```json
{"timestamp_ms":1783340616458,"duration_ms":12,"method":"GET","url":"https://api.example.com/users","status_code":200,"request_headers":{"…":"…"},"response_headers":{"…":"…"},"response_body":"…","protocol_version":"HTTP/1.1","trace_id":"13a7…","span_id":"3699…"}
```

Full field reference and compatibility policy: [docs/jsonl-schema.md](docs/jsonl-schema.md).

## CLI

```text
phantom [OPTIONS] [-- CMD…]        # capture (same as `phantom run`)
phantom run [OPTIONS] [-- CMD…]    # capture, explicit form
phantom cert path|print|export     # manage the HTTPS interception CA

Options (run):
  -b, --backend <proxy|ldpreload>  Capture backend            [default: proxy]
  -o, --output  <tui|jsonl>        Output mode                [default: tui]
  -p, --port    <PORT>             Proxy port                 [default: 8080]
      --insecure                   Skip TLS verification toward backend servers
  -d, --data-dir <DIR>             Storage directory
      --fault <SPEC>               Inject faults (repeatable)
      --agent-lib <PATH>           libphantom_agent.so (ldpreload backend)
```

Run `phantom --help` for the full guide with examples.

## Development

```sh
make check    # fmt + clippy + build + test (same as CI)
make test     # test suite (Node.js integration tests require node/npm)
```

Project conventions and architecture are documented in [AGENTS.md](AGENTS.md).

## Roadmap

Phantom is evolving into a full local-first API development toolbox: HAR export/import, request replay, record-and-mock servers, WebSocket/SSE capture, and OpenAPI generation from live traffic. See [ROADMAP.md](ROADMAP.md) for the detailed plan.

## License

TBD
