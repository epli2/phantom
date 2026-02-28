use tokio::sync::mpsc;

use crate::error::CaptureError;
use crate::trace::HttpTrace;

/// Abstraction over different HTTP traffic capture backends.
///
/// On Linux, a bpftime-based eBPF backend can be used.
/// On all platforms, a local MITM proxy backend is available.
pub trait CaptureBackend: Send {
    /// Start capturing HTTP traffic.
    /// Returns a receiver that yields captured traces.
    fn start(&mut self) -> Result<mpsc::Receiver<HttpTrace>, CaptureError>;

    /// Gracefully stop capturing.
    fn stop(&mut self) -> Result<(), CaptureError>;

    /// Human-readable name of this backend (e.g., "proxy", "bpftime").
    fn name(&self) -> &str;
}
