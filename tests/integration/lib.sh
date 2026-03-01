#!/usr/bin/env bash
# ── Integration test utilities for phantom LD_PRELOAD agent ──────────────────
#
# Provides helper functions for running phantom in JSONL mode against local
# mock HTTP/HTTPS servers (ncat) and asserting trace output with jq.
#
# Source this file from test scripts:
#   source "$(dirname "$0")/lib.sh"

set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────────

PHANTOM=/usr/local/bin/phantom
AGENT_LIB=/usr/local/lib/libphantom_agent.so

# ── Counters ─────────────────────────────────────────────────────────────────

PASS=0
FAIL=0
ERRORS=""
CURRENT_TEST=""

# ── Colours (if stdout is a terminal) ────────────────────────────────────────

if [ -t 1 ]; then
    GREEN='\033[0;32m'
    RED='\033[0;31m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    GREEN='' RED='' BOLD='' RESET=''
fi

# ── HTTP response generator ─────────────────────────────────────────────────
#
# Generates a well-formed HTTP/1.1 response with proper CRLF line endings.
# We generate at runtime to avoid CRLF issues in git.
#
# Usage: make_http_response STATUS BODY [EXTRA_HEADERS]
#   STATUS:        e.g. "200 OK", "404 Not Found"
#   BODY:          response body string
#   EXTRA_HEADERS: additional headers, each ending with \r\n
#                  e.g. "X-Custom: foo\r\nX-Other: bar\r\n"
#
# Output is written to stdout (redirect to a file for use with ncat).

make_http_response() {
    local status="$1"
    local body="$2"
    local extra_headers="${3:-}"
    local body_len=${#body}

    # %b interprets escape sequences (\r\n) in extra_headers.
    printf "HTTP/1.1 %s\r\nContent-Length: %d\r\nConnection: close\r\n%b\r\n%s" \
        "$status" "$body_len" "$extra_headers" "$body"
}

# ── Mock server helpers ──────────────────────────────────────────────────────

# Start a one-shot HTTP mock server on the given port.
# Reads the full request (discards it) and sends the canned response.
# Returns the server PID.
#
# Usage: start_mock_http PORT RESPONSE_FILE
start_mock_http() {
    local port="$1"
    local response_file="$2"
    # Redirect stdout/stderr so $() doesn't wait for the background process.
    ncat -l -p "$port" --send-only < "$response_file" >/dev/null 2>&1 &
    echo $!
}

# Start a one-shot HTTPS mock server (TLS with self-signed cert).
# Returns the server PID.
#
# Usage: start_mock_https PORT RESPONSE_FILE CERT_FILE KEY_FILE
start_mock_https() {
    local port="$1"
    local response_file="$2"
    local cert="$3"
    local key="$4"
    # Use openssl s_server instead of ncat --ssl (ncat --ssl --send-only
    # sends data before TLS handshake completes).
    # -quiet suppresses banner; stdin provides the HTTP response.
    openssl s_server -cert "$cert" -key "$key" -accept "$port" -quiet \
        < "$response_file" >/dev/null 2>&1 &
    echo $!
}

# ── Phantom runner ───────────────────────────────────────────────────────────

# Run phantom in JSONL+ldpreload mode, executing the given command.
# Stdout contains the JSONL trace output; stderr is suppressed.
#
# Usage: output=$(run_phantom_capture "curl -s http://...")
run_phantom_capture() {
    local cmd="$1"
    # Filter to only phantom JSONL lines (containing "trace_id").
    # The child process (curl) shares stdout with phantom, so its body output
    # may be interleaved.  We match on "trace_id" which is unique to phantom's
    # output, then extract the JSON object.
    timeout 10 "$PHANTOM" --backend ldpreload --output jsonl \
        --agent-lib "$AGENT_LIB" -- $cmd 2>/dev/null \
        | grep -o '{"timestamp_ms".*}$' || true
}

# ── Assertion helpers ────────────────────────────────────────────────────────

# Assert that a jq expression on JSON equals an expected string.
# Prints FAIL details on mismatch; returns 0/1.
assert_json_field() {
    local json="$1"
    local jq_expr="$2"
    local expected="$3"
    local actual
    actual=$(echo "$json" | jq -r "$jq_expr" 2>/dev/null) || actual="<jq error>"
    if [ "$actual" = "$expected" ]; then
        return 0
    else
        echo "  FAIL: $jq_expr = '$actual', expected '$expected'"
        return 1
    fi
}

# Assert that a jq expression result contains a substring.
assert_json_field_contains() {
    local json="$1"
    local jq_expr="$2"
    local substring="$3"
    local actual
    actual=$(echo "$json" | jq -r "$jq_expr" 2>/dev/null) || actual="<jq error>"
    if echo "$actual" | grep -qF "$substring"; then
        return 0
    else
        echo "  FAIL: $jq_expr = '$actual', expected to contain '$substring'"
        return 1
    fi
}

# Assert that a jq expression result does NOT contain a substring.
assert_json_field_not_contains() {
    local json="$1"
    local jq_expr="$2"
    local substring="$3"
    local actual
    actual=$(echo "$json" | jq -r "$jq_expr" 2>/dev/null) || actual="<jq error>"
    if echo "$actual" | grep -qF "$substring"; then
        echo "  FAIL: $jq_expr = '$actual', expected NOT to contain '$substring'"
        return 1
    else
        return 0
    fi
}

# Assert the number of non-empty lines in output.
assert_line_count() {
    local output="$1"
    local expected="$2"
    local actual
    actual=$(echo "$output" | grep -c '^{' || true)
    if [ "$actual" -eq "$expected" ]; then
        return 0
    else
        echo "  FAIL: line count = $actual, expected $expected"
        return 1
    fi
}

# ── Test runner ──────────────────────────────────────────────────────────────

# Run a named test function. Captures failures and continues.
#
# Usage: run_test "Test Name" test_function_name
run_test() {
    local name="$1"
    local func="$2"
    CURRENT_TEST="$name"
    local failed=0

    if "$func"; then
        printf "${GREEN}[PASS]${RESET} %s\n" "$name"
        PASS=$((PASS + 1))
    else
        printf "${RED}[FAIL]${RESET} %s\n" "$name"
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  - ${name}"
    fi
}

# Print final results and exit with appropriate code.
report_results() {
    echo ""
    printf "${BOLD}Results: %d passed, %d failed${RESET}\n" "$PASS" "$FAIL"
    if [ "$FAIL" -gt 0 ]; then
        printf "${RED}Failed tests:${RESET}%b\n" "$ERRORS"
        exit 1
    fi
    exit 0
}
