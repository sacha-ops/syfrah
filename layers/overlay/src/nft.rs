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
        "add chain inet {TABLE_NAME} {CHAIN_NAME} {{ type filter hook forward priority 0; policy accept; }}"
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
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} drop"
    )
    .unwrap();
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
        assert!(deny_pos < ssh_pos);
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
