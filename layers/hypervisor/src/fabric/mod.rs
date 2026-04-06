//! Fabric — mesh networking with pluggable backends.
//!
//! Manages connectivity between hypervisors:
//! - **WireGuard** (default): encrypted tunnel over public internet
//! - **Direct**: private network, no tunnel
//! - **Mock**: testing, no-op
//!
//! The backend is selected at `hypervisor init` and persisted in state.

pub mod announce;
pub mod backend;
pub mod direct;
pub mod health;
pub mod mesh;
pub mod mock;
pub mod ops;
pub mod peer;
pub mod peering;
pub mod peering_client;
pub mod peering_server;
pub mod service;
pub mod state;
#[doc(hidden)]
pub mod tls;
pub mod wg;
pub mod wireguard;

pub use backend::{NetworkMode, create_backend};
pub use mesh::*;
pub use peer::*;
pub use peering::*;
pub use state::*;
