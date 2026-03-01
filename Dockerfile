# syntax=docker/dockerfile:1
#
# Multi-stage build using only official Rust / Debian images.
# (No third-party cargo-chef dependency)
#
# Dependency caching strategy:
#   1. Copy only Cargo manifests + create empty stub sources
#   2. Build → external deps compile & cache; our stub code fails (ignored)
#   3. Remove stub artifacts; copy real source
#   4. Final build — deps already compiled, only our code is compiled (~30 sec)
#
# Usage:
#   docker compose build
#   docker compose run --rm test-ldpreload   # LD_PRELOAD backend
#   docker compose run --rm test-proxy       # MITM proxy backend

# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder
WORKDIR /build

# ── 1a. Copy only manifest files to seed the dependency cache ─────────────────
COPY Cargo.toml Cargo.lock ./
COPY crates/phantom-core/Cargo.toml    crates/phantom-core/
COPY crates/phantom-storage/Cargo.toml crates/phantom-storage/
COPY crates/phantom-capture/Cargo.toml crates/phantom-capture/
COPY crates/phantom-tui/Cargo.toml     crates/phantom-tui/
COPY crates/phantom-agent/Cargo.toml   crates/phantom-agent/

# ── 1b. Create minimal stub sources ───────────────────────────────────────────
# These let Cargo resolve and compile all external deps.
# Our workspace crates will fail to compile (stubs are empty), but that's fine.
RUN mkdir -p src \
        crates/phantom-core/src \
        crates/phantom-storage/src \
        crates/phantom-capture/src \
        crates/phantom-tui/src \
        crates/phantom-agent/src \
 && printf 'fn main() {}' > src/main.rs \
 && touch \
        crates/phantom-core/src/lib.rs \
        crates/phantom-storage/src/lib.rs \
        crates/phantom-capture/src/lib.rs \
        crates/phantom-tui/src/lib.rs \
        crates/phantom-agent/src/lib.rs

# ── 1c. Compile external dependencies (cached layer) ──────────────────────────
# Exit code is ignored: external deps compile & cache; our stubs fail, that's OK.
RUN cargo build 2>&1; exit 0

# ── 1d. Remove stub artifacts so the real build picks up our code ─────────────
RUN find target/debug -maxdepth 1 \
        \( -name "phantom" -o -name "phantom_*" -o -name "libphantom_*" \) \
        -delete 2>/dev/null || true \
 && find target/debug/.fingerprint -maxdepth 1 -name "phantom*" \
        -exec rm -rf {} + 2>/dev/null || true

# ── 1e. Copy real source & final build (fast: deps already compiled) ──────────
COPY . .
RUN cargo build -p phantom -p phantom-agent

# ── Stage 2: Minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# curl is used by the default test command inside the container.
# jq, ncat, openssl are used by integration tests.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        jq \
        ncat \
        openssl \
        python3 \
 && rm -rf /var/lib/apt/lists/*

# phantom binary
COPY --from=builder /build/target/debug/phantom              /usr/local/bin/phantom
# LD_PRELOAD dylib (Linux-only; compiled as part of the workspace)
COPY --from=builder /build/target/debug/libphantom_agent.so  /usr/local/lib/libphantom_agent.so

ENV PHANTOM_DATA_DIR=/data
RUN mkdir -p /data

# Integration test scripts
COPY tests/integration/ /tests/integration/

ENTRYPOINT ["/usr/local/bin/phantom"]
CMD ["--help"]
