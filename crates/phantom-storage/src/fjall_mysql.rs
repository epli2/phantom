use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};
use phantom_core::error::StorageError;
use phantom_core::mysql::{MysqlStore, MysqlTrace};
use phantom_core::trace::SpanId;

pub struct FjallMysqlStore {
    keyspace: Keyspace,
    mysql_traces: PartitionHandle,
    mysql_by_time: PartitionHandle,
}

impl FjallMysqlStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        let keyspace = Config::new(path)
            .open()
            .map_err(|e| StorageError::Open(e.to_string()))?;

        let kv_sep_opts = PartitionCreateOptions::default()
            .with_kv_separation(fjall::KvSeparationOptions::default());

        let mysql_traces = keyspace
            .open_partition("mysql_traces", kv_sep_opts)
            .map_err(|e| StorageError::Open(e.to_string()))?;

        let mysql_by_time = keyspace
            .open_partition("mysql_by_time", PartitionCreateOptions::default())
            .map_err(|e| StorageError::Open(e.to_string()))?;

        Ok(Self {
            keyspace,
            mysql_traces,
            mysql_by_time,
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

/// Build the `mysql_by_time` key: `{timestamp_be (8B)}{span_id (8B)}`.
fn time_key(ts: &SystemTime, span_id: &SpanId) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&encode_timestamp(ts));
    key[8..].copy_from_slice(span_id.as_bytes());
    key
}

impl MysqlStore for FjallMysqlStore {
    fn insert(&self, trace: &MysqlTrace) -> Result<(), StorageError> {
        let serialized =
            serde_json::to_vec(trace).map_err(|e| StorageError::Serialization(e.to_string()))?;

        let span_key = trace.span_id.as_bytes();
        let time_k = time_key(&trace.timestamp, &trace.span_id);

        let mut batch = self.keyspace.batch();
        batch.insert(&self.mysql_traces, span_key, &serialized);
        batch.insert(&self.mysql_by_time, time_k, span_key);
        batch
            .commit()
            .map_err(|e| StorageError::Write(e.to_string()))?;

        Ok(())
    }

    fn get_by_span_id(&self, span_id: &SpanId) -> Result<Option<MysqlTrace>, StorageError> {
        let Some(value) = self
            .mysql_traces
            .get(span_id.as_bytes())
            .map_err(|e| StorageError::Read(e.to_string()))?
        else {
            return Ok(None);
        };
        let trace: MysqlTrace = serde_json::from_slice(&value)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        Ok(Some(trace))
    }

    fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<MysqlTrace>, StorageError> {
        let mut results = Vec::with_capacity(limit);
        for (i, entry) in self.mysql_by_time.iter().rev().enumerate() {
            if i < offset {
                continue;
            }
            if results.len() >= limit {
                break;
            }
            let (_key, value) = entry.map_err(|e| StorageError::Read(e.to_string()))?;
            let span_id_bytes: [u8; 8] = value[..8]
                .try_into()
                .map_err(|_| StorageError::Read("invalid span_id in mysql_by_time index".into()))?;
            if let Some(trace) = self.get_by_span_id(&SpanId(span_id_bytes))? {
                results.push(trace);
            }
        }
        Ok(results)
    }

    fn search_by_query(
        &self,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<MysqlTrace>, StorageError> {
        let mut results = Vec::new();
        for entry in self.mysql_by_time.iter().rev() {
            if results.len() >= limit {
                break;
            }
            let (_key, value) = entry.map_err(|e| StorageError::Read(e.to_string()))?;
            let span_id_bytes: [u8; 8] = value[..8]
                .try_into()
                .map_err(|_| StorageError::Read("invalid span_id in mysql_by_time index".into()))?;
            if let Some(trace) = self.get_by_span_id(&SpanId(span_id_bytes))?
                && trace.query.contains(pattern)
            {
                results.push(trace);
            }
        }
        Ok(results)
    }

    fn count(&self) -> Result<u64, StorageError> {
        Ok(self.mysql_traces.approximate_len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use phantom_core::mysql::{MysqlResponseKind, MysqlTrace};
    use phantom_core::trace::{SpanId, TraceId};

    fn make_trace(query: &str, ts_offset_ms: u64) -> MysqlTrace {
        MysqlTrace {
            span_id: SpanId(rand_bytes_8()),
            trace_id: TraceId(rand_bytes_16()),
            parent_span_id: None,
            query: query.to_string(),
            response: MysqlResponseKind::Ok {
                affected_rows: 1,
                last_insert_id: 0,
                warnings: 0,
            },
            timestamp: SystemTime::UNIX_EPOCH
                + Duration::from_secs(1_700_000_000)
                + Duration::from_millis(ts_offset_ms),
            duration: Duration::from_millis(4),
            dest_addr: Some("127.0.0.1:3306".to_string()),
            db_name: Some("testdb".to_string()),
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
    fn test_mysql_insert_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallMysqlStore::open(dir.path()).unwrap();

        let trace = make_trace("SELECT * FROM users", 0);
        let span_id = trace.span_id.clone();

        store.insert(&trace).unwrap();

        let retrieved = store.get_by_span_id(&span_id).unwrap().unwrap();
        assert_eq!(retrieved.query, "SELECT * FROM users");
    }

    #[test]
    fn test_mysql_list_recent_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallMysqlStore::open(dir.path()).unwrap();

        for i in 0..5u64 {
            store
                .insert(&make_trace(&format!("SELECT {i}"), i * 10))
                .unwrap();
        }

        let recent = store.list_recent(3, 0).unwrap();
        assert_eq!(recent.len(), 3);
        // Most recent first (highest timestamp offset = 40ms → "SELECT 4")
        assert!(
            recent[0].query.contains("SELECT 4"),
            "got: {}",
            recent[0].query
        );
    }

    #[test]
    fn test_mysql_search_by_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = FjallMysqlStore::open(dir.path()).unwrap();

        store.insert(&make_trace("SELECT * FROM users", 0)).unwrap();
        store
            .insert(&make_trace("INSERT INTO orders VALUES (1)", 10))
            .unwrap();
        store
            .insert(&make_trace("SELECT * FROM products", 20))
            .unwrap();

        let results = store.search_by_query("SELECT", 10).unwrap();
        assert_eq!(results.len(), 2);

        let results = store.search_by_query("INSERT", 10).unwrap();
        assert_eq!(results.len(), 1);
    }
}
