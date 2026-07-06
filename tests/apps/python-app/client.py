#!/usr/bin/env python3
"""Test client for phantom's Python compatibility check.

Uses only the standard library (urllib.request) so the integration test has
no pip-install step. Makes one HTTP GET and one HTTPS GET, each tagged with
an `x-phantom-client: python` header for trace identification.

Relies entirely on phantom's automatically-injected environment:
  - HTTP_PROXY / HTTPS_PROXY for routing
  - SSL_CERT_FILE for trusting the phantom CA (Python's ssl module, being
    OpenSSL-backed, honours this for the default verify paths)

No proxy-aware code here on purpose — this is the point of the test.
"""

import os
import sys
import urllib.request

http_url = os.environ["BACKEND_HTTP_URL"] + "/api/health"
https_url = os.environ["BACKEND_HTTPS_URL"] + "/api/health"


def fetch(url: str) -> None:
    req = urllib.request.Request(url, headers={"x-phantom-client": "python"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        body = resp.read().decode("utf-8")
        print(f"python {url} -> {resp.status} {body}", file=sys.stderr)


fetch(http_url)
fetch(https_url)
