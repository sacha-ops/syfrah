use std::net::{Ipv4Addr, Ipv6Addr};

/// Trait abstracting Linux networking operations for testability.
///
/// All operations must be idempotent: creating an already-existing resource
/// is a no-op, deleting a missing resource succeeds silently.
#[async_trait::async_trait]
pub trait NetworkBackend: Send + Sync {
    // ── VXLAN ──────────────────────────────────────────────────────
    async fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), BackendError>;
    async fn delete_vxlan(&self, name: &str) -> Result<(), BackendError>;
    async fn add_fdb_entry(
        &self,
        bridge: &str,
        mac: [u8; 6],
        vtep: Ipv6Addr,
    ) -> Result<(), BackendError>;
    async fn remove_fdb_entry(&self, bridge: &str, mac: [u8; 6]) -> Result<(), BackendError>;
    async fn add_arp_proxy(
        &self,
        vxlan: &str,
        ip: Ipv4Addr,
        mac: [u8; 6],
    ) -> Result<(), BackendError>;

    // ── Bridge ─────────────────────────────────────────────────────
    async fn create_bridge(&self, name: &str) -> Result<(), BackendError>;
    async fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), BackendError>;
    async fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), BackendError>;
    async fn delete_bridge(&self, name: &str) -> Result<(), BackendError>;
    async fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), BackendError>;

    // ── TAP / veth ─────────────────────────────────────────────────
    async fn create_tap(&self, name: &str) -> Result<(), BackendError>;
    async fn delete_tap(&self, name: &str) -> Result<(), BackendError>;
    async fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<(), BackendError>;

    // ── Firewall ───────────────────────────────────────────────────
    async fn apply_vm_rules(
        &self,
        tap: &str,
        mac: [u8; 6],
        ip: Ipv4Addr,
    ) -> Result<(), BackendError>;
    async fn remove_vm_rules(&self, tap: &str) -> Result<(), BackendError>;
    async fn apply_nat(
        &self,
        bridge: &str,
        subnet: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), BackendError>;
    async fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str)
        -> Result<(), BackendError>;
}

/// Errors returned by backend operations.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("command failed: {cmd} — {stderr}")]
    CommandFailed { cmd: String, stderr: String },

    #[error("interface not found: {0}")]
    InterfaceNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
