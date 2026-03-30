pub mod api;
pub mod backend;
pub mod error;
pub mod linux;
pub mod mock;
pub mod tap;
pub mod vxlan;

pub use api::OverlayHandler;
pub use backend::NetworkBackend;
pub use error::OverlayError;
pub use linux::LinuxBackend;
pub use mock::MockBackend;

#[cfg(test)]
mod bridge_tests;
