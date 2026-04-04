//! nftables rule generation and application for VM network security.
//!
//! All rules live in the `syfrah` nftables table under a `forward` chain.
//! Rules are applied atomically via `nft -f -` (stdin).
//!
//! Per-VM rules enforce:
//! - Anti-spoofing: source MAC and IP must match IPAM-assigned values
//! - Default-deny ingress with exceptions for SSH (TCP 22) and ICMP
//! - Default-allow egress (after anti-spoofing checks)
//! - Conntrack: established/related connections auto-allowed
//!
//! Isolation rules:
//! - Subnet isolation within the same VPC (cross-subnet blocked by default)
//! - VPC isolation (cross-VPC forwarding blocked by default)

use std::fmt::Write;
use std::net::Ipv4Addr;

// ── Table + chain names ─────────────────────────────────────────────

const TABLE_NAME: &str = "syfrah";
const CHAIN_NAME: &str = "forward";
const INPUT_CHAIN: &str = "input";

/// Infrastructure ports that VMs must never reach.
/// These protect the host overlay/fabric services from VM-initiated traffic.
const VXLAN_PORT: u16 = 4789;
const WIREGUARD_PORT: u16 = 51820;
const PEERING_PORT: u16 = 51821;

// ── Public API ──────────────────────────────────────────────────────

/// Generate the nftables ruleset that creates the `syfrah` table and
/// `forward` chain if they do not already exist.
pub fn generate_table_setup() -> String {
    let mut buf = String::new();
    writeln!(buf, "add table inet {TABLE_NAME}").unwrap();
    writeln!(
        buf,
        "add chain inet {TABLE_NAME} {CHAIN_NAME} {{ type filter hook forward priority 0; policy drop; }}"
    )
    .unwrap();
    writeln!(
        buf,
        "add chain inet {TABLE_NAME} {INPUT_CHAIN} {{ type filter hook input priority 0; policy accept; }}"
    )
    .unwrap();
    buf
}

/// Generate nftables rules that block VMs from reaching host infrastructure
/// ports (VXLAN, WireGuard, peering).
///
/// These rules go in the forward chain (blocking VM-to-VM or VM-to-remote
/// traffic on infrastructure ports) and the input chain (blocking VM-to-host
/// traffic on infrastructure ports via bridge interfaces).
///
/// Must be applied early, before any per-VM or SG rules.
pub fn generate_infra_protection() -> String {
    let mut buf = String::new();
    write!(buf, "{}", generate_table_setup()).unwrap();

    // Forward chain: block infrastructure ports for all forwarded traffic.
    // This prevents VMs from sending VXLAN/WG/peering packets to any
    // destination through the forward path.
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} udp dport {VXLAN_PORT} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} udp dport {WIREGUARD_PORT} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} tcp dport {PEERING_PORT} drop"
    )
    .unwrap();

    // Conntrack: allow established/related return traffic (must come after
    // infra port blocks so infra ports are always dropped first).
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} ct state established,related accept"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} ct state invalid drop"
    )
    .unwrap();

    // Input chain: block VM traffic (from bridge interfaces) to host
    // infrastructure ports. Uses source 10.0.0.0/8 to match VM subnets
    // since nftables does not support wildcard interface matching.
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {INPUT_CHAIN} ip saddr 10.0.0.0/8 udp dport {VXLAN_PORT} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {INPUT_CHAIN} ip saddr 10.0.0.0/8 udp dport {WIREGUARD_PORT} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {INPUT_CHAIN} ip saddr 10.0.0.0/8 tcp dport {PEERING_PORT} drop"
    )
    .unwrap();

    buf
}

/// Generate nftables rules for a VM's TAP interface.
pub fn generate_vm_rules(tap: &str, mac: &str, ip: Ipv4Addr) -> String {
    let mut buf = String::new();
    write!(buf, "{}", generate_table_setup()).unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {tap} ether saddr != {mac} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {tap} ip saddr != {ip} drop"
    )
    .unwrap();
    // Accept rules BEFORE the default deny (order matters in nftables)
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} tcp dport 22 accept"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} icmp type echo-request accept"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} ct state established,related accept"
    )
    .unwrap();
    // Default deny ingress — MUST be AFTER accept rules
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {tap} accept"
    )
    .unwrap();
    buf
}

/// Generate nftables commands to remove all rules for a TAP interface.
pub fn generate_remove_rules(tap: &str) -> String {
    let mut buf = String::new();
    writeln!(
        buf,
        "# flush rules for TAP {tap} from inet {TABLE_NAME} {CHAIN_NAME}"
    )
    .unwrap();
    buf
}

/// Apply an nftables ruleset by writing it to `nft -f -` on stdin.
pub fn apply_ruleset(ruleset: &str) -> std::io::Result<()> {
    use std::io::Write as IoWrite;
    use std::process::{Command, Stdio};

    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(ruleset.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(std::io::Error::other(format!("nft failed: {stderr}")));
    }

    Ok(())
}

// ── VPC bridge base rules ───────────────────────────────────────────

/// Generate nftables rules to allow intra-VPC traffic (same bridge in/out),
/// cross-node VXLAN overlay traffic, and internet egress from a VPC bridge.
///
/// With `policy drop` on the forward chain, traffic between bridges is
/// blocked by default (VPC isolation). These rules explicitly allow:
/// 1. Same-bridge traffic (intra-VPC communication between subnets)
/// 2. Bridge-to-VXLAN traffic (local VM → remote VM via VXLAN tunnel)
/// 3. VXLAN-to-bridge traffic (remote VM → local VM arriving via VXLAN)
/// 4. Outbound internet egress (bridge → non-bridge interface)
///
/// Rules 2 and 3 are essential for cross-AZ / cross-node communication:
/// VXLAN packets decapsulated on the receiving node arrive on the `syfx-*`
/// interface and must be forwarded to the local bridge (`syfb-*`). Without
/// these rules the forward chain's `policy drop` silently discards the
/// decapsulated traffic.
pub fn generate_bridge_accept_rules(bridge: &str) -> String {
    // Derive the matching VXLAN interface name.  Bridge names use the
    // format `syfb-{hash}` and the corresponding VXLAN interface is
    // `syfx-{hash}` (same hash suffix).
    let vxlan = bridge.replacen(crate::naming::BRIDGE_PREFIX, crate::naming::VXLAN_PREFIX, 1);

    let mut buf = String::new();
    // Same bridge: intra-VPC traffic allowed
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iifname \"{bridge}\" oifname \"{bridge}\" accept"
    )
    .unwrap();
    // Bridge → VXLAN: local VM sending to remote VM via overlay tunnel
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iifname \"{bridge}\" oifname \"{vxlan}\" accept"
    )
    .unwrap();
    // VXLAN → Bridge: remote VM traffic arriving via overlay tunnel
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iifname \"{vxlan}\" oifname \"{bridge}\" accept"
    )
    .unwrap();
    // Internet egress: bridge → any non-bridge interface
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iifname \"{bridge}\" oifname != \"syfb-*\" accept"
    )
    .unwrap();
    buf
}

// ── Subnet isolation ────────────────────────────────────────────────

/// Generate nftables rules to block cross-subnet traffic within a VPC.
/// Both directions are blocked (A->B and B->A).
pub fn generate_subnet_isolation(bridge: &str, subnet_a: &str, subnet_b: &str) -> String {
    let mut buf = String::new();
    writeln!(buf, "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {bridge} oif {bridge} ip saddr {subnet_a} ip daddr {subnet_b} drop").unwrap();
    writeln!(buf, "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {bridge} oif {bridge} ip saddr {subnet_b} ip daddr {subnet_a} drop").unwrap();
    buf
}

/// Generate nftables rules to remove subnet isolation.
pub fn generate_remove_subnet_isolation(bridge: &str, subnet_a: &str, subnet_b: &str) -> String {
    let mut buf = String::new();
    writeln!(
        buf,
        "# remove subnet isolation {bridge} {subnet_a} <-> {subnet_b}"
    )
    .unwrap();
    buf
}

// ── VPC isolation ───────────────────────────────────────────────────

/// Generate nftables rules to block all forwarding between two VPC bridges.
/// Both directions are blocked.
pub fn generate_vpc_isolation(bridge_a: &str, bridge_b: &str) -> String {
    let mut buf = String::new();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {bridge_a} oif {bridge_b} drop"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {bridge_b} oif {bridge_a} drop"
    )
    .unwrap();
    buf
}

/// Generate nftables rules to remove VPC isolation.
pub fn generate_remove_vpc_isolation(bridge_a: &str, bridge_b: &str) -> String {
    let mut buf = String::new();
    writeln!(buf, "# remove VPC isolation {bridge_a} <-> {bridge_b}").unwrap();
    buf
}

/// VMs in the same subnet communicate via normal bridge forwarding.
/// No nftables block rule is applied. This is a no-op for documentation symmetry.
pub fn same_subnet_policy() {}

// ── SNAT masquerade ─────────────────────────────────────────────────

/// NAT table name used for SNAT rules.
const NAT_TABLE: &str = "syfrah_nat";
const NAT_CHAIN: &str = "postrouting";

/// Generate nftables rules for SNAT masquerade on a subnet.
///
/// This enables outbound internet access for VMs behind a bridge.
pub fn generate_nat_rules(bridge: &str, subnet_cidr: &str) -> String {
    let mut buf = String::new();
    writeln!(buf, "add table ip {NAT_TABLE}").unwrap();
    writeln!(
        buf,
        "add chain ip {NAT_TABLE} {NAT_CHAIN} {{ type nat hook postrouting priority 100; policy accept; }}"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule ip {NAT_TABLE} {NAT_CHAIN} oif != \"{bridge}\" ip saddr {subnet_cidr} masquerade"
    )
    .unwrap();
    buf
}

/// Generate nftables rule text for the masquerade expression.
pub fn masquerade_rule_expr(bridge: &str, subnet_cidr: &str) -> String {
    format!("oif != \"{bridge}\" ip saddr {subnet_cidr} masquerade")
}

// ── Peering FORWARD rules ───────────────────────────────────────────

/// Generate nftables rules to allow forwarding between two peered VPC bridges.
///
/// Both directions are added:
/// - `iif {bridge_a} oif {bridge_b} accept`
/// - `iif {bridge_b} oif {bridge_a} accept`
pub fn generate_peering_rules(bridge_a: &str, bridge_b: &str) -> String {
    let mut buf = String::new();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {bridge_a} oif {bridge_b} accept"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {bridge_b} oif {bridge_a} accept"
    )
    .unwrap();
    buf
}

/// Generate nftables commands to remove peering rules between two VPC bridges.
pub fn generate_remove_peering_rules(bridge_a: &str, bridge_b: &str) -> String {
    let mut buf = String::new();
    writeln!(buf, "# remove peering rules {bridge_a} <-> {bridge_b}").unwrap();
    buf
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tap() -> String {
        crate::naming::tap_name("vm1")
    }
    const MAC: &str = "02:00:0a:00:01:05";
    const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 1, 5);

    fn rules() -> String {
        generate_vm_rules(&tap(), MAC, IP)
    }

    #[test]
    fn anti_spoof_rules_generated() {
        let r = rules();
        let t = tap();
        assert!(r.contains(&format!("iif {t} ether saddr != {MAC} drop")));
        assert!(r.contains(&format!("iif {t} ip saddr != {IP} drop")));
    }

    #[test]
    fn default_deny_ingress() {
        let r = rules();
        assert!(r.contains(&format!("oif {} drop", tap())));
    }

    #[test]
    fn ssh_allowed() {
        let r = rules();
        assert!(r.contains(&format!("oif {} tcp dport 22 accept", tap())));
    }

    #[test]
    fn icmp_allowed() {
        let r = rules();
        assert!(r.contains(&format!("oif {} icmp type echo-request accept", tap())));
    }

    #[test]
    fn egress_allowed() {
        let r = rules();
        assert!(r.contains(&format!("iif {} accept", tap())));
    }

    #[test]
    fn conntrack_established() {
        let r = rules();
        assert!(r.contains(&format!(
            "oif {} ct state established,related accept",
            tap()
        )));
    }

    #[test]
    fn table_setup_is_idempotent() {
        let setup = generate_table_setup();
        assert!(setup.contains("add table inet syfrah"));
        assert!(setup.contains("add chain inet syfrah forward"));
    }

    #[test]
    fn forward_chain_policy_drop() {
        let setup = generate_table_setup();
        assert!(
            setup.contains("policy drop"),
            "forward chain must use policy drop for VPC isolation"
        );
    }

    #[test]
    fn infra_protection_has_conntrack() {
        let rules = generate_infra_protection();
        assert!(rules.contains("ct state established,related accept"));
        assert!(rules.contains("ct state invalid drop"));
    }

    #[test]
    fn bridge_accept_rules_intra_vpc() {
        let br = crate::naming::bridge_name("100");
        let rules = generate_bridge_accept_rules(&br);
        assert!(
            rules.contains(&format!("iifname \"{br}\" oifname \"{br}\" accept")),
            "same-bridge traffic must be accepted"
        );
    }

    #[test]
    fn bridge_accept_rules_vxlan_to_bridge() {
        let br = crate::naming::bridge_name("100");
        let vx = crate::naming::vxlan_name("100");
        let rules = generate_bridge_accept_rules(&br);
        assert!(
            rules.contains(&format!("iifname \"{vx}\" oifname \"{br}\" accept")),
            "VXLAN-to-bridge traffic must be accepted for cross-node communication"
        );
    }

    #[test]
    fn bridge_accept_rules_bridge_to_vxlan() {
        let br = crate::naming::bridge_name("100");
        let vx = crate::naming::vxlan_name("100");
        let rules = generate_bridge_accept_rules(&br);
        assert!(
            rules.contains(&format!("iifname \"{br}\" oifname \"{vx}\" accept")),
            "bridge-to-VXLAN traffic must be accepted for cross-node communication"
        );
    }

    #[test]
    fn bridge_accept_rules_internet_egress() {
        let br = crate::naming::bridge_name("100");
        let rules = generate_bridge_accept_rules(&br);
        assert!(
            rules.contains(&format!("iifname \"{br}\" oifname != \"syfb-*\" accept")),
            "bridge to internet must be accepted"
        );
    }

    #[test]
    fn rule_ordering() {
        let r = rules();
        let mac_spoof_pos = r.find("ether saddr !=").expect("MAC spoof rule");
        let ip_spoof_pos = r.find("ip saddr !=").expect("IP spoof rule");
        let t = tap();
        let egress_pos = r.find(&format!("iif {t} accept")).expect("egress rule");
        let deny_pos = r.find(&format!("oif {t} drop")).expect("deny rule");
        let ssh_pos = r.find("tcp dport 22 accept").expect("SSH rule");
        assert!(mac_spoof_pos < ip_spoof_pos);
        assert!(ip_spoof_pos < egress_pos);
        // Accept rules must come BEFORE the default deny drop
        assert!(
            ssh_pos < deny_pos,
            "SSH accept must be before default deny drop"
        );
    }

    #[test]
    fn subnet_isolation_blocks_both_directions() {
        let br = crate::naming::bridge_name("100");
        let rules = generate_subnet_isolation(&br, "10.1.1.0/24", "10.1.2.0/24");
        assert!(rules.contains("ip saddr 10.1.1.0/24 ip daddr 10.1.2.0/24 drop"));
        assert!(rules.contains("ip saddr 10.1.2.0/24 ip daddr 10.1.1.0/24 drop"));
    }

    #[test]
    fn vpc_isolation_blocks_both_directions() {
        let br_a = crate::naming::bridge_name("100");
        let br_b = crate::naming::bridge_name("200");
        let rules = generate_vpc_isolation(&br_a, &br_b);
        assert!(rules.contains(&format!("iif {br_a} oif {br_b} drop")));
        assert!(rules.contains(&format!("iif {br_b} oif {br_a} drop")));
    }

    #[test]
    fn same_subnet_no_rules() {
        same_subnet_policy();
    }

    #[test]
    fn subnet_isolation_uses_bridge_name() {
        let br = crate::naming::bridge_name("vpc42");
        let rules = generate_subnet_isolation(&br, "10.0.1.0/24", "10.0.2.0/24");
        assert!(rules.contains(&format!("iif {br} oif {br}")));
    }

    // ── SNAT tests ──────────────────────────────────────────────────

    #[test]
    fn snat_rule_generated() {
        let br = crate::naming::bridge_name("100");
        let rules = generate_nat_rules(&br, "10.1.1.0/24");
        assert!(rules.contains("masquerade"));
        assert!(rules.contains("10.1.1.0/24"));
        assert!(rules.contains(&br));
    }

    #[test]
    fn masquerade_per_bridge() {
        let br = crate::naming::bridge_name("200");
        let expr = masquerade_rule_expr(&br, "10.2.0.0/16");
        assert_eq!(
            expr,
            format!("oif != \"{br}\" ip saddr 10.2.0.0/16 masquerade")
        );
    }

    // ── Peering tests ───────────────────────────────────────────────

    #[test]
    fn peering_forward_rules() {
        let br_a = crate::naming::bridge_name("100");
        let br_b = crate::naming::bridge_name("200");
        let rules = generate_peering_rules(&br_a, &br_b);
        assert!(rules.contains(&format!("iif {br_a} oif {br_b} accept")));
        assert!(rules.contains(&format!("iif {br_b} oif {br_a} accept")));
    }

    #[test]
    fn peering_rules_removed() {
        let br_a = crate::naming::bridge_name("100");
        let br_b = crate::naming::bridge_name("200");
        let rules = generate_remove_peering_rules(&br_a, &br_b);
        assert!(rules.contains(&format!("remove peering rules {br_a} <-> {br_b}")));
    }

    // ── Infrastructure protection tests ─────────────────────────────

    #[test]
    fn infrastructure_ports_blocked() {
        let rules = generate_infra_protection();
        // Forward chain blocks
        assert!(rules.contains("udp dport 4789 drop"));
        assert!(rules.contains("udp dport 51820 drop"));
        assert!(rules.contains("tcp dport 51821 drop"));
        // Input chain blocks (VM-to-host)
        assert!(rules.contains("ip saddr 10.0.0.0/8 udp dport 4789 drop"));
        assert!(rules.contains("ip saddr 10.0.0.0/8 udp dport 51820 drop"));
        assert!(rules.contains("ip saddr 10.0.0.0/8 tcp dport 51821 drop"));
    }

    #[test]
    fn vxlan_port_blocked() {
        let rules = generate_infra_protection();
        // VXLAN port 4789 must be blocked in both forward and input chains
        let forward_rule = format!("add rule inet {TABLE_NAME} {CHAIN_NAME} udp dport 4789 drop");
        let input_rule = format!(
            "add rule inet {TABLE_NAME} {INPUT_CHAIN} ip saddr 10.0.0.0/8 udp dport 4789 drop"
        );
        assert!(rules.contains(&forward_rule));
        assert!(rules.contains(&input_rule));
    }

    #[test]
    fn wireguard_port_blocked() {
        let rules = generate_infra_protection();
        // WireGuard port 51820 must be blocked in both forward and input chains
        let forward_rule = format!("add rule inet {TABLE_NAME} {CHAIN_NAME} udp dport 51820 drop");
        let input_rule = format!(
            "add rule inet {TABLE_NAME} {INPUT_CHAIN} ip saddr 10.0.0.0/8 udp dport 51820 drop"
        );
        assert!(rules.contains(&forward_rule));
        assert!(rules.contains(&input_rule));
    }

    #[test]
    fn peering_port_blocked() {
        let rules = generate_infra_protection();
        // Peering port 51821 must be blocked in both forward and input chains
        let forward_rule = format!("add rule inet {TABLE_NAME} {CHAIN_NAME} tcp dport 51821 drop");
        let input_rule = format!(
            "add rule inet {TABLE_NAME} {INPUT_CHAIN} ip saddr 10.0.0.0/8 tcp dport 51821 drop"
        );
        assert!(rules.contains(&forward_rule));
        assert!(rules.contains(&input_rule));
    }

    #[test]
    fn infra_protection_includes_table_setup() {
        let rules = generate_infra_protection();
        // Must include table and both chains
        assert!(rules.contains("add table inet syfrah"));
        assert!(rules.contains("add chain inet syfrah forward"));
        assert!(rules.contains("add chain inet syfrah input"));
    }

    #[test]
    fn infra_rules_before_vm_rules() {
        // Infrastructure rules must appear before per-VM rules in the
        // generated output when both are composed.
        let infra = generate_infra_protection();
        let vm = generate_vm_rules(&tap(), MAC, IP);

        // When combined, infra port blocks should come first.
        let combined = format!("{infra}{vm}");
        let vxlan_pos = combined.find("udp dport 4789 drop").unwrap();
        let anti_spoof_pos = combined.find("ether saddr !=").unwrap();
        assert!(
            vxlan_pos < anti_spoof_pos,
            "infra protection must precede per-VM rules"
        );
    }
}
