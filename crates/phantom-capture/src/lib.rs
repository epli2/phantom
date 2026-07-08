pub mod fault;
mod proxy;

#[cfg(target_os = "linux")]
mod ldpreload;

pub use fault::{FaultConfig, FaultRule, parse_fault_spec};
pub use proxy::{CaPaths, ProxyCaptureBackend, ensure_ca};

#[cfg(target_os = "linux")]
pub use ldpreload::LdPreloadCaptureBackend;
