# Phantom JSONL Schema

This is the authoritative reference for phantom's `--output jsonl` record format.
`AGENTS.md`, `README.md`, and `phantom --help` all summarize this document —
if they disagree with anything here, this file wins.

## Compatibility policy

- `schema_version` identifies the JSONL record shape. It is currently `2`.
- **Within the same `schema_version`, changes are additive only**: new fields
  may be added, but existing fields are never removed, renamed, or given a
  different meaning or type. Consumers (scripts, `jq` pipelines, AI agents)
  can safely ignore fields they don't recognize.
- A breaking change (field removal, type change, meaning change) always comes
  with a `schema_version` bump, announced in `CHANGELOG.md` (once it exists)
  and this document.

## Schema version history

| Version | Summary |
|---|---|
| 1 | Initial shape (no `schema_version` field). |
| 2 | Added `schema_version`; body encoding/truncation/Content-Encoding fields (see below). |

## Fields (schema_version 2)

One JSON object per line. All fields are always present unless marked
optional (`?`).

| Field | Type | Description |
|---|---|---|
| `schema_version` | number | Always `2` for this document's shape. |
| `trace_id` | string | W3C-compatible 128-bit trace ID (hex, 32 chars) |
| `span_id` | string | 64-bit span ID (hex, 16 chars) |
| `timestamp_ms` | number | Unix epoch milliseconds — request start time |
| `duration_ms` | number | Round-trip latency in milliseconds |
| `method` | string | HTTP verb: `"GET"`, `"POST"`, `"PUT"`, `"DELETE"`, … |
| `url` | string | Full request URL (scheme + host + path + query) |
| `status_code` | number | HTTP response status code |
| `protocol_version` | string | HTTP version string, e.g. `"HTTP/1.1"` |
| `request_headers` | object | Lower-cased header names → values |
| `response_headers` | object | Lower-cased header names → values |
| `request_body` | string? | Request body; omitted when empty. See encoding below. |
| `response_body` | string? | Response body; omitted when empty. See encoding below. |
| `request_body_encoding` | string? | `"utf-8"` or `"base64"`. Present whenever `request_body` is present. |
| `response_body_encoding` | string? | `"utf-8"` or `"base64"`. Present whenever `response_body` is present. |
| `request_body_truncated` | boolean | `true` if `request_body` was cut off at the `--max-body` limit. |
| `response_body_truncated` | boolean | `true` if `response_body` was cut off at the `--max-body` limit. |
| `request_content_encoding` | string? | Original `Content-Encoding` of the request (e.g. `"gzip"`), if it was transparently decoded for this record. Omitted when the request had no (or `identity`) encoding. |
| `response_content_encoding` | string? | Same as above, for the response. |
| `source_addr` | string? | Client socket address, e.g. `"127.0.0.1:54321"` |
| `dest_addr` | string? | Server socket address, e.g. `"93.184.216.34:443"` |

### Body encoding details

- Bodies detected as binary (first 8 KB contains a NUL byte, or more than 10%
  of that sample is invalid UTF-8) are base64-encoded, with
  `*_body_encoding: "base64"`. Text bodies are stored as UTF-8 with lossy
  replacement of any invalid byte sequences, `*_body_encoding: "utf-8"`.
- If the original body was compressed (`Content-Encoding: gzip`/`br`/`zstd`/
  `deflate`), phantom decodes it before applying the above — `*_body` is the
  **decoded** plaintext, and `*_content_encoding` records what it was decoded
  from. The bytes phantom forwards on the wire (to the real client or server)
  are never altered by this — only the recorded/JSONL copy is decoded.
- If decoding a declared `Content-Encoding` fails (corrupt or unsupported
  encoding), phantom stores the raw bytes as-is and omits
  `*_content_encoding` for that record — this is logged as a warning, not a
  hard error.
- Truncation is applied at `--max-body` (default `1mb`; `0` disables it) both
  before and after decoding, since decompression can expand a small payload
  well past its wire size. `*_body_truncated` reflects either case.

## Example record

```json
{
  "schema_version": 2,
  "trace_id": "13a75c87ab6725a6e1ea79e01340d0ce",
  "span_id": "3699bd4e195b83d2",
  "timestamp_ms": 1783340616458,
  "duration_ms": 12,
  "method": "GET",
  "url": "https://api.example.com/users",
  "status_code": 200,
  "protocol_version": "HTTP/1.1",
  "request_headers": { "accept": "application/json" },
  "response_headers": { "content-type": "application/json", "content-encoding": "gzip" },
  "response_body": "{\"id\":1,\"name\":\"Alice\"}",
  "response_body_encoding": "utf-8",
  "response_body_truncated": false,
  "response_content_encoding": "gzip",
  "request_body_truncated": false,
  "source_addr": "127.0.0.1:54321",
  "dest_addr": "93.184.216.34:443"
}
```
