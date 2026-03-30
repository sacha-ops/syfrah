use std::net::{Ipv4Addr, Ipv6Addr};

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

/// IPv4 network (address + prefix length).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv4Net {
    pub addr: Ipv4Addr,
    pub prefix_len: u8,
}

impl std::fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix_len)
    }
}

/// Abstraction over Linux networking operations.
///
/// All methods are idempotent. A real implementation calls `ip`, `bridge`,
/// and `nft` commands; the mock records calls for unit testing.
pub trait NetworkBackend: Send + Sync {
    // -- VXLAN --
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn delete_vxlan(&self, name: &str) -> Result<(), Box<dyn std::error::Error>>;

    fn add_fdb_entry(
        &self,
        bridge: &str,
        mac: MacAddr,
        vtep: Ipv6Addr,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn remove_fdb_entry(
        &self,
        bridge: &str,
        mac: MacAddr,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn add_arp_proxy(
        &self,
        vxlan: &str,
        ip: Ipv4Addr,
        mac: MacAddr,
    ) -> Result<(), Box<dyn std::error::Error>>;

    // -- Bridge --
    fn create_bridge(&self, name: &str) -> Result<(), Box<dyn std::error::Error>>;

    fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn remove_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn delete_bridge(&self, name: &str) -> Result<(), Box<dyn std::error::Error>>;

    fn attach_to_bridge(
        &self,
        interface: &str,
        bridge: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;

    // -- TAP / veth --
    fn create_tap(&self, name: &str) -> Result<(), Box<dyn std::error::Error>>;

    fn delete_tap(&self, name: &str) -> Result<(), Box<dyn std::error::Error>>;

    fn create_veth_pair(
        &self,
        name_a: &str,
        name_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;

    // -- Firewall --
    fn apply_vm_rules(
        &self,
        tap: &str,
        mac: MacAddr,
        ip: Ipv4Addr,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn remove_vm_rules(&self, tap: &str) -> Result<(), Box<dyn std::error::Error>>;

    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<(), Box<dyn std::error::Error>>;

    fn apply_peering_rules(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;

    // -- Isolation --
    fn apply_subnet_isolation(
        &self,
        bridge: &str,
        subnet_a: Ipv4Net,
        subnet_b: Ipv4Net,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn remove_subnet_isolation(
        &self,
        bridge: &str,
        subnet_a: Ipv4Net,
        subnet_b: Ipv4Net,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn apply_vpc_isolation(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn remove_vpc_isolation(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
}
