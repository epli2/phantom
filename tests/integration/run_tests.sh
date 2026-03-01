#!/usr/bin/env bash
# ── Phantom LD_PRELOAD agent — integration test suite ────────────────────────
#
# Runs inside a Docker container (debian:bookworm-slim) with phantom, the
# agent dylib, curl, ncat, jq, and openssl pre-installed.
#
# Usage:
#   make docker-test-integration      (from host)
#   bash /tests/integration/run_tests.sh   (inside container)
#
# Each test:
#   1. Starts a one-shot ncat mock server (HTTP or HTTPS)
#   2. Runs phantom in JSONL mode tracing a curl command
#   3. Asserts the JSONL output using jq

set -euo pipefail
source "$(dirname "$0")/lib.sh"

# ── Generate self-signed cert for HTTPS tests ────────────────────────────────

CERT_DIR=$(mktemp -d)
openssl req -x509 -newkey rsa:2048 \
    -keyout "$CERT_DIR/key.pem" \
    -out "$CERT_DIR/cert.pem" \
    -days 1 -nodes -subj "/CN=localhost" 2>/dev/null

cleanup() {
    rm -rf "$CERT_DIR"
}
trap cleanup EXIT

echo "phantom LD_PRELOAD agent — integration tests"
echo "============================================="
echo ""

# ── Test 1: HTTP GET 200 ────────────────────────────────────────────────────

test_http_get_200() {
    local port=18081
    local body='{"message":"hello"}'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "200 OK" "$body" "Content-Type: application/json\r\n" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 http://127.0.0.1:$port/test")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.method' 'GET' &&
    assert_json_field "$line" '.status_code' '200' &&
    assert_json_field "$line" '.url' "http://127.0.0.1:$port/test" &&
    assert_json_field_contains "$line" '.response_body' 'hello'
}
run_test "HTTP GET 200" test_http_get_200

# ── Test 2: HTTP POST with body ─────────────────────────────────────────────

test_http_post_body() {
    local port=18082
    local body='{"result":"created"}'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "201 Created" "$body" "Content-Type: application/json\r\n" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 -X POST -H Content-Type:application/json -d {\"key\":\"value\"} http://127.0.0.1:$port/create")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.method' 'POST' &&
    assert_json_field "$line" '.status_code' '201' &&
    assert_json_field_contains "$line" '.request_body' 'key' &&
    assert_json_field_contains "$line" '.response_body' 'created'
}
run_test "HTTP POST with body" test_http_post_body

# ── Test 3: HTTP 404 ────────────────────────────────────────────────────────

test_http_404() {
    local port=18083
    local body='Not Found'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "404 Not Found" "$body" "" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 http://127.0.0.1:$port/missing")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.status_code' '404'
}
run_test "HTTP 404" test_http_404

# ── Test 4: HTTP 500 ────────────────────────────────────────────────────────

test_http_500() {
    local port=18084
    local body='Internal Server Error'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "500 Internal Server Error" "$body" "" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 http://127.0.0.1:$port/error")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.status_code' '500'
}
run_test "HTTP 500" test_http_500

# ── Test 5: HTTP multiple sequential requests ───────────────────────────────

test_http_multiple() {
    local port_a=18085 port_b=18086 port_c=18087
    local resp_a resp_b resp_c
    resp_a=$(mktemp); resp_b=$(mktemp); resp_c=$(mktemp)

    make_http_response "200 OK" '{"n":1}' "" > "$resp_a"
    make_http_response "200 OK" '{"n":2}' "" > "$resp_b"
    make_http_response "200 OK" '{"n":3}' "" > "$resp_c"

    local pid_a pid_b pid_c
    pid_a=$(start_mock_http "$port_a" "$resp_a")
    pid_b=$(start_mock_http "$port_b" "$resp_b")
    pid_c=$(start_mock_http "$port_c" "$resp_c")
    sleep 0.2

    # Write a helper script that phantom will execute via bash.
    local script
    script=$(mktemp /tmp/multi_curl_XXXX.sh)
    cat > "$script" <<SCRIPT
#!/bin/bash
curl -s --http1.1 http://127.0.0.1:${port_a}/a
curl -s --http1.1 http://127.0.0.1:${port_b}/b
curl -s --http1.1 http://127.0.0.1:${port_c}/c
SCRIPT
    chmod +x "$script"

    local output
    output=$(run_phantom_capture "$script")

    kill "$pid_a" "$pid_b" "$pid_c" 2>/dev/null; wait "$pid_a" "$pid_b" "$pid_c" 2>/dev/null || true
    rm -f "$resp_a" "$resp_b" "$resp_c" "$script"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    assert_line_count "$output" 3
}
run_test "HTTP multiple sequential" test_http_multiple

# ── Test 6: HTTP response headers ───────────────────────────────────────────

test_http_response_headers() {
    local port=18088
    local body='ok'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "200 OK" "$body" "X-Custom-Header: phantom-test\r\nX-Request-Id: abc123\r\n" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 http://127.0.0.1:$port/headers")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.response_headers["x-custom-header"]' 'phantom-test' &&
    assert_json_field "$line" '.response_headers["x-request-id"]' 'abc123'
}
run_test "HTTP response headers" test_http_response_headers

# ── Test 7: HTTP request headers ────────────────────────────────────────────

test_http_request_headers() {
    local port=18089
    local body='ok'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "200 OK" "$body" "" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 -H X-My-Header:test-value http://127.0.0.1:$port/reqhdr")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.request_headers["x-my-header"]' 'test-value'
}
run_test "HTTP request headers" test_http_request_headers

# ── Test 8: HTTP large body ─────────────────────────────────────────────────

test_http_large_body() {
    local port=18090
    # Generate a 20KB body (larger than MAX_BODY=16384)
    local body
    body=$(dd if=/dev/zero bs=1 count=20480 2>/dev/null | tr '\0' 'A')
    local resp_file
    resp_file=$(mktemp)
    make_http_response "200 OK" "$body" "" > "$resp_file"

    local pid
    pid=$(start_mock_http "$port" "$resp_file")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http1.1 http://127.0.0.1:$port/large")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    # Should have a response body (truncated to MAX_BODY)
    assert_json_field "$line" '.status_code' '200' &&
    assert_json_field_contains "$line" '.response_body' 'AAAA'
}
run_test "HTTP large body" test_http_large_body

# ── Test 9: HTTPS GET 200 ──────────────────────────────────────────────────

test_https_get_200() {
    local port=18091
    local body='{"secure":true}'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "200 OK" "$body" "Content-Type: application/json\r\n" > "$resp_file"

    local pid
    pid=$(start_mock_https "$port" "$resp_file" "$CERT_DIR/cert.pem" "$CERT_DIR/key.pem")
    sleep 0.5

    local output
    output=$(run_phantom_capture "curl -sk --http1.1 https://127.0.0.1:$port/secure")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.method' 'GET' &&
    assert_json_field "$line" '.status_code' '200' &&
    assert_json_field "$line" '.url' "https://127.0.0.1:$port/secure" &&
    assert_json_field_contains "$line" '.response_body' 'secure'
}
run_test "HTTPS GET 200" test_https_get_200

# ── Test 10: HTTPS POST with body ──────────────────────────────────────────

test_https_post_body() {
    local port=18092
    local body='{"encrypted":"yes"}'
    local resp_file
    resp_file=$(mktemp)
    make_http_response "200 OK" "$body" "Content-Type: application/json\r\n" > "$resp_file"

    local pid
    pid=$(start_mock_https "$port" "$resp_file" "$CERT_DIR/cert.pem" "$CERT_DIR/key.pem")
    sleep 0.5

    local output
    output=$(run_phantom_capture "curl -sk --http1.1 -X POST -d {\"tls\":\"data\"} https://127.0.0.1:$port/post")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    rm -f "$resp_file"

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field "$line" '.method' 'POST' &&
    assert_json_field "$line" '.status_code' '200' &&
    assert_json_field "$line" '.url' "https://127.0.0.1:$port/post" &&
    assert_json_field_contains "$line" '.request_body' 'tls' &&
    assert_json_field_contains "$line" '.response_body' 'encrypted'
}
run_test "HTTPS POST with body" test_https_post_body

# ── Test 11: HTTP/2 cleartext GET ───────────────────────────────────────────
#
# Uses curl --http2-prior-knowledge (h2c) to send a plain-TCP HTTP/2 request.
# The agent should detect the HTTP/2 client preface, parse the binary frames,
# and emit a trace with protocol_version="HTTP/2".

test_h2c_get_200() {
    # Require curl with HTTP/2 support.
    if ! curl --version 2>&1 | grep -q 'HTTP2'; then
        echo "  SKIP: curl lacks HTTP/2 support"
        return 0
    fi

    local port=18093
    local body='{"h2":true}'

    local pid
    pid=$(start_mock_h2c "$port" "$body")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http2-prior-knowledge http://127.0.0.1:$port/h2test")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field         "$line" '.protocol_version'       'HTTP/2'      &&
    assert_json_field         "$line" '.method'                 'GET'         &&
    assert_json_field         "$line" '.status_code'            '200'         &&
    assert_json_field_contains "$line" '.url'                   '/h2test'     &&
    assert_json_field_contains "$line" '.response_body'         'h2'
}
run_test "HTTP/2 cleartext GET" test_h2c_get_200

# ── Test 12: HTTP/2 cleartext POST with body ─────────────────────────────────

test_h2c_post_body() {
    if ! curl --version 2>&1 | grep -q 'HTTP2'; then
        echo "  SKIP: curl lacks HTTP/2 support"
        return 0
    fi

    local port=18094
    local body='{"posted":true}'

    local pid
    pid=$(start_mock_h2c "$port" "$body")
    sleep 0.2

    local output
    output=$(run_phantom_capture "curl -s --http2-prior-knowledge -X POST -d {\"key\":\"val\"} http://127.0.0.1:$port/h2post")

    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true

    [ -n "$output" ] || { echo "  FAIL: no output"; return 1; }

    local line
    line=$(echo "$output" | head -1)
    assert_json_field         "$line" '.protocol_version'       'HTTP/2'      &&
    assert_json_field         "$line" '.method'                 'POST'        &&
    assert_json_field         "$line" '.status_code'            '200'         &&
    assert_json_field_contains "$line" '.url'                   '/h2post'     &&
    assert_json_field_contains "$line" '.request_body'          'key'         &&
    assert_json_field_contains "$line" '.response_body'         'posted'
}
run_test "HTTP/2 cleartext POST" test_h2c_post_body

# ── Results ──────────────────────────────────────────────────────────────────

report_results
