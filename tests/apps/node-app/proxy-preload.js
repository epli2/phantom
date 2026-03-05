// Transparent HTTP/HTTPS proxy injection via Node.js --require.
//
// When loaded with `node --require ./proxy-preload.js app.js`, this script
// monkey-patches the built-in `http` and `https` modules to route all outbound
// requests through an HTTP proxy — without touching the application code.
//
// Activated by the HTTP_PROXY (or http_proxy) environment variable.
// If neither is set, this script is a no-op and the app runs normally.
//
// This is the Node.js equivalent of LD_PRELOAD for transparent interception.

"use strict";

const PROXY_URL = process.env.HTTP_PROXY || process.env.http_proxy;
if (!PROXY_URL) {
  // No proxy configured — do nothing.
  return;
}

const http = require("http");
const https = require("https");
const tls = require("tls");
const { URL } = require("url");

const proxy = new URL(PROXY_URL);
const proxyHost = proxy.hostname;
const proxyPort = parseInt(proxy.port, 10);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Convert a URL string or URL object to http.request options. */
function urlToOptions(input) {
  const u = typeof input === "string" ? new URL(input) : input;
  return {
    protocol: u.protocol,
    hostname: u.hostname,
    port: u.port || (u.protocol === "https:" ? 443 : 80),
    path: u.pathname + u.search,
    hash: u.hash,
  };
}

/** Normalise the (url, options, callback) overloaded arguments. */
function normaliseArgs(args) {
  let options, callback;
  if (typeof args[0] === "string" || args[0] instanceof URL) {
    const urlOpts = urlToOptions(args[0]);
    options =
      typeof args[1] === "object" && typeof args[1] !== "function"
        ? { ...urlOpts, ...args[1] }
        : urlOpts;
    callback = typeof args[1] === "function" ? args[1] : args[2];
  } else {
    options = args[0] || {};
    callback = args[1];
  }
  return { options, callback };
}

// ---------------------------------------------------------------------------
// HTTP patching — rewrite request target to go through proxy
// ---------------------------------------------------------------------------

const origHttpRequest = http.request;
const origHttpGet = http.get;

http.request = function (...args) {
  const { options, callback } = normaliseArgs(args);

  const host = options.hostname || options.host || "localhost";
  const port = options.port || 80;
  const path = options.path || "/";

  // Build the absolute URI the proxy expects.
  const absoluteUri = `http://${host}:${port}${path}`;

  const proxyOpts = {
    ...options,
    hostname: proxyHost,
    port: proxyPort,
    path: absoluteUri,
    host: `${proxyHost}:${proxyPort}`,
    headers: {
      ...options.headers,
      Host: port == 80 ? host : `${host}:${port}`,
    },
  };

  return origHttpRequest.call(http, proxyOpts, callback);
};

http.get = function (...args) {
  const req = http.request(...args);
  req.end();
  return req;
};

// ---------------------------------------------------------------------------
// HTTPS patching — CONNECT tunnel through proxy, then TLS handshake
// ---------------------------------------------------------------------------

const origHttpsRequest = https.request;
const origHttpsGet = https.get;

/**
 * Custom HTTPS agent that tunnels through the HTTP proxy using CONNECT.
 *
 * Flow:
 *   1. Open TCP to proxy via origHttpRequest (bypasses our http.request patch)
 *   2. Send CONNECT target:port
 *   3. On 200, wrap the raw socket with tls.connect
 *   4. Return the TLS socket to Node's https machinery
 */
class ProxyTunnelAgent extends https.Agent {
  createConnection(options, oncreate) {
    const targetHost = options.hostname || options.host;
    const targetPort = options.port || 443;

    const connectReq = origHttpRequest.call(http, {
      hostname: proxyHost,
      port: proxyPort,
      method: "CONNECT",
      path: `${targetHost}:${targetPort}`,
      headers: { Host: `${targetHost}:${targetPort}` },
    });

    connectReq.on("connect", (_res, socket) => {
      const tlsSocket = tls.connect(
        {
          socket,
          servername: targetHost,
          // Trust the MITM proxy's dynamically-generated certificates.
          rejectUnauthorized: false,
        },
        () => oncreate(null, tlsSocket)
      );
      tlsSocket.on("error", (err) => oncreate(err));
    });

    connectReq.on("error", (err) => oncreate(err));
    connectReq.end();
  }
}

const tunnelAgent = new ProxyTunnelAgent({
  keepAlive: false,
  rejectUnauthorized: false,
});

https.request = function (...args) {
  const { options, callback } = normaliseArgs(args);
  // Force all HTTPS requests through the tunnel agent.
  options.agent = tunnelAgent;
  return origHttpsRequest.call(https, options, callback);
};

https.get = function (...args) {
  const req = https.request(...args);
  req.end();
  return req;
};
