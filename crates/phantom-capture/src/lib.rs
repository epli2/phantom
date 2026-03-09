pub mod fault;
mod proxy;

#[cfg(target_os = "linux")]
mod ldpreload;

pub use fault::{parse_fault_spec, FaultConfig, FaultRule};
pub use proxy::ProxyCaptureBackend;

#[cfg(target_os = "linux")]
pub use ldpreload::LdPreloadCaptureBackend;
