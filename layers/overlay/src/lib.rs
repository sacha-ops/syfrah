pub mod api;
pub mod backend;
pub mod linux;
pub mod mock;

pub use api::OverlayHandler;
pub use backend::{BackendError, NetworkBackend};
pub use linux::LinuxBackend;
pub use mock::MockBackend;

#[cfg(test)]
mod bridge_tests;
