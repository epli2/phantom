# syntax=docker/dockerfile:1
#
# Multi-stage build using cargo-chef for fast incremental rebuilds.
#
#   First build  : ~3-5 min (downloads + compiles all deps)
#   Source-only  : ~30 sec  (deps are cached in layer)
#   Cargo.toml   : deps rebuilt, source recompiled
#
# Usage:
#   docker compose build
#   docker compose run --rm test-ldpreload   # LD_PRELOAD backend (Phase 2)
#   docker compose run --rm test-proxy       # MITM proxy backend (Phase 1)

# ── Stage 1: cargo-chef base (Rust + cargo-chef pre-installed) ────────────────
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /build

# ── Stage 2: Generate dependency recipe ───────────────────────────────────────
FROM chef AS planner
COPY . .
# Produces recipe.json: the minimal set of files needed to compile all deps.
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Build (cached dep layer + our source) ────────────────────────────
FROM chef AS builder

COPY --from=planner /build/recipe.json recipe.json

# Cook = compile only dependencies.
# This layer is cached as long as Cargo.toml / Cargo.lock don't change.
RUN cargo chef cook --recipe-path recipe.json

# Now copy actual source and compile our crates (fast: deps already built).
COPY . .
RUN cargo build -p phantom -p phantom-agent

# ── Stage 4: Minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# curl: used by the default test command inside the container.
# ca-certificates: needed for HTTPS requests by curl.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# phantom binary
COPY --from=builder /build/target/debug/phantom              /usr/local/bin/phantom
# LD_PRELOAD dylib (Linux-only, compiled as part of the workspace)
COPY --from=builder /build/target/debug/libphantom_agent.so  /usr/local/lib/libphantom_agent.so

# Default storage directory
ENV PHANTOM_DATA_DIR=/data
RUN mkdir -p /data

ENTRYPOINT ["/usr/local/bin/phantom"]
# Override CMD via docker compose run / docker run
CMD ["--help"]
