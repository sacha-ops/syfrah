//! Deterministic interface naming that respects the Linux 15-character limit.
//!
//! Linux network interfaces (bridges, VXLANs, TAPs, veths) must have names
//! no longer than 15 characters (`IFNAMSIZ - 1`). VPC/VM identifiers can be
//! arbitrarily long, so we hash the input to a fixed 8-hex-digit suffix.
//!
//! Naming conventions:
//! - Bridge:         `syfb-{hash}`   (13 chars)
//! - VXLAN:          `syfx-{hash}`   (13 chars)
//! - TAP:            `syft-{hash}`   (13 chars)
//! - veth host:      `syfvh{hash}`   (13 chars)
//! - veth container: `syfvc{hash}`   (13 chars)
//! - peer veth A:    `syfpa{hash}`   (13 chars)
//! - peer veth B:    `syfpb{hash}`   (13 chars)

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Linux interface name length limit (`IFNAMSIZ - 1`).
pub const IFNAMSIZ_MAX: usize = 15;

fn short_hash(input: &str) -> String {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xFFFF_FFFF)
}

/// Bridge interface name for a VPC: `syfb-{hash}`.
pub fn bridge_name(vpc_id: &str) -> String {
    format!("syfb-{}", short_hash(vpc_id))
}

/// VXLAN interface name for a VPC: `syfx-{hash}`.
pub fn vxlan_name(vpc_id: &str) -> String {
    format!("syfx-{}", short_hash(vpc_id))
}

/// TAP interface name for a VM: `syft-{hash}`.
pub fn tap_name(vm_id: &str) -> String {
    format!("syft-{}", short_hash(vm_id))
}

/// Veth host-side name for a container: `syfvh{hash}`.
pub fn veth_host_name(vm_id: &str) -> String {
    format!("syfvh{}", short_hash(vm_id))
}

/// Veth container-side name: `syfvc{hash}`.
pub fn veth_container_name(vm_id: &str) -> String {
    format!("syfvc{}", short_hash(vm_id))
}

/// Peering veth end A: `syfpa{hash}`.
pub fn peer_name_a(peering_id: &str) -> String {
    format!("syfpa{}", short_hash(peering_id))
}

/// Peering veth end B: `syfpb{hash}`.
pub fn peer_name_b(peering_id: &str) -> String {
    format!("syfpb{}", short_hash(peering_id))
}

/// Prefix used for bridge interface names (for `list_interfaces`).
pub const BRIDGE_PREFIX: &str = "syfb-";

/// Prefix used for VXLAN interface names.
pub const VXLAN_PREFIX: &str = "syfx-";

/// Prefix used for TAP interface names.
pub const TAP_PREFIX: &str = "syft-";

/// Prefix used for peering veth interfaces.
pub const PEER_PREFIX: &str = "syfp";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_name_within_limit() {
        let name = bridge_name("acme-backend-default-very-long-vpc-name");
        assert!(
            name.len() <= IFNAMSIZ_MAX,
            "bridge name '{}' is {} chars, exceeds {}",
            name,
            name.len(),
            IFNAMSIZ_MAX
        );
        assert!(name.starts_with(BRIDGE_PREFIX));
    }

    #[test]
    fn vxlan_name_within_limit() {
        let name = vxlan_name("acme-backend-default-very-long-vpc-name");
        assert!(
            name.len() <= IFNAMSIZ_MAX,
            "vxlan name '{}' is {} chars, exceeds {}",
            name,
            name.len(),
            IFNAMSIZ_MAX
        );
        assert!(name.starts_with(VXLAN_PREFIX));
    }

    #[test]
    fn tap_name_within_limit() {
        let name = tap_name("my-super-long-vm-identifier-12345");
        assert!(
            name.len() <= IFNAMSIZ_MAX,
            "tap name '{}' is {} chars, exceeds {}",
            name,
            name.len(),
            IFNAMSIZ_MAX
        );
        assert!(name.starts_with(TAP_PREFIX));
    }

    #[test]
    fn veth_host_name_within_limit() {
        let name = veth_host_name("my-super-long-vm-identifier-12345");
        assert!(
            name.len() <= IFNAMSIZ_MAX,
            "veth host name '{}' is {} chars, exceeds {}",
            name,
            name.len(),
            IFNAMSIZ_MAX
        );
    }

    #[test]
    fn veth_container_name_within_limit() {
        let name = veth_container_name("my-super-long-vm-identifier-12345");
        assert!(
            name.len() <= IFNAMSIZ_MAX,
            "veth container name '{}' is {} chars, exceeds {}",
            name,
            name.len(),
            IFNAMSIZ_MAX
        );
    }

    #[test]
    fn peer_names_within_limit() {
        let a = peer_name_a("very-long-peering-identifier-abc-123");
        let b = peer_name_b("very-long-peering-identifier-abc-123");
        assert!(
            a.len() <= IFNAMSIZ_MAX,
            "peer_a name '{}' is {} chars, exceeds {}",
            a,
            a.len(),
            IFNAMSIZ_MAX
        );
        assert!(
            b.len() <= IFNAMSIZ_MAX,
            "peer_b name '{}' is {} chars, exceeds {}",
            b,
            b.len(),
            IFNAMSIZ_MAX
        );
    }

    #[test]
    fn deterministic_hashing() {
        // Same input always produces the same output.
        assert_eq!(bridge_name("vpc-100"), bridge_name("vpc-100"));
        assert_eq!(tap_name("vm-1"), tap_name("vm-1"));
    }

    #[test]
    fn different_inputs_different_hashes() {
        assert_ne!(bridge_name("vpc-100"), bridge_name("vpc-200"));
        assert_ne!(tap_name("vm-1"), tap_name("vm-2"));
    }

    #[test]
    fn all_names_at_most_13_chars() {
        // With a 4-char prefix + dash + 8 hex chars = 13, or 5-char prefix + 8 = 13.
        let inputs = [
            "a",
            "short",
            "medium-length-id",
            "a-very-long-identifier-that-would-blow-up-the-old-format",
        ];
        for input in &inputs {
            for name in [
                bridge_name(input),
                vxlan_name(input),
                tap_name(input),
                veth_host_name(input),
                veth_container_name(input),
                peer_name_a(input),
                peer_name_b(input),
            ] {
                assert!(
                    name.len() <= 13,
                    "name '{}' for input '{}' is {} chars",
                    name,
                    input,
                    name.len()
                );
            }
        }
    }
}
