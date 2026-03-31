use crate::error::Result;

/// Abstraction over Linux networking primitives (VXLAN, bridge, TAP, nftables).
///
/// All operations are idempotent. The real implementation shells out to
/// `ip`, `bridge`, and `nft`; the mock records calls for testing.
#[async_trait::async_trait]
pub trait NetworkBackend: Send + Sync {
    // ── VXLAN ──────────────────────────────────────────────────────────

    /// Create a VXLAN interface with the given VNI, bound to `local_ip` on `port`.
    async fn create_vxlan(&self, name: &str, vni: u32, local_ip: &str, port: u16) -> Result<()>;

    /// Delete a VXLAN interface.
    async fn delete_vxlan(&self, name: &str) -> Result<()>;

    /// Add a static FDB entry so the bridge knows which VTEP hosts a given MAC.
    async fn add_fdb_entry(&self, bridge: &str, mac: &str, vtep: &str) -> Result<()>;

    /// Remove a static FDB entry.
    async fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()>;

    /// Populate the ARP proxy table on a VXLAN interface.
    async fn add_arp_proxy(&self, vxlan: &str, ip: &str, mac: &str) -> Result<()>;

    /// Remove an ARP proxy entry from a VXLAN interface.
    async fn remove_arp_proxy(&self, vxlan: &str, ip: &str) -> Result<()>;

    // ── Bridge ─────────────────────────────────────────────────────────

    /// Create a Linux bridge.
    async fn create_bridge(&self, name: &str) -> Result<()>;

    /// Add a gateway IP to a bridge (e.g. subnet gateway).
    async fn add_bridge_ip(&self, bridge: &str, ip: &str, prefix_len: u8) -> Result<()>;

    /// Remove an IP from a bridge.
    async fn remove_bridge_ip(&self, bridge: &str, ip: &str) -> Result<()>;

    /// Delete a Linux bridge.
    async fn delete_bridge(&self, name: &str) -> Result<()>;

    /// Attach a network interface to a bridge.
    async fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()>;

    // ── TAP / veth ─────────────────────────────────────────────────────

    /// Create a TAP device (used by Cloud Hypervisor VMs).
    async fn create_tap(&self, name: &str) -> Result<()>;

    /// Delete a TAP device.
    async fn delete_tap(&self, name: &str) -> Result<()>;

    /// Create a veth pair (used by containers).
    async fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()>;

    /// Move a network interface into a container's network namespace.
    async fn move_to_netns(&self, iface: &str, pid: u32) -> Result<()>;

    /// Configure networking inside a container's network namespace.
    ///
    /// Renames `iface` to `eth0`, assigns the given IP/prefix, brings up
    /// `eth0` and `lo`, and adds a default route via `gateway`.
    async fn configure_netns(
        &self,
        pid: u32,
        iface: &str,
        ip: &str,
        prefix_len: u8,
        gateway: &str,
    ) -> Result<()>;

    // ── Firewall ───────────────────────────────────────────────────────

    /// Apply anti-spoofing + default ingress/egress rules for a VM.
    async fn apply_vm_rules(&self, tap: &str, mac: &str, ip: &str) -> Result<()>;

    /// Remove all firewall rules for a VM.
    async fn remove_vm_rules(&self, tap: &str) -> Result<()>;

    /// Enable SNAT/masquerade for a subnet behind a bridge.
    async fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()>;

    /// Remove NAT rules for a subnet.
    async fn remove_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()>;

    /// Allow forwarding between two peered VPC bridges.
    async fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()>;

    /// Remove peering forwarding rules.
    async fn remove_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()>;

    // ── Query / Discovery ──────────────────────────────────────────────

    /// List kernel network interfaces matching a given prefix.
    ///
    /// Used by the reconciliation loop and daemon restart recovery to
    /// discover existing `syfb-*`, `syfx-*`, `syft-*`, and
    /// `syfp*` interfaces.
    async fn list_interfaces(&self, prefix: &str) -> Result<Vec<String>>;
}
