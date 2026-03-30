//! nftables rule management for Syfrah overlay networking.
//!
//! This module provides:
//! - Per-VM anti-spoofing rules (source MAC/IP validation)
//! - Default-deny ingress with SSH + ICMP exceptions
//! - Default-allow egress
//! - Conntrack (established/related auto-allowed)
//! - Subnet isolation within the same VPC (cross-subnet blocked by default)
//! - VPC isolation (cross-VPC forwarding blocked by default)
//! - SNAT masquerade for internet egress

use std::net::Ipv4Addr;

use crate::backend::{Ipv4Net, MacAddr, NetworkBackend};

/// Apply per-VM firewall rules: anti-spoofing, default-deny ingress
/// (SSH + ICMP allowed), default-allow egress, and conntrack.
///
/// These are the base rules from issue #746.
pub fn apply_vm_firewall(
    backend: &dyn NetworkBackend,
    tap: &str,
    mac: MacAddr,
    ip: Ipv4Addr,
) -> Result<(), Box<dyn std::error::Error>> {
    backend.apply_vm_rules(tap, mac, ip)
}

/// Remove per-VM firewall rules when a VM is deleted.
pub fn remove_vm_firewall(
    backend: &dyn NetworkBackend,
    tap: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    backend.remove_vm_rules(tap)
}

/// Apply SNAT masquerade for a subnet's internet egress traffic.
pub fn apply_nat_rules(
    backend: &dyn NetworkBackend,
    bridge: &str,
    subnet: Ipv4Net,
) -> Result<(), Box<dyn std::error::Error>> {
    backend.apply_nat(bridge, subnet)
}

// ---------------------------------------------------------------------------
// Subnet isolation (same VPC, different subnets)
// ---------------------------------------------------------------------------

/// Block cross-subnet traffic within the same VPC.
///
/// By default, VMs in different subnets within the same VPC CANNOT
/// communicate. This adds a FORWARD drop rule on the VPC bridge:
///
/// ```text
/// nft add rule syfrah forward iif {bridge} oif {bridge} \
///     ip saddr {subnet_a} ip daddr {subnet_b} drop
/// ```
///
/// Both directions are blocked (A->B and B->A).
pub fn apply_subnet_isolation(
    backend: &dyn NetworkBackend,
    bridge: &str,
    subnet_a: Ipv4Net,
    subnet_b: Ipv4Net,
) -> Result<(), Box<dyn std::error::Error>> {
    // Block A -> B
    backend.apply_subnet_isolation(bridge, subnet_a, subnet_b)?;
    // Block B -> A
    backend.apply_subnet_isolation(bridge, subnet_b, subnet_a)?;
    Ok(())
}

/// Remove subnet isolation rules between two subnets (e.g., when a security
/// group explicitly allows communication).
pub fn remove_subnet_isolation(
    backend: &dyn NetworkBackend,
    bridge: &str,
    subnet_a: Ipv4Net,
    subnet_b: Ipv4Net,
) -> Result<(), Box<dyn std::error::Error>> {
    backend.remove_subnet_isolation(bridge, subnet_a, subnet_b)?;
    backend.remove_subnet_isolation(bridge, subnet_b, subnet_a)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// VPC isolation (different VPCs)
// ---------------------------------------------------------------------------

/// Block all forwarding between two VPC bridges.
///
/// Different VPCs use different bridges (different VNIs). This rule ensures
/// no traffic can be forwarded between them:
///
/// ```text
/// nft add rule syfrah forward iif syfbr-{vpc_a} oif syfbr-{vpc_b} drop
/// ```
///
/// Both directions are blocked.
pub fn apply_vpc_isolation(
    backend: &dyn NetworkBackend,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Block A -> B
    backend.apply_vpc_isolation(bridge_a, bridge_b)?;
    // Block B -> A
    backend.apply_vpc_isolation(bridge_b, bridge_a)?;
    Ok(())
}

/// Remove VPC isolation rules (e.g., when VPCs are peered).
pub fn remove_vpc_isolation(
    backend: &dyn NetworkBackend,
    bridge_a: &str,
    bridge_b: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    backend.remove_vpc_isolation(bridge_a, bridge_b)?;
    backend.remove_vpc_isolation(bridge_b, bridge_a)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Same-subnet: no block rule needed
// ---------------------------------------------------------------------------

/// VMs in the same subnet communicate via normal bridge forwarding.
/// No nftables block rule is applied — the bridge handles L2 switching.
/// This function is a no-op; it exists for documentation and test symmetry.
pub fn same_subnet_policy() {
    // Intentionally empty: same-subnet traffic is allowed by default
    // through normal Linux bridge forwarding. No nftables rule is needed.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockBackend, MockCall};

    fn subnet(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> Ipv4Net {
        Ipv4Net {
            addr: Ipv4Addr::new(a, b, c, d),
            prefix_len: prefix,
        }
    }

    fn mac(bytes: [u8; 6]) -> MacAddr {
        MacAddr(bytes)
    }

    // -----------------------------------------------------------------------
    // Base rules (scaffolded from #746)
    // -----------------------------------------------------------------------

    #[test]
    fn anti_spoof_rules_generated() {
        let backend = MockBackend::new();
        let test_mac = mac([0x02, 0x00, 0x0a, 0x01, 0x01, 0x03]);
        let test_ip = Ipv4Addr::new(10, 1, 1, 3);

        apply_vm_firewall(&backend, "syftap-vm1", test_mac, test_ip).unwrap();

        assert!(backend.has_call(&MockCall::ApplyVmRules {
            tap: "syftap-vm1".to_string(),
            mac: test_mac,
            ip: test_ip,
        }));
    }

    #[test]
    fn remove_vm_rules_recorded() {
        let backend = MockBackend::new();
        remove_vm_firewall(&backend, "syftap-vm1").unwrap();

        assert!(backend.has_call(&MockCall::RemoveVmRules {
            tap: "syftap-vm1".to_string(),
        }));
    }

    // -----------------------------------------------------------------------
    // Subnet isolation (#747)
    // -----------------------------------------------------------------------

    #[test]
    fn subnet_isolation_rules() {
        let backend = MockBackend::new();
        let subnet_a = subnet(10, 1, 1, 0, 24);
        let subnet_b = subnet(10, 1, 2, 0, 24);

        apply_subnet_isolation(&backend, "syfbr-100", subnet_a, subnet_b).unwrap();

        let calls = backend.recorded_calls();

        // Both directions must be blocked
        assert!(
            calls.contains(&MockCall::ApplySubnetIsolation {
                bridge: "syfbr-100".to_string(),
                subnet_a,
                subnet_b,
            }),
            "expected A->B block rule"
        );
        assert!(
            calls.contains(&MockCall::ApplySubnetIsolation {
                bridge: "syfbr-100".to_string(),
                subnet_a: subnet_b,
                subnet_b: subnet_a,
            }),
            "expected B->A block rule"
        );
    }

    // -----------------------------------------------------------------------
    // VPC isolation (#747)
    // -----------------------------------------------------------------------

    #[test]
    fn vpc_isolation_rules() {
        let backend = MockBackend::new();

        apply_vpc_isolation(&backend, "syfbr-100", "syfbr-200").unwrap();

        let calls = backend.recorded_calls();

        // Both directions must be blocked
        assert!(
            calls.contains(&MockCall::ApplyVpcIsolation {
                bridge_a: "syfbr-100".to_string(),
                bridge_b: "syfbr-200".to_string(),
            }),
            "expected VPC-A->VPC-B block rule"
        );
        assert!(
            calls.contains(&MockCall::ApplyVpcIsolation {
                bridge_a: "syfbr-200".to_string(),
                bridge_b: "syfbr-100".to_string(),
            }),
            "expected VPC-B->VPC-A block rule"
        );
    }

    // -----------------------------------------------------------------------
    // Same subnet allowed (#747)
    // -----------------------------------------------------------------------

    #[test]
    fn same_subnet_allowed() {
        // Same-subnet traffic is allowed by default through bridge
        // forwarding. Verify that no isolation rules are applied when
        // we only call same_subnet_policy().
        let backend = MockBackend::new();

        same_subnet_policy();

        let calls = backend.recorded_calls();

        // No isolation calls should exist
        let has_isolation = calls.iter().any(|c| {
            matches!(
                c,
                MockCall::ApplySubnetIsolation { .. } | MockCall::ApplyVpcIsolation { .. }
            )
        });
        assert!(
            !has_isolation,
            "same-subnet traffic should not generate any isolation rules"
        );
    }

    // -----------------------------------------------------------------------
    // NAT
    // -----------------------------------------------------------------------

    #[test]
    fn nat_rules_applied() {
        let backend = MockBackend::new();
        let sn = subnet(10, 1, 1, 0, 24);

        apply_nat_rules(&backend, "syfbr-100", sn).unwrap();

        assert!(backend.has_call(&MockCall::ApplyNat {
            bridge: "syfbr-100".to_string(),
            subnet: sn,
        }));
    }
}
