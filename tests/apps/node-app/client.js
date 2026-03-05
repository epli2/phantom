// A normal Node.js application that makes HTTP and HTTPS requests.
// This file contains ZERO proxy configuration — it talks directly to backends.
// Proxy injection is done externally via: node --require ./proxy-preload.js client.js
//
// Environment:
//   BACKEND_HTTP_URL  — e.g. http://127.0.0.1:3000
//   BACKEND_HTTPS_URL — e.g. https://127.0.0.1:3443  (optional)

"use strict";

const http = require("http");
const https = require("https");

const BACKEND_HTTP_URL = process.env.BACKEND_HTTP_URL;
const BACKEND_HTTPS_URL = process.env.BACKEND_HTTPS_URL;

if (!BACKEND_HTTP_URL) {
  console.error("BACKEND_HTTP_URL is required");
  process.exit(1);
}

// ---------------------------------------------------------------------------
// Simple promise wrappers around http.get / https.request
// ---------------------------------------------------------------------------

function httpGet(url) {
  return new Promise((resolve, reject) => {
    http.get(url, (res) => {
      let data = "";
      res.on("data", (chunk) => (data += chunk));
      res.on("end", () => resolve({ status: res.statusCode, body: data }));
    }).on("error", reject);
  });
}

function httpsGet(url) {
  return new Promise((resolve, reject) => {
    https.get(url, { rejectUnauthorized: false }, (res) => {
      let data = "";
      res.on("data", (chunk) => (data += chunk));
      res.on("end", () => resolve({ status: res.statusCode, body: data }));
    }).on("error", reject);
  });
}

function httpsPost(url, body) {
  return new Promise((resolve, reject) => {
    const bodyStr = JSON.stringify(body);
    const urlObj = new URL(url);
    const opts = {
      hostname: urlObj.hostname,
      port: urlObj.port,
      path: urlObj.pathname,
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "Content-Length": Buffer.byteLength(bodyStr),
      },
      rejectUnauthorized: false,
    };
    const req = https.request(opts, (res) => {
      let data = "";
      res.on("data", (chunk) => (data += chunk));
      res.on("end", () => resolve({ status: res.statusCode, body: data }));
    });
    req.on("error", reject);
    req.write(bodyStr);
    req.end();
  });
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  // ── HTTP requests ────────────────────────────────────────────────────
  const r1 = await httpGet(`${BACKEND_HTTP_URL}/api/health`);
  console.log(`http health: status=${r1.status} body=${r1.body}`);

  const r2 = await httpGet(`${BACKEND_HTTP_URL}/api/users`);
  console.log(`http users: status=${r2.status} body=${r2.body}`);

  // ── HTTPS requests (only if BACKEND_HTTPS_URL is provided) ──────────
  if (BACKEND_HTTPS_URL) {
    const r3 = await httpsGet(`${BACKEND_HTTPS_URL}/api/health`);
    console.log(`https health: status=${r3.status} body=${r3.body}`);

    const r4 = await httpsPost(`${BACKEND_HTTPS_URL}/api/users`, {
      name: "Charlie",
      email: "charlie@example.com",
    });
    console.log(`https create: status=${r4.status} body=${r4.body}`);
  }

  console.log("CLIENT_DONE");
}

main().catch((err) => {
  console.error("Client error:", err);
  process.exit(1);
});
