//! # syfrah-forge
//!
//! Per-node resource orchestrator for Syfrah.
//!
//! Forge is the single entry point for all resource mutations on a node.
//! It exposes an HTTP/JSON REST API on the fabric interface (`syfrah0`)
//! port 7100, reachable only from within the WireGuard mesh.
//!
//! ## Modules
//!
//! - [`api`] — HTTP server, request routing, health endpoint
//! - [`reconciler`] — reconciliation loop, drift detection (stub)
//! - [`capacity`] — resource tracking, admission control
//! - [`health`] — self-health, node-health checks
//! - [`runtime`] — delegates to compute (VmManager) and overlay (NetworkBackend)
//! - [`task`] — operation records, status tracking

pub mod api;
pub mod capacity;
pub mod health;
pub mod ownership;
pub mod reconciler;
pub mod runtime;
pub mod task;

pub use api::{ForgeHandler, ForgeServer};
