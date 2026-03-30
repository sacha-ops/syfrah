pub mod api;
pub mod backend;
pub mod error;
pub mod fdb;
pub mod linux;
pub mod mock;
pub mod nft;
pub mod tap;
pub mod vxlan;

pub use api::OverlayHandler;
pub use backend::NetworkBackend;
pub use error::OverlayError;
pub use fdb::{add_arp_proxy, add_fdb_entry, register_remote_vm, remove_fdb_entry};
pub use linux::LinuxBackend;
pub use mock::MockBackend;

#[cfg(test)]
mod bridge_tests;
