// Test client for phantom's Go compatibility check.
//
// Uses only net/http (no go.mod / external deps needed — `go run client.go`
// works standalone). Makes an HTTP GET, and an HTTPS GET if
// BACKEND_HTTPS_URL is set, each tagged with an `x-phantom-client: go`
// header for trace identification.
//
// Relies entirely on phantom's automatically-injected environment:
//   - HTTP_PROXY / HTTPS_PROXY for routing (http.ProxyFromEnvironment)
//   - SSL_CERT_FILE for trusting the phantom CA (Go's crypto/x509 honours
//     this env var as an override to the system root pool on Linux)
//
// No proxy-aware code here on purpose — this is the point of the test. See
// tests/proxy_go_integration.rs for known limitations (loopback targets are
// never proxied by Go; IP-literal HTTPS targets aren't trusted by phantom's
// MITM cert) that shape how this client is actually exercised today.
package main

import (
	"fmt"
	"io"
	"net/http"
	"os"
)

func fetch(url string) error {
	req, err := http.NewRequest("GET", url, nil)
	if err != nil {
		return err
	}
	req.Header.Set("x-phantom-client", "go")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "go %s -> %d %s\n", url, resp.StatusCode, string(body))
	return nil
}

func main() {
	httpURL := os.Getenv("BACKEND_HTTP_URL") + "/api/health"
	if err := fetch(httpURL); err != nil {
		fmt.Fprintf(os.Stderr, "go: HTTP request failed: %v\n", err)
		os.Exit(1)
	}

	if base := os.Getenv("BACKEND_HTTPS_URL"); base != "" {
		if err := fetch(base + "/api/health"); err != nil {
			fmt.Fprintf(os.Stderr, "go: HTTPS request failed: %v\n", err)
			os.Exit(1)
		}
	}
}
