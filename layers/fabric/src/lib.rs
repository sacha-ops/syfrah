pub mod audit;
pub mod auth_middleware;
pub mod cli;
pub mod config;
pub mod control;
pub mod daemon;
pub mod events;
pub mod grpc_api;
pub mod http_api;
pub mod metrics;
pub mod migration;
pub mod peering;
pub mod raft_compute_handler;
pub mod raft_handler;
pub mod raft_hypervisor_handler;
pub mod sanitize;
pub mod sd_watchdog;
pub mod store;
pub mod topology;
pub mod ui;
pub mod vm_placement;
pub mod wg;

/// Canonical error returned when a command requires an existing mesh but none
/// is configured.  Every call-site should use this instead of hard-coding the
/// message so that the wording stays consistent across the entire CLI.
pub fn no_mesh_error() -> anyhow::Error {
    anyhow::anyhow!("No mesh configured. Run 'syfrah fabric init' or 'syfrah fabric join' first.")
}
