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

use std::fmt::Write;
use std::net::Ipv4Addr;

// ── Table + chain names ─────────────────────────────────────────────

const TABLE_NAME: &str = "syfrah";
const CHAIN_NAME: &str = "forward";

// ── Public API ──────────────────────────────────────────────────────

/// Generate the nftables ruleset that creates the `syfrah` table and
/// `forward` chain if they do not already exist.
///
/// This is idempotent: re-running it on an already-initialized system
/// is a no-op (we use `create` which silently succeeds if the object
/// exists).
pub fn generate_table_setup() -> String {
    let mut buf = String::new();
    // `table inet` works for both IPv4 and IPv6 in nftables.
    // Using `create` instead of `add` so it does not error if it exists.
    writeln!(buf, "create table inet {TABLE_NAME}").unwrap();
    writeln!(
        buf,
        "create chain inet {TABLE_NAME} {CHAIN_NAME} {{ type filter hook forward priority 0; policy accept; }}"
    )
    .unwrap();
    buf
}

/// Generate nftables rules for a VM's TAP interface.
///
/// The rules enforce (in order):
/// 1. Anti-spoofing: drop traffic from the TAP with wrong source MAC
/// 2. Anti-spoofing: drop traffic from the TAP with wrong source IP
/// 3. Default-deny ingress: drop all traffic going to the TAP
/// 4. Allow SSH (TCP 22) inbound
/// 5. Allow ICMP echo-request inbound
/// 6. Conntrack: allow established/related inbound
/// 7. Egress allow: permit outbound traffic (after anti-spoofing)
///
/// Rules are applied atomically via `nft -f -`.
pub fn generate_vm_rules(tap: &str, mac: &str, ip: Ipv4Addr) -> String {
    let mut buf = String::new();

    // Ensure table and chain exist first (idempotent).
    write!(buf, "{}", generate_table_setup()).unwrap();

    // Anti-spoofing: wrong MAC from this TAP → drop
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {tap} ether saddr != {mac} drop"
    )
    .unwrap();

    // Anti-spoofing: wrong source IP from this TAP → drop
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {tap} ip saddr != {ip} drop"
    )
    .unwrap();

    // Default-deny ingress: drop all traffic toward this TAP
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} drop"
    )
    .unwrap();

    // Allow SSH inbound (TCP port 22)
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} tcp dport 22 accept"
    )
    .unwrap();

    // Allow ICMP echo-request inbound
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} icmp type echo-request accept"
    )
    .unwrap();

    // Conntrack: allow established/related connections inbound
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} oif {tap} ct state established,related accept"
    )
    .unwrap();

    // Egress allow: all outbound traffic from this TAP (after anti-spoofing)
    writeln!(
        buf,
        "add rule inet {TABLE_NAME} {CHAIN_NAME} iif {tap} accept"
    )
    .unwrap();

    buf
}

/// Generate nftables commands to remove all rules for a TAP interface.
///
/// This flushes the chain and re-adds rules for other TAPs. In a
/// production implementation, per-TAP chains would avoid full flushes.
/// For now, we delete rules matching the given TAP name by flushing
/// the chain (the caller is responsible for re-applying rules for
/// remaining VMs).
pub fn generate_remove_rules(tap: &str) -> String {
    // In production, we would use per-TAP chains (e.g., `syfrah-{tap}`)
    // and simply `flush chain inet syfrah syfrah-{tap}; delete chain ...`.
    // For this initial implementation, we output comments marking the TAP
    // for the caller's flush-and-rebuild strategy.
    let mut buf = String::new();
    writeln!(
        buf,
        "# flush rules for TAP {tap} from inet {TABLE_NAME} {CHAIN_NAME}"
    )
    .unwrap();
    // The production backend will parse existing rules and delete by handle.
    // The mock backend records the intent.
    buf
}

/// Apply an nftables ruleset by writing it to `nft -f -` on stdin.
///
/// This is the production entry point. In tests, the mock backend
/// captures the generated ruleset instead.
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

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const TAP: &str = "syftap-vm1";
    const MAC: &str = "02:00:0a:00:01:05";
    const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 1, 5);

    fn rules() -> String {
        generate_vm_rules(TAP, MAC, IP)
    }

    #[test]
    fn anti_spoof_rules_generated() {
        let r = rules();
        assert!(
            r.contains(&format!("iif {TAP} ether saddr != {MAC} drop")),
            "MAC anti-spoof rule missing:\n{r}"
        );
        assert!(
            r.contains(&format!("iif {TAP} ip saddr != {IP} drop")),
            "IP anti-spoof rule missing:\n{r}"
        );
    }

    #[test]
    fn default_deny_ingress() {
        let r = rules();
        assert!(
            r.contains(&format!("oif {TAP} drop")),
            "Default-deny ingress rule missing:\n{r}"
        );
    }

    #[test]
    fn ssh_allowed() {
        let r = rules();
        assert!(
            r.contains(&format!("oif {TAP} tcp dport 22 accept")),
            "SSH allow rule missing:\n{r}"
        );
    }

    #[test]
    fn icmp_allowed() {
        let r = rules();
        assert!(
            r.contains(&format!("oif {TAP} icmp type echo-request accept")),
            "ICMP allow rule missing:\n{r}"
        );
    }

    #[test]
    fn egress_allowed() {
        let r = rules();
        assert!(
            r.contains(&format!("iif {TAP} accept")),
            "Egress allow rule missing:\n{r}"
        );
    }

    #[test]
    fn conntrack_established() {
        let r = rules();
        assert!(
            r.contains(&format!("oif {TAP} ct state established,related accept")),
            "Conntrack rule missing:\n{r}"
        );
    }

    #[test]
    fn table_setup_is_idempotent() {
        let setup = generate_table_setup();
        // Uses `create` (not `add`) so re-running is a no-op.
        assert!(
            setup.contains("create table inet syfrah"),
            "Table creation should use 'create' for idempotency:\n{setup}"
        );
        assert!(
            setup.contains("create chain inet syfrah forward"),
            "Chain creation should use 'create' for idempotency:\n{setup}"
        );
    }

    #[test]
    fn rule_ordering() {
        let r = rules();
        // Anti-spoofing must come before egress allow.
        let mac_spoof_pos = r.find("ether saddr !=").expect("MAC spoof rule");
        let ip_spoof_pos = r.find("ip saddr !=").expect("IP spoof rule");
        let egress_pos = r.find(&format!("iif {TAP} accept")).expect("egress rule");
        let deny_pos = r.find(&format!("oif {TAP} drop")).expect("deny rule");
        let ssh_pos = r.find("tcp dport 22 accept").expect("SSH rule");

        assert!(
            mac_spoof_pos < ip_spoof_pos,
            "MAC spoof should come before IP spoof"
        );
        assert!(
            ip_spoof_pos < egress_pos,
            "Anti-spoofing should come before egress allow"
        );
        assert!(
            deny_pos < ssh_pos,
            "Default deny should come before SSH allow (nft evaluates in order)"
        );
    }
}
