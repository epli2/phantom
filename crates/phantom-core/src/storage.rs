use crate::error::StorageError;
use crate::trace::{HttpTrace, SpanId, TraceId};

/// Abstraction over trace storage backends.
pub trait TraceStore: Send + Sync {
    /// Store a new trace.
    fn insert(&self, trace: &HttpTrace) -> Result<(), StorageError>;

    /// Retrieve a trace by span ID.
    fn get_by_span_id(&self, span_id: &SpanId) -> Result<Option<HttpTrace>, StorageError>;

    /// List recent traces (newest first), with pagination.
    fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<HttpTrace>, StorageError>;

    /// List all spans belonging to a given trace ID.
    fn get_by_trace_id(&self, trace_id: &TraceId) -> Result<Vec<HttpTrace>, StorageError>;

    /// Search traces by URL substring.
    fn search_by_url(
        &self,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<HttpTrace>, StorageError>;

    /// Get total trace count.
    fn count(&self) -> Result<u64, StorageError>;
}
