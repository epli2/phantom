mod proxy;

#[cfg(target_os = "linux")]
mod ldpreload;

pub use proxy::ProxyCaptureBackend;

#[cfg(target_os = "linux")]
pub use ldpreload::LdPreloadCaptureBackend;
