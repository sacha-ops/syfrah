//! Security Group nftables rule generation and atomic application.
//!
//! Converts `SecurityGroupRule` objects into nftables chain rules for
//! per-VM ingress and egress chains. Rules from all SGs attached to a NIC
//! are merged and sorted by priority before generation.
//!
//! Provides atomic apply/remove via `nft -f -` for transactional updates.

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
    /// Host-side interface name (TAP or veth) used for vmap dispatch.
    pub iface_name: String,
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

/// Compute the per-VM egress chain name: `vm_{hash}_out`.
pub fn egress_chain_name(vm_id: &str) -> String {
    format!("vm_{}_out", short_hash(vm_id))
}

/// Compute the named-set name for a security group: `sg_{hash}_ips`.
pub fn sg_set_name(sg_name: &str) -> String {
    format!("sg_{}_ips", short_hash(sg_name))
}

/// Generate an nftables named set definition for a security group.
///
/// Produces a set of IPv4 addresses that can be referenced in rules via
/// `@sg_{hash}_ips`. Used when ingress source or egress destination is
/// a security-group reference.
///
/// Example output:
/// ```text
/// set sg_a1b2c3d4_ips {
///     type ipv4_addr
///     elements = { 10.1.0.5, 10.1.0.6 }
/// }
/// ```
///
/// If `ips` is empty, the set is still generated (with no elements).
pub fn generate_named_set(sg_name: &str, ips: &[String]) -> String {
    let name = sg_set_name(sg_name);
    let mut buf = String::new();
    writeln!(buf, "set {name} {{").unwrap();
    writeln!(buf, "    type ipv4_addr").unwrap();
    if !ips.is_empty() {
        writeln!(buf, "    elements = {{ {} }}", ips.join(", ")).unwrap();
    }
    writeln!(buf, "}}").unwrap();
    buf
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

/// Generate the nftables egress chain rules for a NIC.
///
/// Collects all egress rules from the provided list, sorts by priority
/// (ascending -- lower number evaluated first), translates each to an nft
/// rule statement.
///
/// If no egress rules exist, returns a single `accept` rule (default allow).
/// If egress rules exist, appends an implicit `drop` at the end.
pub fn generate_egress_chain(_nic: &NetworkInterface, rules: &[SecurityGroupRule]) -> Vec<NftRule> {
    let mut egress: Vec<&SecurityGroupRule> = rules
        .iter()
        .filter(|r| r.direction == Direction::Egress)
        .collect();
    egress.sort_by_key(|r| r.priority);

    // No egress rules → default accept (allow all outbound).
    if egress.is_empty() {
        return vec![NftRule {
            text: "accept".to_string(),
        }];
    }

    let mut nft_rules: Vec<NftRule> = Vec::with_capacity(egress.len() + 1);

    for rule in &egress {
        if let Some(text) = translate_egress_rule(rule) {
            nft_rules.push(NftRule { text });
        }
    }

    // Implicit deny at end of chain.
    nft_rules.push(NftRule {
        text: "drop".to_string(),
    });

    nft_rules
}

/// Render a full nftables egress chain definition as a string.
pub fn render_egress_chain(vm_id: &str, rules: &[NftRule]) -> String {
    let chain = egress_chain_name(vm_id);
    let mut buf = String::new();
    writeln!(buf, "chain {chain} {{").unwrap();
    for rule in rules {
        writeln!(buf, "    {}", rule.text).unwrap();
    }
    writeln!(buf, "}}").unwrap();
    buf
}

// ── Atomic apply / remove ─────────────────────────────────────────

/// The nftables table name used for SG chains.
const SG_TABLE: &str = "syfrah_sg";

/// Name of the base forward chain in the SG table.
const SG_FORWARD_CHAIN: &str = "forward";

/// Name of the ingress dispatch chain (physdev oif + oifname -> per-VM ingress chain).
const INGRESS_DISPATCH_CHAIN: &str = "dispatch_ingress";

/// Name of the egress dispatch chain (physdev iif + iifname -> per-VM egress chain).
const EGRESS_DISPATCH_CHAIN: &str = "dispatch_egress";

/// Generate the SG table infrastructure: base forward chain + physdev dispatch chains.
///
/// This must be applied once at startup (or before the first VM). All
/// statements are idempotent (`add table` / `add chain` are no-ops if the
/// object already exists).
///
/// With `br_netfilter` active, bridged VM-to-VM traffic passes through the
/// `forward` hook with `oifname`/`iifname` set to the **bridge** name rather
/// than the individual veth. Using `physdev oifname`/`physdev iifname` matches
/// the real per-VM veth, so SG rules are correctly enforced for same-bridge
/// traffic. The fallback `oifname`/`iifname` lines handle routed (non-bridge)
/// traffic such as VXLAN return packets where physdev is not set.
///
/// Produces:
/// ```text
/// add table inet syfrah_sg
/// add chain inet syfrah_sg forward { type filter hook forward priority 0; policy drop; }
/// add rule inet syfrah_sg forward ct state established,related accept
/// add rule inet syfrah_sg forward ct state invalid drop
/// add rule inet syfrah_sg forward oifname != "lo" goto dispatch_ingress
/// add rule inet syfrah_sg forward iifname != "lo" goto dispatch_egress
/// add chain inet syfrah_sg dispatch_ingress
/// add chain inet syfrah_sg dispatch_egress
/// ```
pub fn build_sg_base_chain() -> String {
    let mut buf = String::new();
    writeln!(buf, "add table inet {SG_TABLE}").unwrap();
    // Dispatch chains must be created before the forward chain rules that
    // reference them via `goto`, otherwise `nft -f -` will reject the rules.
    writeln!(buf, "add chain inet {SG_TABLE} {INGRESS_DISPATCH_CHAIN}").unwrap();
    writeln!(buf, "add chain inet {SG_TABLE} {EGRESS_DISPATCH_CHAIN}").unwrap();
    // Base forward chain with default-drop policy.
    writeln!(
        buf,
        "add chain inet {SG_TABLE} {SG_FORWARD_CHAIN} {{ type filter hook forward priority 0; policy drop; }}"
    )
    .unwrap();
    // Conntrack: allow established/related, drop invalid.
    writeln!(
        buf,
        "add rule inet {SG_TABLE} {SG_FORWARD_CHAIN} ct state established,related accept"
    )
    .unwrap();
    writeln!(
        buf,
        "add rule inet {SG_TABLE} {SG_FORWARD_CHAIN} ct state invalid drop"
    )
    .unwrap();
    // Dispatch to per-VM chains.  Skip loopback (lo) to avoid self-loops.
    writeln!(
        buf,
        r#"add rule inet {SG_TABLE} {SG_FORWARD_CHAIN} oifname != "lo" goto {INGRESS_DISPATCH_CHAIN}"#
    )
    .unwrap();
    writeln!(
        buf,
        r#"add rule inet {SG_TABLE} {SG_FORWARD_CHAIN} iifname != "lo" goto {EGRESS_DISPATCH_CHAIN}"#
    )
    .unwrap();
    buf
}

/// Build a complete nftables ruleset for a VM's security groups.
///
/// The ruleset includes:
/// 1. Table + base forward chain + dispatch vmaps (idempotent)
/// 2. Named sets for any SG references
/// 3. Ingress chain (`vm_{hash}_in`)
/// 4. Egress chain (`vm_{hash}_out`)
/// 5. Vmap entries mapping the NIC's interface to the per-VM chains
///
/// `sg_ip_map` provides the IP addresses for each referenced SG name.
pub fn build_sg_ruleset(
    nic: &NetworkInterface,
    rules: &[SecurityGroupRule],
    sg_ip_map: &std::collections::HashMap<String, Vec<String>>,
) -> String {
    let mut buf = String::new();

    // Idempotent base infrastructure (table, forward chain, vmaps).
    write!(buf, "{}", build_sg_base_chain()).unwrap();

    // Named sets for SG references.
    for (sg_name, ips) in sg_ip_map {
        write!(buf, "{}", generate_named_set_in_table(sg_name, ips)).unwrap();
    }

    // Ingress chain: create (idempotent) then flush then populate.
    let ingress_rules = generate_ingress_chain(nic, rules);
    let in_chain = ingress_chain_name(&nic.vm_id);
    writeln!(buf, "add chain inet {SG_TABLE} {in_chain}").unwrap();
    writeln!(buf, "flush chain inet {SG_TABLE} {in_chain}").unwrap();
    for rule in &ingress_rules {
        writeln!(buf, "add rule inet {SG_TABLE} {in_chain} {}", rule.text).unwrap();
    }

    // Egress chain: create (idempotent) then flush then populate.
    let egress_rules = generate_egress_chain(nic, rules);
    let out_chain = egress_chain_name(&nic.vm_id);
    writeln!(buf, "add chain inet {SG_TABLE} {out_chain}").unwrap();
    writeln!(buf, "flush chain inet {SG_TABLE} {out_chain}").unwrap();
    for rule in &egress_rules {
        writeln!(buf, "add rule inet {SG_TABLE} {out_chain} {}", rule.text).unwrap();
    }

    // Dispatch entries: physdev match (bridged traffic) + iifname/oifname
    // fallback (routed traffic such as VXLAN return packets).
    let iface = &nic.iface_name;
    writeln!(
        buf,
        r#"add rule inet {SG_TABLE} {INGRESS_DISPATCH_CHAIN} physdev oifname "{iface}" jump {in_chain}"#
    )
    .unwrap();
    writeln!(
        buf,
        r#"add rule inet {SG_TABLE} {INGRESS_DISPATCH_CHAIN} oifname "{iface}" jump {in_chain}"#
    )
    .unwrap();
    writeln!(
        buf,
        r#"add rule inet {SG_TABLE} {EGRESS_DISPATCH_CHAIN} physdev iifname "{iface}" jump {out_chain}"#
    )
    .unwrap();
    writeln!(
        buf,
        r#"add rule inet {SG_TABLE} {EGRESS_DISPATCH_CHAIN} iifname "{iface}" jump {out_chain}"#
    )
    .unwrap();

    buf
}

/// Generate a named set definition inside the SG table.
fn generate_named_set_in_table(sg_name: &str, ips: &[String]) -> String {
    let name = sg_set_name(sg_name);
    let mut buf = String::new();
    writeln!(buf, "add set inet {SG_TABLE} {name} {{ type ipv4_addr; }}").unwrap();
    if !ips.is_empty() {
        writeln!(
            buf,
            "add element inet {SG_TABLE} {name} {{ {} }}",
            ips.join(", ")
        )
        .unwrap();
    }
    buf
}

/// Apply SG rules for a VM atomically via `nft -f -`.
///
/// Builds the complete ruleset and pipes it to nft.
pub fn apply_sg_for_vm(
    nic: &NetworkInterface,
    rules: &[SecurityGroupRule],
    sg_ip_map: &std::collections::HashMap<String, Vec<String>>,
) -> std::io::Result<()> {
    let ruleset = build_sg_ruleset(nic, rules, sg_ip_map);
    crate::nft::apply_ruleset(&ruleset)
}

/// Remove all SG chains for a VM, flushing the dispatch chains and
/// deleting the per-VM chains from the SG table.
///
/// Because the dispatch chains contain inline `physdev`/`iifname` rules
/// (not vmap elements), we flush both dispatch chains entirely and rely
/// on the reconciliation loop to re-add the entries for remaining VMs.
/// The per-VM chains are flushed then deleted.
///
/// `iface_name` is accepted for API compatibility but is not used in
/// the removal ruleset — the dispatch chains are flushed wholesale.
pub fn remove_sg_for_vm(vm_id: &str, iface_name: &str) -> std::io::Result<()> {
    let ruleset = build_remove_ruleset(vm_id, iface_name);
    crate::nft::apply_ruleset(&ruleset)
}

/// Build the removal ruleset for a VM (for testing without executing).
///
/// Flushes both dispatch chains (removing all inline dispatch rules),
/// then flushes and deletes the per-VM ingress/egress chains.
/// The `iface_name` parameter is accepted for API compatibility.
pub fn build_remove_ruleset(vm_id: &str, _iface_name: &str) -> String {
    let in_chain = ingress_chain_name(vm_id);
    let out_chain = egress_chain_name(vm_id);
    let mut buf = String::new();
    // Flush dispatch chains so inline physdev rules for this VM are gone.
    // The reconciliation loop re-adds entries for surviving VMs.
    writeln!(buf, "flush chain inet {SG_TABLE} {INGRESS_DISPATCH_CHAIN}").unwrap();
    writeln!(buf, "flush chain inet {SG_TABLE} {EGRESS_DISPATCH_CHAIN}").unwrap();
    // Flush then delete per-VM chains — idempotent-safe under `nft -f`.
    writeln!(buf, "flush chain inet {SG_TABLE} {in_chain}").unwrap();
    writeln!(buf, "delete chain inet {SG_TABLE} {in_chain}").unwrap();
    writeln!(buf, "flush chain inet {SG_TABLE} {out_chain}").unwrap();
    writeln!(buf, "delete chain inet {SG_TABLE} {out_chain}").unwrap();
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

/// Translate a single egress `SecurityGroupRule` into an nft rule statement.
///
/// For egress rules the `source` field is re-interpreted as **destination**:
/// - `Cidr` → `ip daddr <cidr>`
/// - `SecurityGroup` → `ip daddr @sg_{hash}_ips`
fn translate_egress_rule(rule: &SecurityGroupRule) -> Option<String> {
    if rule.direction != Direction::Egress {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();

    // Destination filter (source field re-interpreted for egress).
    match &rule.source {
        TrafficSource::Cidr(cidr) if cidr != "0.0.0.0/0" => {
            parts.push(format!("ip daddr {cidr}"));
        }
        TrafficSource::SecurityGroup(sg_name) => {
            let set_name = format!("sg_{}_ips", short_hash(sg_name));
            parts.push(format!("ip daddr @{set_name}"));
        }
        _ => {}
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
        Protocol::All => {}
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
            iface_name: "syft-abcd1234".to_string(),
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

    // ── Egress tests ──────────────────────────────────────────────

    fn egress_rule(
        protocol: Protocol,
        port_range: Option<PortRange>,
        source: TrafficSource,
        priority: u32,
    ) -> SecurityGroupRule {
        SecurityGroupRule {
            id: RuleId(format!("rule-e-{priority}")),
            sg_id: SecurityGroupId("sg-default".to_string()),
            direction: Direction::Egress,
            protocol,
            port_range,
            source,
            priority,
            description: String::new(),
            created_at: 0,
        }
    }

    #[test]
    fn test_egress_no_rules_default_accept() {
        let nic = test_nic();
        let chain = generate_egress_chain(&nic, &[]);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].text, "accept");
    }

    #[test]
    fn test_egress_tcp_with_implicit_drop() {
        let nic = test_nic();
        let rules = vec![egress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 443, to: 443 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].text, "tcp dport 443 accept");
        assert_eq!(chain[1].text, "drop");
    }

    #[test]
    fn test_egress_cidr_destination() {
        let nic = test_nic();
        let rules = vec![egress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 80, to: 80 }),
            TrafficSource::Cidr("10.0.0.0/8".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "ip daddr 10.0.0.0/8 tcp dport 80 accept");
    }

    #[test]
    fn test_egress_sg_destination() {
        let nic = test_nic();
        let rules = vec![egress_rule(
            Protocol::Tcp,
            Some(PortRange {
                from: 5432,
                to: 5432,
            }),
            TrafficSource::SecurityGroup("db-sg".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        let expected_set = format!("sg_{}_ips", short_hash("db-sg"));
        assert_eq!(
            chain[0].text,
            format!("ip daddr @{expected_set} tcp dport 5432 accept")
        );
    }

    #[test]
    fn test_egress_priority_sorting() {
        let nic = test_nic();
        let rules = vec![
            egress_rule(
                Protocol::Tcp,
                Some(PortRange { from: 443, to: 443 }),
                TrafficSource::Cidr("0.0.0.0/0".to_string()),
                200,
            ),
            egress_rule(
                Protocol::Tcp,
                Some(PortRange { from: 80, to: 80 }),
                TrafficSource::Cidr("0.0.0.0/0".to_string()),
                100,
            ),
        ];
        let chain = generate_egress_chain(&nic, &rules);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].text, "tcp dport 80 accept");
        assert_eq!(chain[1].text, "tcp dport 443 accept");
        assert_eq!(chain[2].text, "drop");
    }

    #[test]
    fn test_egress_ingress_rules_filtered_out() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        // No egress rules → default accept.
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].text, "accept");
    }

    #[test]
    fn test_egress_chain_name_deterministic() {
        assert_eq!(egress_chain_name("vm-1"), egress_chain_name("vm-1"));
        assert_ne!(egress_chain_name("vm-1"), egress_chain_name("vm-2"));
    }

    #[test]
    fn test_render_egress_chain() {
        let rules = vec![
            NftRule {
                text: "tcp dport 443 accept".to_string(),
            },
            NftRule {
                text: "drop".to_string(),
            },
        ];
        let rendered = render_egress_chain("vm-1", &rules);
        let chain_name = egress_chain_name("vm-1");
        assert!(rendered.contains(&format!("chain {chain_name} {{")));
        assert!(rendered.contains("    tcp dport 443 accept"));
        assert!(rendered.contains("    drop"));
    }

    #[test]
    fn test_egress_udp_range() {
        let nic = test_nic();
        let rules = vec![egress_rule(
            Protocol::Udp,
            Some(PortRange {
                from: 8000,
                to: 9000,
            }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "udp dport 8000-9000 accept");
    }

    #[test]
    fn test_egress_icmp() {
        let nic = test_nic();
        let rules = vec![egress_rule(
            Protocol::Icmp,
            None,
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "icmp type echo-request accept");
    }

    #[test]
    fn test_egress_protocol_all() {
        let nic = test_nic();
        let rules = vec![egress_rule(
            Protocol::All,
            None,
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let chain = generate_egress_chain(&nic, &rules);
        assert_eq!(chain[0].text, "accept");
        assert_eq!(chain[1].text, "drop");
    }

    // ── Named set tests ───────────────────────────────────────────

    #[test]
    fn test_named_set_with_ips() {
        let output =
            generate_named_set("web-sg", &["10.1.0.5".to_string(), "10.1.0.6".to_string()]);
        let set_name = sg_set_name("web-sg");
        assert!(output.contains(&format!("set {set_name} {{")));
        assert!(output.contains("type ipv4_addr"));
        assert!(output.contains("elements = { 10.1.0.5, 10.1.0.6 }"));
    }

    #[test]
    fn test_named_set_empty_ips() {
        let output = generate_named_set("empty-sg", &[]);
        let set_name = sg_set_name("empty-sg");
        assert!(output.contains(&format!("set {set_name} {{")));
        assert!(output.contains("type ipv4_addr"));
        assert!(!output.contains("elements"));
    }

    #[test]
    fn test_named_set_single_ip() {
        let output = generate_named_set("single-sg", &["192.168.1.1".to_string()]);
        assert!(output.contains("elements = { 192.168.1.1 }"));
    }

    #[test]
    fn test_sg_set_name_deterministic() {
        assert_eq!(sg_set_name("web-sg"), sg_set_name("web-sg"));
        assert_ne!(sg_set_name("web-sg"), sg_set_name("db-sg"));
    }

    #[test]
    fn test_sg_set_name_matches_chain_refs() {
        // The set name generated by sg_set_name must match what
        // translate_rule produces for SG-ref sources.
        let set_name = sg_set_name("web-sg");
        let expected = format!("sg_{}_ips", short_hash("web-sg"));
        assert_eq!(set_name, expected);
    }

    // ── Atomic apply/remove tests ─────────────────────────────────

    #[test]
    fn test_build_sg_ruleset_contains_table() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let sg_map = std::collections::HashMap::new();
        let ruleset = build_sg_ruleset(&nic, &rules, &sg_map);
        assert!(ruleset.contains("add table inet syfrah_sg"));
    }

    #[test]
    fn test_build_sg_ruleset_contains_chains() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let sg_map = std::collections::HashMap::new();
        let ruleset = build_sg_ruleset(&nic, &rules, &sg_map);
        let in_chain = ingress_chain_name(&nic.vm_id);
        let out_chain = egress_chain_name(&nic.vm_id);
        assert!(ruleset.contains(&format!("add chain inet syfrah_sg {in_chain}")));
        assert!(ruleset.contains(&format!("add chain inet syfrah_sg {out_chain}")));
    }

    #[test]
    fn test_build_sg_ruleset_with_sg_ref() {
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
        let mut sg_map = std::collections::HashMap::new();
        sg_map.insert(
            "web-sg".to_string(),
            vec!["10.1.0.5".to_string(), "10.1.0.6".to_string()],
        );
        let ruleset = build_sg_ruleset(&nic, &rules, &sg_map);
        let set_name = sg_set_name("web-sg");
        assert!(ruleset.contains(&format!("add set inet syfrah_sg {set_name}")));
        assert!(ruleset.contains("10.1.0.5, 10.1.0.6"));
    }

    #[test]
    fn test_build_sg_ruleset_ingress_and_egress() {
        let nic = test_nic();
        let rules = vec![
            ingress_rule(
                Protocol::Tcp,
                Some(PortRange { from: 22, to: 22 }),
                TrafficSource::Cidr("0.0.0.0/0".to_string()),
                100,
            ),
            egress_rule(
                Protocol::Tcp,
                Some(PortRange { from: 443, to: 443 }),
                TrafficSource::Cidr("0.0.0.0/0".to_string()),
                100,
            ),
        ];
        let sg_map = std::collections::HashMap::new();
        let ruleset = build_sg_ruleset(&nic, &rules, &sg_map);
        // Ingress: TCP 22 + drop.
        assert!(ruleset.contains("tcp dport 22 accept"));
        assert!(ruleset.contains("drop"));
        // Egress: TCP 443 + drop (since rules exist).
        assert!(ruleset.contains("tcp dport 443 accept"));
    }

    #[test]
    fn test_build_remove_ruleset() {
        let ruleset = build_remove_ruleset("vm-1", "syft-abcd1234");
        let in_chain = ingress_chain_name("vm-1");
        let out_chain = egress_chain_name("vm-1");
        // Dispatch chains must be flushed before per-VM chains are deleted.
        assert!(ruleset.contains("flush chain inet syfrah_sg dispatch_ingress"));
        assert!(ruleset.contains("flush chain inet syfrah_sg dispatch_egress"));
        assert!(ruleset.contains(&format!("flush chain inet syfrah_sg {in_chain}")));
        assert!(ruleset.contains(&format!("delete chain inet syfrah_sg {in_chain}")));
        assert!(ruleset.contains(&format!("flush chain inet syfrah_sg {out_chain}")));
        assert!(ruleset.contains(&format!("delete chain inet syfrah_sg {out_chain}")));
        // Old vmap-style element deletions must not be present.
        assert!(!ruleset.contains("delete element inet syfrah_sg ingress_dispatch"));
        assert!(!ruleset.contains("delete element inet syfrah_sg egress_dispatch"));
    }

    // ── Base chain + physdev dispatch tests ──────────────────────────

    #[test]
    fn test_build_sg_base_chain() {
        let base = build_sg_base_chain();
        assert!(base.contains("add table inet syfrah_sg"));
        assert!(base.contains("add chain inet syfrah_sg forward { type filter hook forward priority 0; policy drop; }"));
        assert!(base.contains("ct state established,related accept"));
        assert!(base.contains("ct state invalid drop"));
        // Physdev dispatch chains replace old vmaps.
        assert!(base.contains("goto dispatch_ingress"));
        assert!(base.contains("goto dispatch_egress"));
        assert!(base.contains("add chain inet syfrah_sg dispatch_ingress"));
        assert!(base.contains("add chain inet syfrah_sg dispatch_egress"));
        // Old vmap-style dispatch must not be present.
        assert!(!base.contains("add map inet syfrah_sg ingress_dispatch"));
        assert!(!base.contains("add map inet syfrah_sg egress_dispatch"));
    }

    #[test]
    fn test_build_sg_ruleset_includes_base_chain() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let sg_map = std::collections::HashMap::new();
        let ruleset = build_sg_ruleset(&nic, &rules, &sg_map);
        // Must include the base forward chain with hook.
        assert!(ruleset.contains("type filter hook forward priority 0; policy drop;"));
        // Must include physdev dispatch chains.
        assert!(ruleset.contains("add chain inet syfrah_sg dispatch_ingress"));
        assert!(ruleset.contains("add chain inet syfrah_sg dispatch_egress"));
    }

    #[test]
    fn test_build_sg_ruleset_includes_physdev_dispatch_entries() {
        let nic = test_nic();
        let rules = vec![ingress_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let sg_map = std::collections::HashMap::new();
        let ruleset = build_sg_ruleset(&nic, &rules, &sg_map);
        let in_chain = ingress_chain_name(&nic.vm_id);
        let out_chain = egress_chain_name(&nic.vm_id);
        // Physdev (bridged) entries.
        assert!(ruleset.contains(&format!(
            r#"add rule inet syfrah_sg dispatch_ingress physdev oifname "syft-abcd1234" jump {in_chain}"#
        )));
        assert!(ruleset.contains(&format!(
            r#"add rule inet syfrah_sg dispatch_egress physdev iifname "syft-abcd1234" jump {out_chain}"#
        )));
        // Fallback (routed) entries.
        assert!(ruleset.contains(&format!(
            r#"add rule inet syfrah_sg dispatch_ingress oifname "syft-abcd1234" jump {in_chain}"#
        )));
        assert!(ruleset.contains(&format!(
            r#"add rule inet syfrah_sg dispatch_egress iifname "syft-abcd1234" jump {out_chain}"#
        )));
    }

    #[test]
    fn test_dispatch_chains_flushed_before_vm_chains() {
        let ruleset = build_remove_ruleset("vm-1", "syft-abcd1234");
        let in_chain = ingress_chain_name("vm-1");
        // The dispatch chain flush must precede the per-VM chain flush.
        let dispatch_pos = ruleset
            .find("flush chain inet syfrah_sg dispatch_ingress")
            .unwrap();
        let vm_chain_pos = ruleset
            .find(&format!("flush chain inet syfrah_sg {in_chain}"))
            .unwrap();
        assert!(
            dispatch_pos < vm_chain_pos,
            "dispatch chains must be flushed before per-VM chains are deleted"
        );
    }
}
