.DEFAULT_GOAL := help
.PHONY: help build test-ldpreload test-proxy shell clean

# ── Help ──────────────────────────────────────────────────────────────────────

help:
	@echo "phantom — Docker test targets"
	@echo ""
	@echo "  make build           Build (or rebuild) the Docker image"
	@echo "  make test-ldpreload  Test LD_PRELOAD backend (Linux agent, Mac-friendly via Docker)"
	@echo "  make test-proxy      Test MITM proxy backend (port 8080)"
	@echo "  make shell           Drop into a bash shell inside the image"
	@echo "  make clean           Remove the built Docker image"
	@echo ""
	@echo "Native (macOS proxy only):"
	@echo "  cargo run"
	@echo "  curl -x http://127.0.0.1:8080 http://httpbin.org/get"

# ── Docker ────────────────────────────────────────────────────────────────────

## Build the Docker image.
## First run: ~3-5 min (downloads deps). Subsequent runs: ~30 sec (cached).
build:
	docker compose build

## Test LD_PRELOAD backend inside a Linux container.
##
## phantom starts, spawns `curl http://httpbin.org/get` with the agent injected,
## and the captured HTTP trace appears in the TUI. Press 'q' to quit.
test-ldpreload: build
	docker compose run --rm test-ldpreload

## Test MITM proxy backend (port 8080 forwarded to host).
##
## While this is running, from another terminal:
##   curl -x http://127.0.0.1:8080 http://httpbin.org/get
test-proxy: build
	docker compose run --rm -p 8080:8080 test-proxy

## Open a bash shell inside the runtime image for manual exploration.
##
## Example inside shell:
##   PHANTOM_SOCKET=/tmp/t.sock \
##   LD_PRELOAD=/usr/local/lib/libphantom_agent.so \
##   curl http://httpbin.org/get
shell: build
	docker compose run --rm shell

## Remove the built Docker image and stop any running containers.
clean:
	docker compose down --rmi local --volumes --remove-orphans
