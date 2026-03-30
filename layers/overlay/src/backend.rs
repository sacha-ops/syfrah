//! NetworkBackend trait — abstraction over Linux networking primitives.
//!
//! All overlay networking operations go through this trait. The real
//! implementation shells out to `ip`, `bridge`, and `nft` commands.
//! The mock implementation records calls for testing.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::Ipv4Net;

use crate::error::OverlayError;

/// A MAC address represented as 6 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// Parse a MAC address from colon-separated hex string (e.g., "02:00:0a:00:01:05").
    pub fn parse(s: &str) -> Result<Self, OverlayError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(OverlayError::InvalidMac(s.to_string()));
        }
        let mut bytes = [0u8; 6];
        for (i, part) in parts.iter().enumerate() {
            bytes[i] = u8::from_str_radix(part, 16)
                .map_err(|_| OverlayError::InvalidMac(s.to_string()))?;
        }
        Ok(MacAddr(bytes))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

impl serde::Serialize for MacAddr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for MacAddr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        MacAddr::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Abstraction over Linux networking primitives.
///
/// Every method is synchronous and returns `Result<()>`. The real
/// implementation runs shell commands; the mock records calls.
pub trait NetworkBackend: Send + Sync {
    // ── VXLAN ───────────────────────────────────────────────────────
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), OverlayError>;
    fn delete_vxlan(&self, name: &str) -> Result<(), OverlayError>;

    // ── FDB ─────────────────────────────────────────────────────────
    fn add_fdb_entry(&self, bridge: &str, mac: MacAddr, vtep: Ipv6Addr)
        -> Result<(), OverlayError>;
    fn remove_fdb_entry(&self, bridge: &str, mac: MacAddr) -> Result<(), OverlayError>;

    // ── ARP proxy ───────────────────────────────────────────────────
    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: MacAddr) -> Result<(), OverlayError>;
    fn remove_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr) -> Result<(), OverlayError>;

    // ── Bridge ──────────────────────────────────────────────────────
    fn create_bridge(&self, name: &str) -> Result<(), OverlayError>;
    fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), OverlayError>;
    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), OverlayError>;
    fn delete_bridge(&self, name: &str) -> Result<(), OverlayError>;
    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), OverlayError>;

    // ── TAP/veth ────────────────────────────────────────────────────
    fn create_tap(&self, name: &str) -> Result<(), OverlayError>;
    fn delete_tap(&self, name: &str) -> Result<(), OverlayError>;
    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<(), OverlayError>;

    // ── Firewall ────────────────────────────────────────────────────
    fn apply_vm_rules(&self, tap: &str, mac: MacAddr, ip: Ipv4Addr) -> Result<(), OverlayError>;
    fn remove_vm_rules(&self, tap: &str) -> Result<(), OverlayError>;
    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<(), OverlayError>;
    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<(), OverlayError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_addr_parse_valid() {
        let mac = MacAddr::parse("02:00:0a:00:01:05").unwrap();
        assert_eq!(mac.0, [0x02, 0x00, 0x0a, 0x00, 0x01, 0x05]);
    }

    #[test]
    fn mac_addr_display() {
        let mac = MacAddr([0x02, 0x00, 0x0a, 0x00, 0x01, 0x05]);
        assert_eq!(mac.to_string(), "02:00:0a:00:01:05");
    }

    #[test]
    fn mac_addr_roundtrip() {
        let original = "02:00:c0:a8:ff:01";
        let mac = MacAddr::parse(original).unwrap();
        assert_eq!(mac.to_string(), original);
    }

    #[test]
    fn mac_addr_parse_invalid() {
        assert!(MacAddr::parse("not-a-mac").is_err());
        assert!(MacAddr::parse("02:00:0a:00:01").is_err()); // too short
        assert!(MacAddr::parse("02:00:0a:00:01:05:ff").is_err()); // too long
        assert!(MacAddr::parse("gg:00:0a:00:01:05").is_err()); // invalid hex
    }
}
