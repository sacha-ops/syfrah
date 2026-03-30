use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::Ipv4Net;

/// MAC address represented as 6 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

/// All networking primitives needed by the overlay layer.
///
/// Implemented by `LinuxBackend` for production and `MockNetworkBackend` for
/// unit tests. Every method is idempotent — calling it twice with the same
/// arguments must succeed without error.
pub trait NetworkBackend: Send + Sync {
    // ── VXLAN ───────────────────────────────────────────────────────────
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), BackendError>;

    fn delete_vxlan(&self, name: &str) -> Result<(), BackendError>;

    fn add_fdb_entry(&self, bridge: &str, mac: MacAddr, vtep: Ipv6Addr)
        -> Result<(), BackendError>;

    fn remove_fdb_entry(&self, bridge: &str, mac: MacAddr) -> Result<(), BackendError>;

    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: MacAddr) -> Result<(), BackendError>;

    // ── Bridge ──────────────────────────────────────────────────────────
    fn create_bridge(&self, name: &str) -> Result<(), BackendError>;

    fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), BackendError>;

    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), BackendError>;

    fn delete_bridge(&self, name: &str) -> Result<(), BackendError>;

    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), BackendError>;

    // ── TAP / veth ──────────────────────────────────────────────────────
    fn create_tap(&self, name: &str) -> Result<(), BackendError>;

    fn delete_tap(&self, name: &str) -> Result<(), BackendError>;

    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<(), BackendError>;

    // ── Firewall (nftables) ─────────────────────────────────────────────
    fn apply_vm_rules(&self, tap: &str, mac: MacAddr, ip: Ipv4Addr) -> Result<(), BackendError>;

    fn remove_vm_rules(&self, tap: &str) -> Result<(), BackendError>;

    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<(), BackendError>;

    /// Allow FORWARD between two peered VPC bridges (both directions).
    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<(), BackendError>;

    /// Remove FORWARD rules between two previously-peered VPC bridges.
    fn remove_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<(), BackendError>;
}

/// Errors produced by backend operations.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("command failed: {0}")]
    CommandFailed(String),
    #[error("interface not found: {0}")]
    InterfaceNotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
