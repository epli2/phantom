# Phantom

[![CI](https://github.com/epli2/phantom/actions/workflows/ci.yml/badge.svg)](https://github.com/epli2/phantom/actions/workflows/ci.yml)

Phantom is a zero-instrumentation HTTP/HTTPS API observability tool, written in Rust. It captures traffic from any process via a MITM proxy (or an `LD_PRELOAD` agent on Linux), and lets you explore it in an interactive terminal UI, stream it as JSON Lines, query it offline, or hand it to an AI coding agent over MCP — without touching the target application's code.

## Features

- **Interactive TUI** — browse captured requests/responses live, filter by URL.
- **JSON Lines streaming** — `phantom run --output jsonl` prints one JSON object per trace to stdout and exits with the traced process's exit code, ideal for scripting and CI.
- **Offline queries** — `phantom list` / `get` / `search` / `stats` filter and inspect previously captured traces without a live capture running.
- **MCP server** — `phantom mcp` exposes capture control and trace queries as tools for AI coding agents (e.g. Claude Code).
- **Zero-instrumentation capture** for common languages:
  - **Node.js** — HTTPS captured transparently via an injected preload script (`http`, `https`, `undici`, `fetch`, `axios`, all supported).
  - **PHP** — libcurl-based HTTP/HTTPS captured via injected `curl.cainfo`, no code changes.
  - **Java** — JVM HTTP clients captured via an injected `-javaagent` and JVM proxy system properties.
  - **Anything else** — `HTTP_PROXY`/`HTTPS_PROXY` are set automatically for the spawned command.
- **`LD_PRELOAD` backend** (Linux only) — hooks libc `send`/`recv` and OpenSSL directly, capturing HTTP + HTTPS for any dynamically linked process with no proxy configuration at all.
- **Docker sidecar mode** (`--bind 0.0.0.0`) — trace a target container you don't spawn, over a shared Docker network.
- **Fault injection** (`--fault`) — inject delays or error responses into proxied traffic for resilience testing.

## Quickstart

Build from source (requires Rust stable; a JDK is optional and only needed for Java capture support):

```sh
cargo build --release
```

Trace a command — HTTP and HTTPS are captured with zero application changes:

```sh
phantom run -- node app.js
phantom run -- java -jar app.jar
phantom run -- php app.php
phantom run -- curl http://api.example.com/v1/users
```

Stream traces as JSON Lines for scripting (exits with the child's exit code):

```sh
phantom run --output jsonl -- node app.js | jq 'select(.status_code >= 400)'
```

Query traces captured in a previous run:

```sh
phantom list --status 5xx --since 10m
phantom get <SPAN_ID>
```

Run as an MCP server for AI coding agents:

```sh
claude mcp add phantom -- phantom mcp
```

## CLI

| Subcommand | Purpose |
|---|---|
| `run` | Capture traffic; optionally spawn and trace a command (`-- <CMD>`) |
| `list` | Query stored traces (newest first) with filters |
| `get <SPAN_ID>` | One trace as pretty JSON |
| `search <PATTERN>` | Shorthand for `list --url <PATTERN>` |
| `stats` | Trace count and data directory as JSON |
| `clear --yes` | Delete all traces |
| `mcp` | MCP server over stdio, for AI coding agents |

Run `phantom <SUBCOMMAND> --help` for the full flag reference, or see [`AGENTS.md`](AGENTS.md) for the complete CLI structure, JSONL schema, and MCP tool list.

## Documentation

- [`docs/how-to-use.ja.md`](docs/how-to-use.ja.md) — detailed Japanese-language usage guide.
- [`AGENTS.md`](AGENTS.md) — architecture, CLI reference, and conventions for AI coding agents working on this repository (also available as `CLAUDE.md` / `GEMINI.md`).
- [`examples/docker-sidecar/`](examples/docker-sidecar/) — running phantom as a Docker Compose sidecar.
- [`plan.md`](plan.md) — technical design document (Japanese).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
