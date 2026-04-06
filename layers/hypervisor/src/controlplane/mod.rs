//! Control plane — TiKV distributed KV store.
//!
//! Manages PD + TiKV as systemd services and exposes `ClusterDb`
//! for distributed state (VMs, VPCs, users, etc.).
//! All traffic flows over the encrypted WireGuard mesh.
//!
//! # Sub-modules
//!
//! - `service`: Low-level systemd management (install, start, stop, reload, status)
//! - `ops`: High-level orchestration (bootstrap, join, leave)
//! - `store`: `ClusterDb` — async TiKV client

pub mod ops;
pub mod service;
pub mod store;

/// Default ports (on mesh IPv6).
pub const PD_CLIENT_PORT: u16 = 2379;
pub const PD_PEER_PORT: u16 = 2380;
pub const TIKV_PORT: u16 = 20160;

pub use store::ClusterDb;
