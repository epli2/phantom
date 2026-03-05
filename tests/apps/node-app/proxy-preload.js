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
  const port = Number(options.port) || 80;
  const path = options.path || "/";

  // If this request already has an absolute-URI path AND is connecting to our
  // proxy, it was pre-formatted by a proxy-aware client (e.g. axios reading
  // HTTP_PROXY automatically).  Pass it through unchanged to avoid double-wrapping.
  if (
    (path.startsWith("http://") || path.startsWith("https://")) &&
    host === proxyHost &&
    port === proxyPort
  ) {
    return origHttpRequest.call(http, options, callback);
  }

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
      Host: port === 80 ? host : `${host}:${port}`,
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

// ---------------------------------------------------------------------------
// Undici / native fetch (Node.js 18+)
// ---------------------------------------------------------------------------
// undici implements its own HTTP stack and does NOT go through the patched
// http/https modules above.  Redirect it through the proxy by installing a
// global ProxyAgent dispatcher.  This also covers globalThis.fetch, which is
// built on top of undici in Node.js 18+.

(function patchUndici() {
  let undici;
  // Try the built-in first (Node 18.13+), then fall back to the npm package.
  for (const mod of ["node:undici", "undici"]) {
    try {
      undici = require(mod);
      break;
    } catch (_) {}
  }
  if (!undici || !undici.ProxyAgent || !undici.setGlobalDispatcher) return;
  try {
    undici.setGlobalDispatcher(
      new undici.ProxyAgent({
        uri: PROXY_URL,
        // phantom presents a MITM certificate — skip TLS verification.
        connect: { rejectUnauthorized: false },
      })
    );
  } catch (_) {
    // Ignore: proxy agent creation can fail in some environments.
  }
})();

// ---------------------------------------------------------------------------
// Patch globalThis.fetch for HTTP
// ---------------------------------------------------------------------------
// undici's ProxyAgent (set above) sends a CONNECT tunnel for *all* requests
// made via fetch(), including plain HTTP.  phantom handles CONNECT as an HTTPS
// MITM tunnel — sending plain HTTP through it fails ("Connection reset by peer").
//
// Fix: wrap globalThis.fetch so that http:// requests are dispatched through
// our already-patched http.request instead of through the ProxyAgent.
// https:// requests are left unchanged (CONNECT → MITM works fine for TLS).

if (typeof globalThis.fetch === "function") {
  const _origFetch = globalThis.fetch;

  globalThis.fetch = async function patchedFetch(input, init) {
    // Determine the target URL string.
    let urlStr;
    try {
      urlStr =
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.href
            : input.url;
    } catch (_) {
      return _origFetch.apply(this, arguments);
    }

    // Only intercept plain HTTP; leave HTTPS to the ProxyAgent.
    if (!urlStr.startsWith("http://")) {
      return _origFetch.apply(this, arguments);
    }

    const url = new URL(urlStr);
    const method = (
      (init && init.method) ||
      (input && input.method) ||
      "GET"
    ).toUpperCase();

    // Normalise headers to a plain object.
    const rawHeaders =
      (init && init.headers) || (input && input.headers) || {};
    const headers = {};
    if (rawHeaders && typeof rawHeaders.entries === "function") {
      for (const [k, v] of rawHeaders.entries()) headers[k] = v;
    } else {
      Object.assign(headers, rawHeaders);
    }

    // Serialise body to a Buffer if present.
    let bodyData = null;
    const rawBody = init && init.body;
    if (rawBody != null) {
      if (typeof rawBody === "string") bodyData = rawBody;
      else if (Buffer.isBuffer(rawBody)) bodyData = rawBody;
      else if (rawBody instanceof ArrayBuffer) bodyData = Buffer.from(rawBody);
      else if (rawBody instanceof Uint8Array) bodyData = Buffer.from(rawBody);
      // Streams, FormData, URLSearchParams, etc. fall through to origFetch.
      else return _origFetch.apply(this, arguments);
    }

    // Dispatch through our patched http.request → goes via phantom proxy.
    return new Promise((resolve, reject) => {
      const opts = {
        hostname: url.hostname,
        port: url.port || 80,
        path: url.pathname + (url.search || ""),
        method,
        headers,
      };

      const req = http.request(opts, (res) => {
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => {
          const body = Buffer.concat(chunks);
          // Node 18+ exposes Headers and Response as globals.
          resolve(
            new Response(body, {
              status: res.statusCode,
              statusText: res.statusMessage || "",
              headers: new Headers(res.headers),
            })
          );
        });
        res.on("error", reject);
      });

      req.on("error", reject);
      if (bodyData != null) req.write(bodyData);
      req.end();
    });
  };
}
