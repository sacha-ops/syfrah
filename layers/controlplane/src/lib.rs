//! Syfrah control plane — TiKV distributed KV store over the WireGuard mesh.
//!
//! Manages the PD (Placement Driver) and TiKV server processes:
//! - Auto-installs via TiUP
//! - Configures to listen on mesh IPv6 (encrypted by WireGuard)
//! - Runs as systemd services
//! - Bootstrap single-node, join existing cluster
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐   ┌─────────────┐   ┌─────────────┐
//! │   Node 1    │   │   Node 2    │   │   Node 3    │
//! │  PD + TiKV  │───│  PD + TiKV  │───│  PD + TiKV  │
//! │  (leader)   │   │ (follower)  │   │ (follower)  │
//! └──────┬──────┘   └──────┬──────┘   └──────┬──────┘
//!        │                 │                 │
//!        └────── WireGuard mesh (syfrah0) ───┘
//! ```
//!
//! Each node runs both PD and TiKV. PD handles Raft consensus and
//! scheduling, TiKV handles storage. All traffic flows over the
//! encrypted WireGuard mesh.

pub mod service;

/// Default ports (on mesh IPv6).
pub const PD_CLIENT_PORT: u16 = 2379;
pub const PD_PEER_PORT: u16 = 2380;
pub const TIKV_PORT: u16 = 20160;
