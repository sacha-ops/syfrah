//! Fabric — WireGuard mesh networking.
//!
//! Manages the encrypted mesh between hypervisors:
//! - Mesh identity (secret, prefix, node address)
//! - WireGuard interface lifecycle
//! - Peer management (add, remove, health)
//! - Peering protocol (join requests, approval)
//! - State persistence

pub mod mesh;
pub mod peer;
pub mod state;

pub use mesh::*;
pub use peer::*;
pub use state::*;
