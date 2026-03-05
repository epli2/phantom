// client-alts.js — Tests alternative HTTP client libraries through the proxy.
//
// Uses axios, undici (node:undici built-in), and the global fetch API —
// all with ZERO proxy configuration in this file.
// Proxy injection is handled externally via: node --require ./proxy-preload.js
//
// Each client adds an `x-phantom-client` header so traces can be identified
// in the JSONL output by inspecting request_headers.
//
// Environment:
//   BACKEND_HTTP_URL  — e.g. http://127.0.0.1:3000
//   BACKEND_HTTPS_URL — e.g. https://localhost:3443

"use strict";

const BACKEND_HTTP_URL = process.env.BACKEND_HTTP_URL;
const BACKEND_HTTPS_URL = process.env.BACKEND_HTTPS_URL;

if (!BACKEND_HTTP_URL || !BACKEND_HTTPS_URL) {
  console.error("BACKEND_HTTP_URL and BACKEND_HTTPS_URL are required");
  process.exit(1);
}

async function main() {
  // ── axios ──────────────────────────────────────────────────────────────────
  // axios in Node.js uses the built-in http/https modules internally, so it
  // goes through the patched http.request / https.request automatically.
  const axios = require("axios");
  // For HTTPS, disable certificate verification for the self-signed test cert.
  const axiosHttps = axios.create({
    httpsAgent: new (require("https").Agent)({ rejectUnauthorized: false }),
  });

  const a1 = await axios.get(`${BACKEND_HTTP_URL}/api/health`, {
    headers: { "x-phantom-client": "axios" },
  });
  console.log(`axios  http health: status=${a1.status}`);

  const a2 = await axiosHttps.get(`${BACKEND_HTTPS_URL}/api/health`, {
    headers: { "x-phantom-client": "axios" },
  });
  console.log(`axios  https health: status=${a2.status}`);

  // ── undici ─────────────────────────────────────────────────────────────────
  // undici has its own HTTP stack and bypasses http/https modules.
  // proxy-preload.js patches it via setGlobalDispatcher(ProxyAgent).
  const { request } = require("undici");

  const u1 = await request(`${BACKEND_HTTP_URL}/api/users`, {
    headers: { "x-phantom-client": "undici" },
  });
  await u1.body.text(); // consume body to allow connection to close
  console.log(`undici http users: status=${u1.statusCode}`);

  const u2 = await request(`${BACKEND_HTTPS_URL}/api/users`, {
    headers: { "x-phantom-client": "undici" },
  });
  await u2.body.text();
  console.log(`undici https users: status=${u2.statusCode}`);

  // ── fetch (global, Node.js 18+) ────────────────────────────────────────────
  // proxy-preload.js sets undici's global ProxyAgent dispatcher so that
  // requests from fetch go through the phantom proxy.  HTTP_PROXY is removed
  // from the environment after our patches are installed to prevent fetch's
  // built-in proxy detection from conflicting with the ProxyAgent.
  const postBody = JSON.stringify({ name: "Dave", email: "dave@example.com" });
  const postHeaders = {
    "content-type": "application/json",
    "x-phantom-client": "fetch",
  };

  const f1 = await fetch(`${BACKEND_HTTP_URL}/api/users`, {
    method: "POST",
    headers: postHeaders,
    body: postBody,
  });
  console.log(`fetch  http  post users: status=${f1.status}`);

  const f2 = await fetch(`${BACKEND_HTTPS_URL}/api/users`, {
    method: "POST",
    headers: postHeaders,
    body: postBody,
  });
  console.log(`fetch  https post users: status=${f2.status}`);

  console.log("CLIENT_DONE");
}

main().catch((err) => {
  console.error("Client error:", err);
  process.exit(1);
});
