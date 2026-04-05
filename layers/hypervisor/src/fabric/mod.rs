//! Fabric — WireGuard mesh networking.
//!
//! Manages the encrypted mesh between hypervisors:
//! - Mesh identity (secret, prefix, node address)
//! - WireGuard interface lifecycle
//! - Peer management (add, remove, health)
//! - Peering protocol (join requests, approval)
//! - State persistence

pub mod mesh;
pub mod ops;
pub mod peer;
pub mod peering;
pub mod service;
pub mod state;
pub mod wg;

pub use mesh::*;
pub use peer::*;
pub use peering::*;
pub use state::*;
