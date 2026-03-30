//! NetworkBackend trait — abstraction over Linux networking commands.
//!
//! Production code calls real `ip`, `nft`, `bridge` commands.
//! Tests use `MockBackend` which records every call for assertion.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Result type for backend operations.
pub type Result<T> = std::result::Result<T, BackendError>;

/// Errors from network backend operations.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("command failed: {0}")]
    CommandFailed(String),

    #[error("interface not found: {0}")]
    InterfaceNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Abstraction over Linux networking primitives.
///
/// Every method is idempotent: calling it twice with the same arguments
/// must succeed without side effects.
pub trait NetworkBackend: Send + Sync {
    // ── VXLAN ───────────────────────────────────────────────────────
    fn create_vxlan(&self, name: &str, vni: u32, local_ip: Ipv6Addr, port: u16) -> Result<()>;
    fn delete_vxlan(&self, name: &str) -> Result<()>;
    fn add_fdb_entry(&self, bridge: &str, mac: &str, vtep: Ipv6Addr) -> Result<()>;
    fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()>;
    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: &str) -> Result<()>;

    // ── Bridge ──────────────────────────────────────────────────────
    fn create_bridge(&self, name: &str) -> Result<()>;
    fn add_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr, prefix_len: u8) -> Result<()>;
    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<()>;
    fn delete_bridge(&self, name: &str) -> Result<()>;
    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()>;

    // ── TAP / veth ──────────────────────────────────────────────────
    fn create_tap(&self, name: &str) -> Result<()>;
    fn delete_tap(&self, name: &str) -> Result<()>;
    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()>;

    // ── Firewall (nftables) ─────────────────────────────────────────
    fn apply_vm_rules(&self, tap: &str, mac: &str, ip: Ipv4Addr) -> Result<()>;
    fn remove_vm_rules(&self, tap: &str) -> Result<()>;
    fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()>;
    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()>;
}
