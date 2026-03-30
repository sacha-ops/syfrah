//! nftables SNAT masquerade helpers.
//!
//! Applies and removes masquerade rules so that VMs in a subnet can reach the
//! internet through the host's public IP.
//!
//! Rule structure:
//! ```text
//! table ip syfrah_nat {
//!     chain postrouting {
//!         type nat hook postrouting priority 100; policy accept;
//!         oif != "syfbr-100" ip saddr 10.1.1.0/24 masquerade
//!     }
//! }
//! ```

use std::process::Command;

use ipnet::Ipv4Net;

use crate::backend::{BackendError, Result};

/// Table name used for all Syfrah NAT rules.
const TABLE: &str = "syfrah_nat";

/// Chain name within the NAT table.
const CHAIN: &str = "postrouting";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Ensure the syfrah_nat table and postrouting chain exist, then add a
/// masquerade rule for outbound traffic from `subnet` exiting via any
/// interface other than `bridge`.
///
/// Idempotent: if the rule already exists, nftables silently succeeds.
pub fn apply_nat(bridge: &str, subnet: Ipv4Net) -> Result<()> {
    ensure_table()?;
    ensure_chain()?;
    add_masquerade_rule(bridge, subnet)?;
    Ok(())
}

/// Remove the masquerade rule for `bridge`/`subnet`.
///
/// If the rule does not exist the function still succeeds (idempotent).
/// We list all handles in the chain, find the matching rule, and delete it.
pub fn remove_nat(bridge: &str, subnet: Ipv4Net) -> Result<()> {
    if let Some(handle) = find_rule_handle(bridge, subnet)? {
        nft_run(&[
            "delete",
            "rule",
            "ip",
            TABLE,
            CHAIN,
            "handle",
            &handle.to_string(),
        ])?;
    }
    Ok(())
}

/// Build the nftables rule expression for a masquerade rule.
///
/// Exposed for testing so callers can verify the generated rule text without
/// running real nftables commands.
pub fn masquerade_rule_expr(bridge: &str, subnet: Ipv4Net) -> String {
    format!("oif != \"{bridge}\" ip saddr {subnet} masquerade")
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn ensure_table() -> Result<()> {
    // `add table` is idempotent in nftables.
    nft_run(&["add", "table", "ip", TABLE])
}

fn ensure_chain() -> Result<()> {
    // `add chain` with full spec is idempotent.
    nft_run_raw(&format!(
        "add chain ip {TABLE} {CHAIN} {{ type nat hook postrouting priority 100; policy accept; }}"
    ))
}

fn add_masquerade_rule(bridge: &str, subnet: Ipv4Net) -> Result<()> {
    let expr = masquerade_rule_expr(bridge, subnet);
    nft_run_raw(&format!("add rule ip {TABLE} {CHAIN} {expr}"))
}

/// Find the nftables handle of a masquerade rule matching `bridge` and `subnet`.
fn find_rule_handle(bridge: &str, subnet: Ipv4Net) -> Result<Option<u64>> {
    let output = Command::new("nft")
        .args(["-a", "list", "chain", "ip", TABLE, CHAIN])
        .output()
        .map_err(BackendError::Io)?;

    if !output.status.success() {
        // Chain/table doesn't exist — nothing to remove.
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let needle_bridge = format!("oif != \"{bridge}\"");
    let needle_subnet = subnet.to_string();

    for line in stdout.lines() {
        if line.contains(&needle_bridge)
            && line.contains(&needle_subnet)
            && line.contains("masquerade")
        {
            // Lines look like: `... masquerade # handle 4`
            if let Some(pos) = line.rfind("# handle ") {
                let handle_str = line[pos + 9..].trim();
                if let Ok(h) = handle_str.parse::<u64>() {
                    return Ok(Some(h));
                }
            }
        }
    }

    Ok(None)
}

/// Run `nft <args>`.
fn nft_run(args: &[&str]) -> Result<()> {
    let output = Command::new("nft")
        .args(args)
        .output()
        .map_err(BackendError::Io)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackendError::CommandFailed(format!(
            "nft {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(())
}

/// Run `nft` with a single command string (required for chain specs with braces).
fn nft_run_raw(cmd: &str) -> Result<()> {
    let output = Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(cmd.as_bytes())?;
            }
            child.wait_with_output()
        })
        .map_err(BackendError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackendError::CommandFailed(format!(
            "nft command failed: {stderr}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masquerade_rule_expr_contains_subnet_cidr() {
        let subnet: Ipv4Net = "10.1.1.0/24".parse().unwrap();
        let expr = masquerade_rule_expr("syfbr-100", subnet);
        assert!(
            expr.contains("10.1.1.0/24"),
            "rule expression must include the subnet CIDR"
        );
        assert!(
            expr.contains("masquerade"),
            "rule expression must include masquerade"
        );
        assert!(
            expr.contains("oif != \"syfbr-100\""),
            "rule expression must exclude the bridge interface"
        );
    }

    #[test]
    fn masquerade_rule_per_bridge() {
        let subnet_a: Ipv4Net = "10.1.1.0/24".parse().unwrap();
        let subnet_b: Ipv4Net = "10.2.1.0/24".parse().unwrap();

        let expr_a = masquerade_rule_expr("syfbr-100", subnet_a);
        let expr_b = masquerade_rule_expr("syfbr-200", subnet_b);

        assert_ne!(
            expr_a, expr_b,
            "different bridges must produce different rules"
        );
        assert!(expr_a.contains("syfbr-100"));
        assert!(expr_b.contains("syfbr-200"));
        assert!(expr_a.contains("10.1.1.0/24"));
        assert!(expr_b.contains("10.2.1.0/24"));
    }
}
