use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};
use phantom_core::error::StorageError;
use phantom_core::query::TraceQuery;
use phantom_core::storage::TraceStore;
use phantom_core::trace::{HttpTrace, SpanId, TraceId};

pub struct FjallTraceStore {
    keyspace: Keyspace,
    traces: PartitionHandle,
    by_time: PartitionHandle,
    by_trace_id: PartitionHandle,
    /// Advisory exclusive lock on the data directory, released on drop.
    /// fjall itself does not lock across processes, and two writers on one
    /// keyspace would corrupt it — so we enforce single-process access here.
    _lock: std::fs::File,
}

impl FjallTraceStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        std::fs::create_dir_all(path).map_err(|e| StorageError::Open(e.to_string()))?;
        let lock_path = path.join("phantom.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| StorageError::Open(format!("{}: {e}", lock_path.display())))?;
        lock.try_lock().map_err(|_| {
            StorageError::Open(format!(
                "data dir {} holds an active store lock (another phantom process is using it)",
                path.display()
            ))
        })?;

        let keyspace = Config::new(path)
            .open()
            .map_err(|e| StorageError::Open(e.to_string()))?;

        let kv_sep_opts = PartitionCreateOptions::default()
            .with_kv_separation(fjall::KvSeparationOptions::default());

        let traces = keyspace
            .open_partition("traces", kv_sep_opts)
            .map_err(|e| StorageError::Open(e.to_string()))?;

        let by_time = keyspace
            .open_partition("by_time", PartitionCreateOptions::default())
            .map_err(|e| StorageError::Open(e.to_string()))?;

        let by_trace_id = keyspace
            .open_partition("by_trace_id", PartitionCreateOptions::default())
            .map_err(|e| StorageError::Open(e.to_string()))?;

        Ok(Self {
            keyspace,
            traces,
            by_time,
            by_trace_id,
            _lock: lock,
        })
    }
}

/// Encode a `SystemTime` as big-endian nanoseconds since UNIX epoch.
fn encode_timestamp(ts: &SystemTime) -> [u8; 8] {
    let nanos = ts
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos() as u64;
    nanos.to_be_bytes()
}

/// Build the `by_time` key: `{timestamp_be (8B)}{span_id (8B)}`.
fn time_key(ts: &SystemTime, span_id: &SpanId) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&encode_timestamp(ts));
    key[8..].copy_from_slice(span_id.as_bytes());
    key
}

/// Build the `by_trace_id` key: `{trace_id (16B)}{span_id (8B)}`.
fn trace_id_key(trace_id: &TraceId, span_id: &SpanId) -> [u8; 24] {
    let mut key = [0u8; 24];
    key[..16].copy_from_slice(trace_id.as_bytes());
    key[16..].copy_from_slice(span_id.as_bytes());
    key
}

impl TraceStore for FjallTraceStore {
    fn insert(&self, trace: &HttpTrace) -> Result<(), StorageError> {
        let serialized =
            serde_json::to_vec(trace).map_err(|e| StorageError::Serialization(e.to_string()))?;

        let span_key = trace.span_id.as_bytes();
        let time_k = time_key(&trace.timestamp, &trace.span_id);
        let trace_id_k = trace_id_key(&trace.trace_id, &trace.span_id);

        let mut batch = self.keyspace.batch();
        batch.insert(&self.traces, span_key, &serialized);
        batch.insert(&self.by_time, time_k, span_key);
        batch.insert(&self.by_trace_id, trace_id_k, span_key);
        batch
            .commit()
            .map_err(|e| StorageError::Write(e.to_string()))?;

        Ok(())
    }

    fn get_by_span_id(&self, span_id: &SpanId) -> Result<Option<HttpTrace>, StorageError> {
        let Some(value) = self
            .traces
            .get(span_id.as_bytes())
            .map_err(|e| StorageError::Read(e.to_string()))?
        else {
            return Ok(None);
        };
        let trace: HttpTrace = serde_json::from_slice(&value)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        Ok(Some(trace))
    }

    fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<HttpTrace>, StorageError> {
        let mut results = Vec::with_capacity(limit);
        for (i, entry) in self.by_time.iter().rev().enumerate() {
            if i < offset {
                continue;
            }
            if results.len() >= limit {
                break;
            }
            let (_key, value) = entry.map_err(|e| StorageError::Read(e.to_string()))?;
            let span_id_bytes: [u8; 8] = value[..8]
                .try_into()
                .map_err(|_| StorageError::Read("invalid span_id in index".into()))?;
            if let Some(trace) = self.get_by_span_id(&SpanId(span_id_bytes))? {
                results.push(trace);
            }
        }
        Ok(results)
    }

    fn get_by_trace_id(&self, trace_id: &TraceId) -> Result<Vec<HttpTrace>, StorageError> {
        let mut results = Vec::new();
        for entry in self.by_trace_id.prefix(trace_id.as_bytes()) {
            let (_key, value) = entry.map_err(|e| StorageError::Read(e.to_string()))?;
            let span_id_bytes: [u8; 8] = value[..8]
                .try_into()
                .map_err(|_| StorageError::Read("invalid span_id in index".into()))?;
            if let Some(trace) = self.get_by_span_id(&SpanId(span_id_bytes))? {
                results.push(trace);
            }
        }
        Ok(results)
    }

    fn search_by_url(&self, pattern: &str, limit: usize) -> Result<Vec<HttpTrace>, StorageError> {
        let mut results = Vec::new();
        // Full scan with URL substring match (MVP approach)
        for entry in self.by_time.iter().rev() {
            if results.len() >= limit {
                break;
            }
            let (_key, value) = entry.map_err(|e| StorageError::Read(e.to_string()))?;
            let span_id_bytes: [u8; 8] = value[..8]
                .try_into()
                .map_err(|_| StorageError::Read("invalid span_id in index".into()))?;
            if let Some(trace) = self.get_by_span_id(&SpanId(span_id_bytes))?
                && trace.url.contains(pattern)
            {
                results.push(trace);
            }
        }
        Ok(results)
    }

    fn count(&self) -> Result<u64, StorageError> {
        Ok(self.traces.approximate_len() as u64)
    }

    fn query(&self, query: &TraceQuery) -> Result<Vec<HttpTrace>, StorageError> {
        const DEFAULT_LIMIT: usize = 100;
        let limit = if query.limit == 0 {
            DEFAULT_LIMIT
        } else {
            query.limit
        };

        // With a trace_id filter, the by_trace_id prefix scan is far narrower
        // than a time scan; otherwise scan by_time (newest first), bounded by
        // the since/until key range when given.
        let index_entries: Box<dyn Iterator<Item = fjall::Result<(fjall::Slice, fjall::Slice)>>> =
            if let Some(trace_id) = &query.trace_id {
                Box::new(self.by_trace_id.prefix(trace_id.as_bytes()))
            } else {
                let start: [u8; 16] = query
                    .since
                    .map(|ts| time_key(&ts, &SpanId([0x00; 8])))
                    .unwrap_or([0x00; 16]);
                let end: [u8; 16] = query
                    .until
                    .map(|ts| time_key(&ts, &SpanId([0xff; 8])))
                    .unwrap_or([0xff; 16]);
                Box::new(self.by_time.range(start..=end).rev())
            };

        let mut skipped = 0;
        let mut results = Vec::new();
        for entry in index_entries {
            if results.len() >= limit {
                break;
            }
            let (_key, value) = entry.map_err(|e| StorageError::Read(e.to_string()))?;
            let span_id_bytes: [u8; 8] = value[..8]
                .try_into()
                .map_err(|_| StorageError::Read("invalid span_id in index".into()))?;
            if let Some(trace) = self.get_by_span_id(&SpanId(span_id_bytes))?
                && query.matches(&trace)
            {
                if skipped < query.offset {
                    skipped += 1;
                    continue;
                }
                results.push(trace);
            }
        }
        Ok(results)
    }

    fn clear(&self) -> Result<(), StorageError> {
        for partition in [&self.traces, &self.by_time, &self.by_trace_id] {
            let keys: Vec<_> = partition
                .keys()
                .collect::<Result<_, _>>()
                .map_err(|e| StorageError::Read(e.to_string()))?;
            let mut batch = self.keyspace.batch();
            for key in keys {
                batch.remove(partition, key);
            }
            batch
                .commit()
                .map_err(|e| StorageError::Write(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn make_trace(url: &str, status: u16) -> HttpTrace {
        HttpTrace {
            span_id: SpanId(rand_bytes_8()),
            trace_id: TraceId(rand_bytes_16()),
            parent_span_id: None,
            method: phantom_core::trace::HttpMethod::Get,
            url: url.to_string(),
            request_headers: HashMap::new(),
            request_body: None,
            status_code: status,
            response_headers: HashMap::new(),
            response_body: None,
            timestamp: SystemTime::now(),
            duration: Duration::from_millis(42),
            source_addr: None,
            dest_addr: None,
            protocol_version: "HTTP/1.1".to_string(),
        }
    }

    fn rand_bytes_8() -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf.iter_mut().for_each(|b| *b = rand::random());
        buf
    }

    fn rand_bytes_16() -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf.iter_mut().for_each(|b| *b = rand::random());
        buf
    }

    #[test]
    fn test_insert_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        let trace = make_trace("http://example.com/api/users", 200);
        let span_id = trace.span_id.clone();

        store.insert(&trace).unwrap();

        let retrieved = store.get_by_span_id(&span_id).unwrap().unwrap();
        assert_eq!(retrieved.url, "http://example.com/api/users");
        assert_eq!(retrieved.status_code, 200);
    }

    #[test]
    fn test_list_recent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        for i in 0..5 {
            let mut trace = make_trace(&format!("http://example.com/api/{i}"), 200);
            trace.timestamp = SystemTime::now() + Duration::from_millis(i as u64 * 10);
            store.insert(&trace).unwrap();
        }

        let recent = store.list_recent(3, 0).unwrap();
        assert_eq!(recent.len(), 3);
        // Most recent first
        assert!(recent[0].url.contains("/api/4"));
    }

    #[test]
    fn test_get_by_trace_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        let shared_trace_id = TraceId(rand_bytes_16());
        for i in 0..3 {
            let mut trace = make_trace(&format!("http://example.com/api/{i}"), 200);
            trace.trace_id = shared_trace_id.clone();
            store.insert(&trace).unwrap();
        }
        // Insert one with a different trace_id
        store.insert(&make_trace("http://other.com", 404)).unwrap();

        let grouped = store.get_by_trace_id(&shared_trace_id).unwrap();
        assert_eq!(grouped.len(), 3);
    }

    #[test]
    fn test_search_by_url() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        store
            .insert(&make_trace("http://example.com/api/users", 200))
            .unwrap();
        store
            .insert(&make_trace("http://example.com/api/orders", 201))
            .unwrap();
        store
            .insert(&make_trace("http://example.com/health", 200))
            .unwrap();

        let results = store.search_by_url("/api/", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    use phantom_core::trace::HttpMethod;

    fn make_trace_at(url: &str, status: u16, ts_secs: u64) -> HttpTrace {
        let mut t = make_trace(url, status);
        t.timestamp = std::time::UNIX_EPOCH + Duration::from_secs(ts_secs);
        t
    }

    #[test]
    fn test_query_by_method() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        let mut post = make_trace("http://a/create", 201);
        post.method = HttpMethod::Post;
        store.insert(&post).unwrap();
        store.insert(&make_trace("http://a/read", 200)).unwrap();

        let results = store
            .query(&TraceQuery {
                methods: vec![HttpMethod::Post],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "http://a/create");
    }

    #[test]
    fn test_query_by_status_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        store.insert(&make_trace("http://a/ok", 200)).unwrap();
        store.insert(&make_trace("http://a/missing", 404)).unwrap();
        store.insert(&make_trace("http://a/boom", 500)).unwrap();

        let results = store
            .query(&TraceQuery {
                status: Some("4xx".parse().unwrap()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status_code, 404);
    }

    #[test]
    fn test_query_time_range_bounds_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        for ts in [100, 200, 300, 400] {
            store
                .insert(&make_trace_at(&format!("http://a/{ts}"), 200, ts))
                .unwrap();
        }

        // Inclusive on both ends; newest first.
        let results = store
            .query(&TraceQuery {
                since: Some(std::time::UNIX_EPOCH + Duration::from_secs(200)),
                until: Some(std::time::UNIX_EPOCH + Duration::from_secs(300)),
                ..Default::default()
            })
            .unwrap();
        let urls: Vec<_> = results.iter().map(|t| t.url.as_str()).collect();
        assert_eq!(urls, ["http://a/300", "http://a/200"]);
    }

    #[test]
    fn test_query_offset_applied_after_filter() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        // Interleave matching (404) and non-matching (200) traces.
        for i in 0..6u64 {
            let status = if i % 2 == 0 { 404 } else { 200 };
            store
                .insert(&make_trace_at(&format!("http://a/{i}"), status, 100 + i))
                .unwrap();
        }

        // Matching, newest first: /4, /2, /0. Offset 1 + limit 1 → /2.
        let results = store
            .query(&TraceQuery {
                status: Some("404".parse().unwrap()),
                offset: 1,
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "http://a/2");
    }

    #[test]
    fn test_query_by_trace_id_with_filters() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        let shared = TraceId(rand_bytes_16());
        for status in [200, 404] {
            let mut t = make_trace(&format!("http://a/{status}"), status);
            t.trace_id = shared.clone();
            store.insert(&t).unwrap();
        }
        store.insert(&make_trace("http://other/404", 404)).unwrap();

        let results = store
            .query(&TraceQuery {
                trace_id: Some(shared),
                status: Some("404".parse().unwrap()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "http://a/404");
    }

    #[test]
    fn test_open_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        let second = FjallTraceStore::open(dir.path());
        assert!(matches!(second, Err(StorageError::Open(_))));

        // Lock is released when the store is dropped.
        drop(store);
        assert!(FjallTraceStore::open(dir.path()).is_ok());
    }

    #[test]
    fn test_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallTraceStore::open(dir.path()).unwrap();

        let trace = make_trace("http://a/x", 200);
        let span_id = trace.span_id.clone();
        store.insert(&trace).unwrap();
        store.insert(&make_trace("http://a/y", 200)).unwrap();

        store.clear().unwrap();

        assert!(store.get_by_span_id(&span_id).unwrap().is_none());
        assert!(store.list_recent(10, 0).unwrap().is_empty());
        assert!(store.query(&TraceQuery::default()).unwrap().is_empty());
    }
}
