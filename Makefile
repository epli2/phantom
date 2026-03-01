.DEFAULT_GOAL := help
.PHONY: help build test-ldpreload test-proxy test-jsonl shell clean fmt clippy test ci

# ── Help ──────────────────────────────────────────────────────────────────────

help:
	@echo "phantom — Tasks"
	@echo ""
	@echo "Build:"
	@echo "  make build           Build the project"
	@echo "  make clean           Clean the build"
	@echo ""
	@echo "Checks:"
	@echo "  make fmt             Format Rust code"
	@echo "  make fmt-fix         Fix Rust code formatting"
	@echo "  make clippy          Run Clippy lints (warnings as errors)"
	@echo "  make test            Run Rust tests"
	@echo "  make check           Run all CI checks locally (fmt --check, clippy, build, test)"
	@echo ""
	@echo "Docker test targets:"
	@echo "  make docker-build           Build (or rebuild) the Docker image"
	@echo "  make docker-test-ldpreload  Test LD_PRELOAD backend — interactive TUI"
	@echo "  make docker-test-proxy      Test MITM proxy backend (port 8080) — interactive TUI"
	@echo "  make docker-test-jsonl      Test LD_PRELOAD backend — JSONL stdout (non-interactive)"
	@echo "  make docker-test-integration  Run integration test suite (HTTP + HTTPS, local mock servers)"
	@echo "  make docker-shell           Drop into a bash shell inside the image"
	@echo "  make docker-clean           Remove the built Docker image"
	@echo ""
	@echo "Native (macOS proxy only):"
	@echo "  cargo run"
	@echo "  curl -x http://127.0.0.1:8080 http://httpbin.org/get"

# ── Build ───────────────────────────────────────────────────────────

build:
	cargo build --workspace --all-targets --all-features

clean:
	cargo clean

# ── Checks ──────────────────────────────────────────────────────────

## Format Rust code
fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

## Run Clippy lints (fails on warnings)
clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

## Run Clippy for Linux (checks phantom-agent which is Linux-only)
clippy-linux:
	rustup target add x86_64-unknown-linux-gnu
	cargo clippy -p phantom-agent --target x86_64-unknown-linux-gnu -- -D warnings

## Run Rust tests
test:
	cargo test --workspace --all-targets --all-features

## Run all checks
check: fmt clippy clippy-linux build test

# ── Docker ───────────────────────────────────────────────────────────

## Build the Docker image.
## First run: ~3-5 min (downloads deps). Subsequent runs: ~30 sec (cached).
docker-build:
	docker compose build

## Test LD_PRELOAD backend inside a Linux container.
##
## phantom starts, spawns `curl http://httpbin.org/get` with the agent injected,
## and the captured HTTP trace appears in the TUI. Press 'q' to quit.
docker-test-ldpreload: docker-build
	docker compose run --rm test-ldpreload

## Test MITM proxy backend (port 8080 forwarded to host).
##
## While this is running, from another terminal:
##   curl -x http://127.0.0.1:8080 http://httpbin.org/get
docker-test-proxy: docker-build
	docker compose run --rm -p 8080:8080 test-proxy

## Test LD_PRELOAD backend in JSONL output mode (non-interactive).
##
## phantom starts, runs `curl http://httpbin.org/get` with the agent, then
## exits automatically.  Each captured HTTP trace is written to stdout as a
## single JSON object (one per line).
##
##   make test-jsonl          # raw JSON Lines
##   make test-jsonl | jq .   # pretty-print via jq
docker-test-jsonl: docker-build
	docker compose run --rm test-jsonl

## Run the integration test suite (HTTP + HTTPS) inside a Linux container.
##
## All tests use local ncat mock servers — no network access required.
## Exits with 0 on success, 1 on failure.
docker-test-integration: docker-build
	docker compose run --rm test-integration

## Open a bash shell inside the runtime image for manual exploration.
##
## Example inside shell:
##   PHANTOM_SOCKET=/tmp/t.sock \
##   LD_PRELOAD=/usr/local/lib/libphantom_agent.so \
##   curl http://httpbin.org/get
docker-shell: docker-build
	docker compose run --rm shell

## Remove the built Docker image and stop any running containers.
docker-clean:
	docker compose down --rmi local --volumes --remove-orphans
