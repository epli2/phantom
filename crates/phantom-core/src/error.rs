use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("failed to start capture: {0}")]
    StartFailed(String),
    #[error("failed to stop capture: {0}")]
    StopFailed(String),
    #[error("capture error: {0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("failed to open storage: {0}")]
    Open(String),
    #[error("failed to write: {0}")]
    Write(String),
    #[error("failed to read: {0}")]
    Read(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}
