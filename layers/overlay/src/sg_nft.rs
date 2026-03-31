//! Per-VM chain nftables architecture with vmap dispatch for security groups.
//!
//! Replaces the flat single-chain approach in `nft.rs` with isolated per-VM
//! chains and O(1) vmap-based interface dispatch. Each VM gets:
//!
//! - `spoof_{vm_id}` — anti-spoofing: validates source MAC + IP
//! - `in_{vm_id}`    — ingress rules from attached security groups
//! - `out_{vm_id}`   — egress rules (default allow if none specified)
//!
//! Three vmaps route traffic to the correct chain:
//! - `spoofcheck`     — iif → spoof chain (outbound from VM)
//! - `ingress_chains` — oif → ingress chain (inbound to VM)
//! - `egress_chains`  — iif → egress chain (outbound from VM)
//!
//! The global `forward` chain uses `policy drop` so unmatched traffic is denied.

use std::fmt::Write;

// ── Constants ──────────────────────────────────────────────────────

const TABLE: &str = "syfrah";

// ── Security group rule model ──────────────────────────────────────

/// Direction of a security group rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ingress,
    Egress,
}

/// Protocol for a security group rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
    Icmp,
    All,
}

/// Port range (inclusive). For a single port, `from == to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub from: u16,
    pub to: u16,
}

/// A single security group rule used for nftables generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityGroupRule {
    pub direction: Direction,
    pub protocol: Protocol,
    pub port_range: Option<PortRange>,
    /// CIDR source (ingress) or destination (egress), e.g. "0.0.0.0/0".
    pub cidr: String,
    /// Lower priority = evaluated first.
    pub priority: u32,
}

// ── Generator ──────────────────────────────────────────────────────

/// Generates nftables rulesets for per-VM chain architecture.
pub struct SgNftGenerator;

impl SgNftGenerator {
    /// Generate the complete nftables ruleset for a single VM.
    ///
    /// Returns an `nft -f` compatible ruleset that creates/replaces the
    /// VM's spoof, ingress, and egress chains.
    pub fn generate_vm_ruleset(
        vm_id: &str,
        iface: &str,
        mac: &str,
        ip: &str,
        ingress_rules: &[SecurityGroupRule],
        egress_rules: &[SecurityGroupRule],
    ) -> String {
        let mut buf = String::new();

        // Ensure table exists
        writeln!(buf, "add table inet {TABLE}").unwrap();

        // ── Spoof chain ────────────────────────────────────────────
        let spoof_chain = format!("spoof_{vm_id}");
        writeln!(buf, "add chain inet {TABLE} {spoof_chain}").unwrap();
        writeln!(buf, "flush chain inet {TABLE} {spoof_chain}").unwrap();
        writeln!(
            buf,
            "add rule inet {TABLE} {spoof_chain} ether saddr != {mac} drop"
        )
        .unwrap();
        writeln!(
            buf,
            "add rule inet {TABLE} {spoof_chain} ip saddr != {ip} drop"
        )
        .unwrap();

        // ── Ingress chain (traffic entering VM) ────────────────────
        let in_chain = format!("in_{vm_id}");
        writeln!(buf, "add chain inet {TABLE} {in_chain}").unwrap();
        writeln!(buf, "flush chain inet {TABLE} {in_chain}").unwrap();

        let mut sorted_ingress: Vec<_> = ingress_rules
            .iter()
            .filter(|r| r.direction == Direction::Ingress)
            .collect();
        sorted_ingress.sort_by_key(|r| r.priority);

        for rule in &sorted_ingress {
            write_sg_rule(&mut buf, &in_chain, rule);
        }
        // Default deny at end of ingress chain
        writeln!(buf, "add rule inet {TABLE} {in_chain} drop").unwrap();

        // ── Egress chain (traffic leaving VM) ──────────────────────
        let out_chain = format!("out_{vm_id}");
        writeln!(buf, "add chain inet {TABLE} {out_chain}").unwrap();
        writeln!(buf, "flush chain inet {TABLE} {out_chain}").unwrap();

        let mut sorted_egress: Vec<_> = egress_rules
            .iter()
            .filter(|r| r.direction == Direction::Egress)
            .collect();
        sorted_egress.sort_by_key(|r| r.priority);

        for rule in &sorted_egress {
            write_sg_rule(&mut buf, &out_chain, rule);
        }
        // Default: accept all egress if no explicit deny
        writeln!(buf, "add rule inet {TABLE} {out_chain} accept").unwrap();

        // ── Update vmap entries for this VM ─────────────────────────
        Self::write_vmap_entries(&mut buf, vm_id, iface);

        buf
    }

    /// Generate the global dispatch table: the `forward` chain with vmap
    /// references, plus the three maps.
    ///
    /// `vms` is a list of `(vm_id, iface)` pairs for all VMs on this host.
    pub fn generate_dispatch_table(vms: &[(String, String)]) -> String {
        let mut buf = String::new();

        writeln!(buf, "add table inet {TABLE}").unwrap();

        // Forward chain with policy drop
        writeln!(
            buf,
            "add chain inet {TABLE} forward {{ type filter hook forward priority 0; policy drop; }}"
        )
        .unwrap();
        writeln!(buf, "flush chain inet {TABLE} forward").unwrap();

        // Maps
        writeln!(
            buf,
            "add map inet {TABLE} spoofcheck {{ type ifname : verdict; }}"
        )
        .unwrap();
        writeln!(
            buf,
            "add map inet {TABLE} ingress_chains {{ type ifname : verdict; }}"
        )
        .unwrap();
        writeln!(
            buf,
            "add map inet {TABLE} egress_chains {{ type ifname : verdict; }}"
        )
        .unwrap();

        // Forward chain rules
        writeln!(buf, "add rule inet {TABLE} forward iif vmap @spoofcheck").unwrap();
        writeln!(
            buf,
            "add rule inet {TABLE} forward ct state established,related accept"
        )
        .unwrap();
        writeln!(buf, "add rule inet {TABLE} forward ct state invalid drop").unwrap();
        writeln!(
            buf,
            "add rule inet {TABLE} forward oif vmap @ingress_chains"
        )
        .unwrap();
        writeln!(buf, "add rule inet {TABLE} forward iif vmap @egress_chains").unwrap();

        // Populate maps
        for (vm_id, iface) in vms {
            Self::write_vmap_entries(&mut buf, vm_id, iface);
        }

        buf
    }

    /// Generate nftables commands to remove a VM's chains and vmap entries.
    pub fn generate_remove_vm(vm_id: &str, iface: &str) -> String {
        let mut buf = String::new();

        // Remove vmap entries
        writeln!(buf, "delete element inet {TABLE} spoofcheck {{ {iface} }}").unwrap();
        writeln!(
            buf,
            "delete element inet {TABLE} ingress_chains {{ {iface} }}"
        )
        .unwrap();
        writeln!(
            buf,
            "delete element inet {TABLE} egress_chains {{ {iface} }}"
        )
        .unwrap();

        // Flush and delete per-VM chains
        writeln!(buf, "flush chain inet {TABLE} spoof_{vm_id}").unwrap();
        writeln!(buf, "delete chain inet {TABLE} spoof_{vm_id}").unwrap();
        writeln!(buf, "flush chain inet {TABLE} in_{vm_id}").unwrap();
        writeln!(buf, "delete chain inet {TABLE} in_{vm_id}").unwrap();
        writeln!(buf, "flush chain inet {TABLE} out_{vm_id}").unwrap();
        writeln!(buf, "delete chain inet {TABLE} out_{vm_id}").unwrap();

        buf
    }

    /// Write vmap element additions for a single VM.
    fn write_vmap_entries(buf: &mut String, vm_id: &str, iface: &str) {
        writeln!(
            buf,
            "add element inet {TABLE} spoofcheck {{ {iface} : goto spoof_{vm_id} }}"
        )
        .unwrap();
        writeln!(
            buf,
            "add element inet {TABLE} ingress_chains {{ {iface} : goto in_{vm_id} }}"
        )
        .unwrap();
        writeln!(
            buf,
            "add element inet {TABLE} egress_chains {{ {iface} : goto out_{vm_id} }}"
        )
        .unwrap();
    }
}

/// Apply an nftables ruleset via `nft -f -` on stdin.
pub fn apply_sg_ruleset(ruleset: &str) -> std::io::Result<()> {
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

// ── Private helpers ────────────────────────────────────────────────

fn write_sg_rule(buf: &mut String, chain: &str, rule: &SecurityGroupRule) {
    let proto = match rule.protocol {
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
        Protocol::Icmp => "icmp",
        Protocol::All => "all",
    };

    match rule.protocol {
        Protocol::Icmp => {
            if rule.cidr == "0.0.0.0/0" {
                writeln!(
                    buf,
                    "add rule inet {TABLE} {chain} icmp type echo-request accept"
                )
                .unwrap();
            } else {
                writeln!(
                    buf,
                    "add rule inet {TABLE} {chain} ip saddr {} icmp type echo-request accept",
                    rule.cidr
                )
                .unwrap();
            }
        }
        Protocol::All => {
            if rule.cidr == "0.0.0.0/0" {
                writeln!(buf, "add rule inet {TABLE} {chain} accept").unwrap();
            } else {
                writeln!(
                    buf,
                    "add rule inet {TABLE} {chain} ip saddr {} accept",
                    rule.cidr
                )
                .unwrap();
            }
        }
        Protocol::Tcp | Protocol::Udp => {
            let port_expr = match rule.port_range {
                Some(PortRange { from, to }) if from == to => format!("{proto} dport {from}"),
                Some(PortRange { from, to }) => format!("{proto} dport {from}-{to}"),
                None => proto.to_string(),
            };
            if rule.cidr == "0.0.0.0/0" {
                writeln!(buf, "add rule inet {TABLE} {chain} {port_expr} accept").unwrap();
            } else {
                writeln!(
                    buf,
                    "add rule inet {TABLE} {chain} ip saddr {} {port_expr} accept",
                    rule.cidr
                )
                .unwrap();
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const VM_ID: &str = "vm1";
    const IFACE: &str = "syft-abc12345";
    const MAC: &str = "02:00:0a:00:01:05";
    const IP: &str = "10.0.1.5";

    fn ssh_rule() -> SecurityGroupRule {
        SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            port_range: Some(PortRange { from: 22, to: 22 }),
            cidr: "0.0.0.0/0".into(),
            priority: 100,
        }
    }

    fn icmp_rule() -> SecurityGroupRule {
        SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Icmp,
            port_range: None,
            cidr: "0.0.0.0/0".into(),
            priority: 200,
        }
    }

    fn egress_all_rule() -> SecurityGroupRule {
        SecurityGroupRule {
            direction: Direction::Egress,
            protocol: Protocol::All,
            port_range: None,
            cidr: "0.0.0.0/0".into(),
            priority: 100,
        }
    }

    #[test]
    fn generate_vm_ruleset_basic() {
        let ingress = vec![ssh_rule(), icmp_rule()];
        let egress = vec![egress_all_rule()];

        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &ingress, &egress);

        // Table creation
        assert!(ruleset.contains("add table inet syfrah"));

        // Spoof chain
        assert!(ruleset.contains("add chain inet syfrah spoof_vm1"));
        assert!(ruleset.contains(&format!(
            "add rule inet syfrah spoof_vm1 ether saddr != {MAC} drop"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet syfrah spoof_vm1 ip saddr != {IP} drop"
        )));

        // Ingress chain with rules
        assert!(ruleset.contains("add chain inet syfrah in_vm1"));
        assert!(ruleset.contains("add rule inet syfrah in_vm1 tcp dport 22 accept"));
        assert!(ruleset.contains("add rule inet syfrah in_vm1 icmp type echo-request accept"));
        // Default deny at end
        assert!(ruleset.contains("add rule inet syfrah in_vm1 drop"));

        // Egress chain
        assert!(ruleset.contains("add chain inet syfrah out_vm1"));
        // Explicit accept from egress_all_rule + default accept
        assert!(ruleset.contains("add rule inet syfrah out_vm1 accept"));

        // Vmap entries
        assert!(ruleset.contains(&format!(
            "add element inet syfrah spoofcheck {{ {IFACE} : goto spoof_vm1 }}"
        )));
        assert!(ruleset.contains(&format!(
            "add element inet syfrah ingress_chains {{ {IFACE} : goto in_vm1 }}"
        )));
        assert!(ruleset.contains(&format!(
            "add element inet syfrah egress_chains {{ {IFACE} : goto out_vm1 }}"
        )));
    }

    #[test]
    fn generate_dispatch_table() {
        let vms = vec![
            ("vm1".to_string(), "syft-aaa".to_string()),
            ("vm2".to_string(), "syft-bbb".to_string()),
        ];

        let ruleset = SgNftGenerator::generate_dispatch_table(&vms);

        // Forward chain
        assert!(ruleset.contains("add chain inet syfrah forward"));
        assert!(ruleset.contains("policy drop"));

        // Maps
        assert!(ruleset.contains("add map inet syfrah spoofcheck"));
        assert!(ruleset.contains("add map inet syfrah ingress_chains"));
        assert!(ruleset.contains("add map inet syfrah egress_chains"));

        // Forward chain rules
        assert!(ruleset.contains("iif vmap @spoofcheck"));
        assert!(ruleset.contains("ct state established,related accept"));
        assert!(ruleset.contains("ct state invalid drop"));
        assert!(ruleset.contains("oif vmap @ingress_chains"));
        assert!(ruleset.contains("iif vmap @egress_chains"));

        // Vmap entries for both VMs
        assert!(
            ruleset.contains("add element inet syfrah spoofcheck { syft-aaa : goto spoof_vm1 }")
        );
        assert!(
            ruleset.contains("add element inet syfrah spoofcheck { syft-bbb : goto spoof_vm2 }")
        );
        assert!(
            ruleset.contains("add element inet syfrah ingress_chains { syft-aaa : goto in_vm1 }")
        );
        assert!(
            ruleset.contains("add element inet syfrah ingress_chains { syft-bbb : goto in_vm2 }")
        );
        assert!(
            ruleset.contains("add element inet syfrah egress_chains { syft-aaa : goto out_vm1 }")
        );
        assert!(
            ruleset.contains("add element inet syfrah egress_chains { syft-bbb : goto out_vm2 }")
        );
    }

    #[test]
    fn spoofcheck_chain_correct() {
        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &[], &[]);

        // Spoof chain must check MAC first, then IP
        let mac_pos = ruleset
            .find(&format!("ether saddr != {MAC} drop"))
            .expect("MAC spoof rule");
        let ip_pos = ruleset
            .find(&format!("ip saddr != {IP} drop"))
            .expect("IP spoof rule");
        assert!(mac_pos < ip_pos, "MAC check must come before IP check");
    }

    #[test]
    fn empty_rules_default_deny() {
        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &[], &[]);

        // Ingress chain with no rules should still have drop at the end
        assert!(ruleset.contains("add rule inet syfrah in_vm1 drop"));

        // Verify no accept rules in ingress chain (only the egress chain has accept)
        let in_chain_start = ruleset.find("flush chain inet syfrah in_vm1").unwrap();
        let in_chain_end = ruleset.find("add chain inet syfrah out_vm1").unwrap();
        let in_section = &ruleset[in_chain_start..in_chain_end];
        assert!(
            !in_section.contains("accept"),
            "ingress chain with no rules should not contain accept"
        );
    }

    #[test]
    fn egress_default_allow() {
        // No egress rules specified => default accept
        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &[], &[]);

        // Egress chain must end with accept
        let out_chain_start = ruleset.find("flush chain inet syfrah out_vm1").unwrap();
        let out_section = &ruleset[out_chain_start..];
        assert!(
            out_section.contains("add rule inet syfrah out_vm1 accept"),
            "egress chain with no rules should default to accept"
        );
    }

    #[test]
    fn remove_vm_cleans_up() {
        let ruleset = SgNftGenerator::generate_remove_vm(VM_ID, IFACE);

        // Vmap entries removed
        assert!(ruleset.contains(&format!(
            "delete element inet syfrah spoofcheck {{ {IFACE} }}"
        )));
        assert!(ruleset.contains(&format!(
            "delete element inet syfrah ingress_chains {{ {IFACE} }}"
        )));
        assert!(ruleset.contains(&format!(
            "delete element inet syfrah egress_chains {{ {IFACE} }}"
        )));

        // Chains flushed and deleted
        assert!(ruleset.contains("flush chain inet syfrah spoof_vm1"));
        assert!(ruleset.contains("delete chain inet syfrah spoof_vm1"));
        assert!(ruleset.contains("flush chain inet syfrah in_vm1"));
        assert!(ruleset.contains("delete chain inet syfrah in_vm1"));
        assert!(ruleset.contains("flush chain inet syfrah out_vm1"));
        assert!(ruleset.contains("delete chain inet syfrah out_vm1"));
    }

    #[test]
    fn ingress_rules_ordered_by_priority() {
        let rules = vec![
            SecurityGroupRule {
                direction: Direction::Ingress,
                protocol: Protocol::Tcp,
                port_range: Some(PortRange { from: 443, to: 443 }),
                cidr: "0.0.0.0/0".into(),
                priority: 200,
            },
            SecurityGroupRule {
                direction: Direction::Ingress,
                protocol: Protocol::Tcp,
                port_range: Some(PortRange { from: 22, to: 22 }),
                cidr: "0.0.0.0/0".into(),
                priority: 100,
            },
        ];

        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &rules, &[]);

        let ssh_pos = ruleset.find("tcp dport 22 accept").expect("SSH rule");
        let https_pos = ruleset.find("tcp dport 443 accept").expect("HTTPS rule");
        assert!(
            ssh_pos < https_pos,
            "SSH (priority 100) should come before HTTPS (priority 200)"
        );
    }

    #[test]
    fn cidr_restricted_rule() {
        let rules = vec![SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            port_range: Some(PortRange {
                from: 5432,
                to: 5432,
            }),
            cidr: "10.0.1.0/24".into(),
            priority: 100,
        }];

        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &rules, &[]);

        assert!(ruleset.contains("ip saddr 10.0.1.0/24 tcp dport 5432 accept"));
    }

    #[test]
    fn port_range_rule() {
        let rules = vec![SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            port_range: Some(PortRange {
                from: 8000,
                to: 9000,
            }),
            cidr: "0.0.0.0/0".into(),
            priority: 100,
        }];

        let ruleset = SgNftGenerator::generate_vm_ruleset(VM_ID, IFACE, MAC, IP, &rules, &[]);

        assert!(ruleset.contains("tcp dport 8000-9000 accept"));
    }
}
