//! Security Group nftables rule generation.
//!
//! Converts `SecurityGroupRule` objects into nftables chain rules for
//! per-VM ingress chains. Rules from all SGs attached to a NIC are
//! merged and sorted by priority before generation.
//!
//! This module handles ingress only. Egress, named sets, and atomic
//! apply are in separate modules.

use std::fmt::Write;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::sg::{Direction, Protocol, SecurityGroupId, SecurityGroupRule, TrafficSource};

// ── NIC type ───────────────────────────────────────────────────────

/// Unique identifier for a NIC.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct NicId(pub String);

impl std::fmt::Display for NicId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Minimal NIC representation for nftables rule generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub id: NicId,
    pub vm_id: String,
    pub private_ip: Ipv4Addr,
    pub mac: String,
    pub security_groups: Vec<SecurityGroupId>,
}

// ── nftables rule generation ───────────────────────────────────────

/// A single nftables rule statement (text form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftRule {
    pub text: String,
}

/// Short hash for chain/set naming, consistent with `naming.rs`.
fn short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xFFFF_FFFF)
}

/// Compute the per-VM ingress chain name: `vm_{hash}_in`.
pub fn ingress_chain_name(vm_id: &str) -> String {
    format!("vm_{}_in", short_hash(vm_id))
}

/// Generate the nftables ingress chain rules for a NIC.
///
/// Collects all ingress rules from the provided list, sorts by priority
/// (ascending -- lower number evaluated first), translates each to an nft
/// rule statement, and appends an implicit `drop` at the end.
///
/// Returns a vector of `NftRule` representing the chain contents.
pub fn generate_ingress_chain(
    _nic: &NetworkInterface,
    rules: &[SecurityGroupRule],
) -> Vec<NftRule> {
    // Filter to ingress rules only, then sort by priority ascending.
    let mut ingress: Vec<&SecurityGroupRule> = rules
        .iter()
        .filter(|r| r.direction == Direction::Ingress)
        .collect();
    ingress.sort_by_key(|r| r.priority);

    let mut nft_rules: Vec<NftRule> = Vec::with_capacity(ingress.len() + 1);

    for rule in &ingress {
        if let Some(text) = translate_rule(rule) {
            nft_rules.push(NftRule { text });
        }
    }

    // Implicit deny at end of chain.
    nft_rules.push(NftRule {
        text: "drop".to_string(),
    });

    nft_rules
}

/// Render a full nftables chain definition as a string.
///
/// Produces:
/// ```text
/// chain vm_{hash}_in {
///     tcp dport 22 accept
///     drop
/// }
/// ```
pub fn render_ingress_chain(vm_id: &str, rules: &[NftRule]) -> String {
    let chain = ingress_chain_name(vm_id);
    let mut buf = String::new();
    writeln!(buf, "chain {chain} {{").unwrap();
    for rule in rules {
        writeln!(buf, "    {}", rule.text).unwrap();
    }
    writeln!(buf, "}}").unwrap();
    buf
}

/// Translate a single `SecurityGroupRule` into an nft rule statement.
///
/// Returns `None` if the rule cannot be translated (e.g., egress rule
/// passed by mistake -- should not happen after filtering).
fn translate_rule(rule: &SecurityGroupRule) -> Option<String> {
    if rule.direction != Direction::Ingress {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();

    // Source filter.
    match &rule.source {
        TrafficSource::Cidr(cidr) if cidr != "0.0.0.0/0" => {
            parts.push(format!("ip saddr {cidr}"));
        }
        TrafficSource::SecurityGroup(sg_name) => {
            let set_name = format!("sg_{}_ips", short_hash(sg_name));
            parts.push(format!("ip saddr @{set_name}"));
        }
        _ => {
            // 0.0.0.0/0 means any source -- no filter needed.
        }
    }

    // Protocol + port.
    match rule.protocol {
        Protocol::Tcp => {
            if let Some(ref pr) = rule.port_range {
                if pr.from == pr.to {
                    parts.push(format!("tcp dport {}", pr.from));
                } else {
                    parts.push(format!("tcp dport {}-{}", pr.from, pr.to));
                }
            } else {
                parts.push("tcp dport 0-65535".to_string());
            }
        }
        Protocol::Udp => {
            if let Some(ref pr) = rule.port_range {
                if pr.from == pr.to {
                    parts.push(format!("udp dport {}", pr.from));
                } else {
                    parts.push(format!("udp dport {}-{}", pr.from, pr.to));
                }
            } else {
                parts.push("udp dport 0-65535".to_string());
            }
        }
        Protocol::Icmp => {
            parts.push("icmp type echo-request".to_string());
        }
        Protocol::All => {
            // No protocol filter -- accept all protocols.
        }
    }

    parts.push("accept".to_string());
    Some(parts.join(" "))
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sg::{PortRange, RuleId};

    fn test_nic() -> NetworkInterface {
        NetworkInterface {
            id: NicId("nic-1".to_string()),
            vm_id: "vm-1".to_string(),
            private_ip: Ipv4Addr::new(10, 1, 0, 5),
            mac: "02:00:0a:01:00:05".to_string(),
            security_groups: vec![SecurityGroupId("sg-default".to_string())],
        }
    }

    fn ingress_rule(
        protocol: Protocol,
        port_range: Option<PortRange>,
        source: TrafficSource,
        priority: u32,
    ) -> SecurityGroupRule {
        SecurityGroupRule {
            id: RuleId(format!("rule-{priority}")),
            sg_id: SecurityGroupId("sg-default".to_string()),
            direction: Direction::Ingress,
            protocol,
            port_range,
            source,
            priority,
            description: String::new(),
            created_at: 0,
        }
    }

    #[test]
    fn test_generate_ingress_tcp() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        assert_eq!(chain.len(), 2); // rule + drop
        assert_eq!(chain[0].text, "tcp dport 22 accept");
        assert_eq!(chain[1].text, "drop");
    }

    #[test]
    fn test_generate_ingress_udp_range() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Udp,
            Some(PortRange {
                from: 8000,
                to: 9000,
            }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "udp dport 8000-9000 accept");
    }

    #[test]
    fn test_generate_ingress_icmp() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Icmp,
            None,
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            200,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "icmp type echo-request accept");
    }

    #[test]
    fn test_generate_ingress_cidr_source() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 443, to: 443 }),
            TrafficSource::Cidr("10.0.0.0/8".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "ip saddr 10.0.0.0/8 tcp dport 443 accept");
    }

    #[test]
    fn test_generate_ingress_sg_source() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange {
                from: 5432,
                to: 5432,
            }),
            TrafficSource::SecurityGroup("web-sg".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        let expected_set = format!("sg_{}_ips", short_hash("web-sg"));
        assert_eq!(
            chain[0].text,
            format!("ip saddr @{expected_set} tcp dport 5432 accept")
        );
    }

    #[test]
    fn test_merge_multiple_sgs() {
        let nic = test_nic();
        // Two rules from different SGs, different priorities.
        let rules = vec![
            SecurityGroupRule {
                id: RuleId("r1".to_string()),
                sg_id: SecurityGroupId("sg-a".to_string()),
                direction: Direction::Ingress,
                protocol: Protocol::Tcp,
                port_range: Some(PortRange { from: 22, to: 22 }),
                source: TrafficSource::Cidr("0.0.0.0/0".to_string()),
                priority: 200,
                description: "SSH".to_string(),
                created_at: 0,
            },
            SecurityGroupRule {
                id: RuleId("r2".to_string()),
                sg_id: SecurityGroupId("sg-b".to_string()),
                direction: Direction::Ingress,
                protocol: Protocol::Tcp,
                port_range: Some(PortRange { from: 443, to: 443 }),
                source: TrafficSource::Cidr("0.0.0.0/0".to_string()),
                priority: 100,
                description: "HTTPS".to_string(),
                created_at: 0,
            },
        ];
        let chain = generate_ingress_chain(&nic, &rules);
        // Priority 100 (HTTPS) should come before priority 200 (SSH).
        assert_eq!(chain.len(), 3); // 2 rules + drop
        assert_eq!(chain[0].text, "tcp dport 443 accept");
        assert_eq!(chain[1].text, "tcp dport 22 accept");
        assert_eq!(chain[2].text, "drop");
    }

    #[test]
    fn test_implicit_deny() {
        let nic = test_nic();
        // Even with no rules, chain must end with drop.
        let chain = generate_ingress_chain(&nic, &[]);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].text, "drop");
    }

    #[test]
    fn test_egress_rules_filtered_out() {
        let nic = test_nic();
        let rules = vec![SecurityGroupRule {
            id: RuleId("r1".to_string()),
            sg_id: SecurityGroupId("sg-a".to_string()),
            direction: Direction::Egress,
            protocol: Protocol::Tcp,
            port_range: Some(PortRange { from: 443, to: 443 }),
            source: TrafficSource::Cidr("0.0.0.0/0".to_string()),
            priority: 100,
            description: "HTTPS out".to_string(),
            created_at: 0,
        }];
        let chain = generate_ingress_chain(&nic, &rules);
        // Only the implicit drop -- egress rules are not included.
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].text, "drop");
    }

    #[test]
    fn test_protocol_all_any_source() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::All,
            None,
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "accept");
    }

    #[test]
    fn test_protocol_all_cidr_source() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::All,
            None,
            TrafficSource::Cidr("10.1.0.0/16".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "ip saddr 10.1.0.0/16 accept");
    }

    #[test]
    fn test_render_ingress_chain() {
        let rules = vec![
            NftRule {
                text: "tcp dport 22 accept".to_string(),
            },
            NftRule {
                text: "drop".to_string(),
            },
        ];
        let rendered = render_ingress_chain("vm-1", &rules);
        let chain_name = ingress_chain_name("vm-1");
        assert!(rendered.contains(&format!("chain {chain_name} {{")));
        assert!(rendered.contains("    tcp dport 22 accept"));
        assert!(rendered.contains("    drop"));
        assert!(rendered.contains('}'));
    }

    #[test]
    fn test_ingress_chain_name_deterministic() {
        assert_eq!(ingress_chain_name("vm-1"), ingress_chain_name("vm-1"));
        assert_ne!(ingress_chain_name("vm-1"), ingress_chain_name("vm-2"));
    }

    #[test]
    fn test_single_port_no_range() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 80, to: 80 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_ingress_chain(&nic, &rules);
        // Single port should not produce a range.
        assert_eq!(chain[0].text, "tcp dport 80 accept");
        assert!(!chain[0].text.contains('-'));
    }
}
