use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::trace::{SpanId, TraceId};

// ─────────────────────────────────────────────────────────────────────────────
// MySQL trace types
// ─────────────────────────────────────────────────────────────────────────────

/// The outcome of a MySQL COM_QUERY command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MysqlResponseKind {
    /// SELECT / SHOW / EXPLAIN — server returned a result set.
    ResultSet { column_count: u64, row_count: u64 },
    /// INSERT / UPDATE / DELETE / DDL — server returned an OK packet.
    Ok {
        affected_rows: u64,
        last_insert_id: u64,
        warnings: u16,
    },
    /// Server returned an ERR packet.
    Err {
        error_code: u16,
        sql_state: String,
        message: String,
    },
}

/// A complete MySQL COM_QUERY round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MysqlTrace {
    /// Unique ID for this trace span.
    pub span_id: SpanId,
    /// W3C Trace Context trace ID.
    pub trace_id: TraceId,
    /// Parent span ID (`None` if root span).
    pub parent_span_id: Option<SpanId>,

    // -- Query --
    pub query: String,

    // -- Response --
    pub response: MysqlResponseKind,

    // -- Timing --
    pub timestamp: SystemTime,
    pub duration: Duration,

    // -- Connection metadata --
    pub dest_addr: Option<String>,
    pub db_name: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// MysqlStore trait
// ─────────────────────────────────────────────────────────────────────────────

/// Persistent store for [`MysqlTrace`] records.
pub trait MysqlStore: Send + Sync {
    fn insert(&self, trace: &MysqlTrace) -> Result<(), StorageError>;
    fn get_by_span_id(&self, span_id: &SpanId) -> Result<Option<MysqlTrace>, StorageError>;
    fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<MysqlTrace>, StorageError>;
    fn search_by_query(&self, pattern: &str, limit: usize)
    -> Result<Vec<MysqlTrace>, StorageError>;
    fn count(&self) -> Result<u64, StorageError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::trace::{SpanId, TraceId};

    fn make_trace(response: MysqlResponseKind) -> MysqlTrace {
        MysqlTrace {
            span_id: SpanId([1u8; 8]),
            trace_id: TraceId([2u8; 16]),
            parent_span_id: None,
            query: "SELECT 1".to_string(),
            response,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            duration: Duration::from_millis(4),
            dest_addr: Some("127.0.0.1:3306".to_string()),
            db_name: Some("mydb".to_string()),
        }
    }

    #[test]
    fn test_mysql_response_kind_serde_result_set() {
        let kind = MysqlResponseKind::ResultSet {
            column_count: 3,
            row_count: 12,
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: MysqlResponseKind = serde_json::from_str(&json).unwrap();
        match back {
            MysqlResponseKind::ResultSet {
                column_count,
                row_count,
            } => {
                assert_eq!(column_count, 3);
                assert_eq!(row_count, 12);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_mysql_response_kind_serde_ok() {
        let kind = MysqlResponseKind::Ok {
            affected_rows: 1,
            last_insert_id: 42,
            warnings: 0,
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: MysqlResponseKind = serde_json::from_str(&json).unwrap();
        match back {
            MysqlResponseKind::Ok {
                affected_rows,
                last_insert_id,
                warnings,
            } => {
                assert_eq!(affected_rows, 1);
                assert_eq!(last_insert_id, 42);
                assert_eq!(warnings, 0);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_mysql_response_kind_serde_err() {
        let kind = MysqlResponseKind::Err {
            error_code: 1064,
            sql_state: "42000".to_string(),
            message: "syntax error".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: MysqlResponseKind = serde_json::from_str(&json).unwrap();
        match back {
            MysqlResponseKind::Err {
                error_code,
                sql_state,
                message,
            } => {
                assert_eq!(error_code, 1064);
                assert_eq!(sql_state, "42000");
                assert_eq!(message, "syntax error");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_mysql_trace_serde_roundtrip() {
        let trace = make_trace(MysqlResponseKind::Ok {
            affected_rows: 5,
            last_insert_id: 0,
            warnings: 0,
        });
        let json = serde_json::to_string(&trace).unwrap();
        let back: MysqlTrace = serde_json::from_str(&json).unwrap();
        assert_eq!(back.query, "SELECT 1");
        assert_eq!(back.span_id.0, [1u8; 8]);
        assert_eq!(back.trace_id.0, [2u8; 16]);
        assert_eq!(back.dest_addr.as_deref(), Some("127.0.0.1:3306"));
    }
}
