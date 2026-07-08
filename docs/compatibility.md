# Runtime / Client Compatibility Matrix

This document tracks, honestly, which HTTP client runtimes phantom's **proxy backend**
(`--backend proxy`, the default) can transparently trace — and what's required for
each. "Verified" means covered by an automated integration test in `tests/`; anything
else is a known gap or genuinely untested.

The **LD_PRELOAD backend** (`--backend ldpreload`, Linux only) is a separate mechanism
(libc `send`/`recv` hooks) and is not covered by this matrix — it captures plain HTTP
only, regardless of runtime, and has no HTTPS story.

## Summary table

| Runtime / client | HTTP capture | HTTPS capture | Requirements | Verified by |
|---|---|---|---|---|
| Node.js (`http`/`https`/`axios`/`undici`/`fetch`) | ✅ | ✅ | Nothing — phantom auto-injects `proxy-preload.js` via `--require` | `tests/proxy_node_integration.rs` |
| curl | ✅ | ✅ | Nothing — `HTTP_PROXY`/`HTTPS_PROXY` + `CURL_CA_BUNDLE` are auto-set | `tests/proxy_curl_https_integration.rs`, `tests/proxy_gzip_integration.rs` |
| Python 3 (`urllib.request` stdlib) | ✅ | ✅ (see limitation 3 on strict OpenSSL/LibreSSL builds) | Nothing — `HTTP_PROXY`/`HTTPS_PROXY` + `SSL_CERT_FILE` are auto-set and honoured by Python's OpenSSL-backed `ssl` module | `tests/proxy_python_integration.rs` |
| Python 3 (`requests`) | Expected to work (untested) | Expected to work (untested) | Same as above; `requests` additionally reads `REQUESTS_CA_BUNDLE`, which phantom also sets | — |
| Go (`net/http`) | ✅ (non-loopback targets only) | ❌ (see limitation 2) | `HTTP_PROXY` auto-set; target host must not be `localhost`/loopback (see limitation 1) | `tests/proxy_go_integration.rs` |
| Java (JDK HttpClient, Apache HttpClient 5) | ✅ | ✅ | Nothing — phantom injects `-javaagent` + JVM proxy system properties via `JAVA_TOOL_OPTIONS` (see PR #4) | `tests/proxy_java_clients_integration.rs` |
| Deno | Expected to work (untested) | Expected to work (untested) | `HTTPS_PROXY` + `DENO_CERT` are auto-set | — |
| Bun | Expected to work (untested) | Untested — likely needs the same preload treatment as Node | — |
| Ruby (`net/http`) | Expected to work (untested) | Expected to work (untested) — Ruby's OpenSSL binding should honour `SSL_CERT_FILE` | — |
| Rust (`reqwest`, native-tls/rustls) | Untested | Untested — `rustls`-based clients do **not** read `SSL_CERT_FILE` by default (Rust has no OS-trust-store convention); would need explicit code | — |

## Known limitations (verified while building the above)

### 1. Go's `net/http` never proxies loopback destinations

`net/http.ProxyFromEnvironment` (used by `http.DefaultClient`/`http.DefaultTransport`)
unconditionally refuses to route a request through any proxy if the request's host is
the literal string `localhost` or any IP in the loopback range (`127.0.0.0/8`, `::1`) —
**regardless of `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`**. This is intentional upstream Go
behavior (a hardening measure against SSRF via proxy env vars), not a phantom bug, and
it cannot be worked around from phantom's side.

Practical impact: tracing a Go program that talks to `http://localhost:3000` (a very
common local-dev pattern) will **not** capture that traffic — the request bypasses
phantom's proxy entirely and goes straight to the target. Traffic to any other host
(a real hostname, a Docker service name, a non-loopback IP) is unaffected and traces
normally.

Confirmed directly against `go1.24`:

```go
req, _ := http.NewRequest("GET", "https://localhost:9999/x", nil)
http.ProxyFromEnvironment(req) // -> nil, even with HTTPS_PROXY set

req, _ = http.NewRequest("GET", "https://example.com:9999/x", nil)
http.ProxyFromEnvironment(req) // -> the configured proxy, as expected
```

### 2. phantom's MITM certificate never carries an IP SAN

When the CONNECT target is an IP literal (e.g. `https://192.168.1.5:8443/`), phantom's
proxy (via `hudsucker::certificate_authority::RcgenAuthority`) still generates the
leaf certificate with only a **DNS-name** SAN containing that IP string — never a
proper **IP-address** SAN. Clients that perform strict RFC 6125 hostname verification
for IP-literal hosts (Go's `crypto/tls` is one; this is not Go-specific) reject the
certificate outright:

```
x509: cannot validate certificate for 192.168.1.5 because it doesn't contain any IP SANs
```

Practical impact: HTTPS capture only works for **named hosts** today (the overwhelmingly
common case — `https://api.example.com`, `https://localhost` conventions aside). Direct
IP-literal HTTPS targets will fail TLS verification client-side, independent of which
language/runtime is used. This is a real gap in phantom's own cert generation, not a
per-runtime quirk — fixing it (teach `proxy.rs`'s CA/cert path to add an IP SAN when the
CONNECT authority parses as an IP) is tracked as future work, not fixed in this pass.

### 3. phantom's MITM leaf certificates never carry an Authority Key Identifier

`hudsucker::certificate_authority::RcgenAuthority` (the MITM certificate generator
phantom uses) builds every per-site leaf certificate from `rcgen::CertificateParams
::default()`, which leaves `use_authority_key_identifier_extension` at its default of
`false`. Confirmed against both the pinned `hudsucker 0.22.0` and the latest upstream
`main` branch — this is not fixed in a newer release, it is how `RcgenAuthority::gen_cert`
is written today, and there is no public API to opt in without replacing the
`CertificateAuthority` implementation entirely.

Most TLS stacks tolerate a leaf certificate without an AKI extension (it's optional per
RFC 5280 for certs whose issuer can be identified another way). Some don't: macOS's
Homebrew-built Python 3.14 (linked against a stricter OpenSSL/LibreSSL) rejects it
outright:

```
ssl.SSLCertVerificationError: [SSL: CERTIFICATE_VERIFY_FAILED] certificate verify
failed: Missing Authority Key Identifier
```

Practical impact: HTTPS interception can fail client-side on TLS stacks that enforce
this strictly, independent of which language/runtime is used — it's a property of the
generated certificate, not of any particular client. `tests/proxy_python_integration.rs`
detects exactly this failure (via a distinct marker printed by
`tests/apps/python-app/client.py`) and treats it as a known, non-regression outcome
rather than a test failure, so the HTTP leg of that test still guards against real
breakage everywhere, and the HTTPS leg still guards against it wherever the local
OpenSSL/LibreSSL build is lenient (e.g. Linux CI). A proper fix requires phantom to stop
using `RcgenAuthority` and implement `hudsucker::certificate_authority::CertificateAuthority`
directly so it can set `use_authority_key_identifier_extension = true` on generated leaf
certs; tracked as future work, not fixed in this pass.

## How to add a runtime to this matrix

1. Add a minimal, dependency-free (or dependency-light) test client under
   `tests/apps/<runtime>-app/`.
2. Add `tests/proxy_<runtime>_integration.rs` following the existing tests' pattern:
   skip gracefully if the runtime isn't on `PATH`, spin up local HTTP(S) mock backends,
   run `phantom --output jsonl -- <runtime> <client>`, and assert on the JSONL trace.
3. Update the table above with real results — including honest negatives.
