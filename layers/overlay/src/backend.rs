use std::net::{Ipv4Addr, Ipv6Addr};

/// Result type for network backend operations.
pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// Errors returned by network backend operations.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("interface {0} not found")]
    InterfaceNotFound(String),

    #[error("interface {0} already exists")]
    InterfaceAlreadyExists(String),

    #[error("command failed: {0}")]
    CommandFailed(String),

    #[error("{0}")]
    Other(String),
}

/// Abstraction over Linux networking commands.
///
/// All operations are idempotent where specified. Real implementation
/// shells out to `ip`, `bridge`, and `nft`. Mock implementation records
/// calls for testing.
pub trait NetworkBackend: Send + Sync {
    // -- VXLAN --

    /// Create a VXLAN interface with the given VNI, local (underlay) IP, and UDP port.
    /// Flags: `nolearning` (static FDB only), `proxy` (ARP proxy mode).
    /// The interface is brought up automatically.
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> BackendResult<()>;

    /// Delete a VXLAN interface.
    fn delete_vxlan(&self, name: &str) -> BackendResult<()>;

    /// Check whether a network interface exists.
    fn interface_exists(&self, name: &str) -> BackendResult<bool>;

    /// Attach an interface to a bridge (`ip link set <iface> master <bridge>`).
    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> BackendResult<()>;

    // -- FDB --

    fn add_fdb_entry(&self, dev: &str, mac: &str, vtep: Ipv6Addr) -> BackendResult<()>;
    fn remove_fdb_entry(&self, dev: &str, mac: &str) -> BackendResult<()>;
    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: &str) -> BackendResult<()>;

    // -- Bridge --

    fn create_bridge(&self, name: &str) -> BackendResult<()>;
    fn add_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr, prefix_len: u8) -> BackendResult<()>;
    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> BackendResult<()>;
    fn delete_bridge(&self, name: &str) -> BackendResult<()>;

    // -- TAP / veth --

    fn create_tap(&self, name: &str) -> BackendResult<()>;
    fn delete_tap(&self, name: &str) -> BackendResult<()>;
    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> BackendResult<()>;

    // -- Firewall --

    fn apply_vm_rules(&self, tap: &str, mac: &str, ip: Ipv4Addr) -> BackendResult<()>;
    fn remove_vm_rules(&self, tap: &str) -> BackendResult<()>;
    fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> BackendResult<()>;
    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> BackendResult<()>;
}
