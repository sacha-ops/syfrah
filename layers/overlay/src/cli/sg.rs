//! `syfrah sg add-rule|remove-rule|rules` handlers.
//!
//! All operations go through the daemon's control socket. The CLI
//! validates arguments locally and sends structured requests to the
//! daemon which owns the database exclusively.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Subcommand;

use crate::sg::{Direction, PortRange, Protocol, SecurityGroupRule, TrafficSource};

/// Top-level security group CLI command.
#[derive(Debug, Subcommand)]
pub enum SgCommand {
    /// Add a firewall rule to a security group
    #[command(
        name = "add-rule",
        after_help = "Examples:\n  \
            syfrah sg add-rule web-sg --direction ingress --protocol tcp --port 443 --source 0.0.0.0/0\n  \
            syfrah sg add-rule db-sg --direction ingress --protocol tcp --port 5432 --source-sg web-sg\n  \
            syfrah sg add-rule app-sg --direction egress --protocol tcp --port 8000-9000 --source 10.0.0.0/8"
    )]
    AddRule {
        /// Security group name
        sg: String,
        /// Rule direction: ingress or egress
        #[arg(long)]
        direction: String,
        /// Protocol: tcp, udp, icmp, or all
        #[arg(long)]
        protocol: String,
        /// Port number (e.g. 443) or range (e.g. 8000-9000)
        #[arg(long)]
        port: Option<String>,
        /// Source/destination as CIDR (e.g. 0.0.0.0/0)
        #[arg(long, conflicts_with = "source_sg")]
        source: Option<String>,
        /// Source/destination as security group name
        #[arg(long, conflicts_with = "source")]
        source_sg: Option<String>,
        /// Rule description
        #[arg(long)]
        description: Option<String>,
        /// Priority (lower = evaluated first, default: 100)
        #[arg(long)]
        priority: Option<u32>,
    },
    /// Remove a rule from a security group
    #[command(
        name = "remove-rule",
        after_help = "Examples:\n  syfrah sg remove-rule web-sg --rule-id rule-abc123"
    )]
    RemoveRule {
        /// Security group name
        sg: String,
        /// Rule ID to remove
        #[arg(long)]
        rule_id: String,
    },
    /// List rules in a security group
    #[command(
        name = "rules",
        after_help = "Examples:\n  syfrah sg rules web-sg\n  syfrah sg rules web-sg --json"
    )]
    Rules {
        /// Security group name
        sg: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Check if traffic would be allowed or denied by a VM's security groups
    #[command(
        name = "check",
        after_help = "Examples:\n  \
            syfrah sg check --vm web-1 --port 443 --protocol tcp\n  \
            syfrah sg check --vm web-1 --port 22 --protocol tcp --source 10.0.0.1"
    )]
    Check {
        /// VM name to evaluate
        #[arg(long)]
        vm: String,
        /// Port to check
        #[arg(long)]
        port: u16,
        /// Protocol: tcp, udp, icmp
        #[arg(long, default_value = "tcp")]
        protocol: String,
        /// Source IP address to check (default: 0.0.0.0 = any)
        #[arg(long)]
        source: Option<String>,
    },
}

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

fn daemon_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon — is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

/// Validate and parse the add-rule arguments.
pub fn parse_add_rule_args(
    direction: &str,
    protocol: &str,
    port: Option<&str>,
    source: Option<&str>,
    source_sg: Option<&str>,
) -> Result<(Direction, Protocol, Option<PortRange>, TrafficSource)> {
    let dir: Direction = direction
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    let proto: Protocol = protocol
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    // Validate port: protocol must support ports
    let port_range = match port {
        Some(p) => {
            match proto {
                Protocol::Icmp => {
                    bail!("--port is not valid with protocol 'icmp'");
                }
                Protocol::All => {
                    bail!("--port is not valid with protocol 'all'");
                }
                Protocol::Tcp | Protocol::Udp => {}
            }
            let pr: PortRange = p.parse().map_err(|e: String| anyhow::anyhow!("{e}"))?;
            Some(pr)
        }
        None => None,
    };

    // Resolve source
    let traffic_source = match (source, source_sg) {
        (Some(cidr), None) => cidr
            .parse::<TrafficSource>()
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        (None, Some(sg_name)) => TrafficSource::SecurityGroup(sg_name.to_string()),
        (None, None) => {
            // Default to 0.0.0.0/0
            TrafficSource::Cidr("0.0.0.0/0".to_string())
        }
        (Some(_), Some(_)) => {
            bail!("cannot specify both --source and --source-sg");
        }
    };

    Ok((dir, proto, port_range, traffic_source))
}

/// Execute a security group CLI command.
pub async fn run(cmd: SgCommand) -> Result<()> {
    match cmd {
        SgCommand::AddRule {
            sg,
            direction,
            protocol,
            port,
            source,
            source_sg,
            description,
            priority,
        } => {
            run_add_rule(
                &sg,
                &direction,
                &protocol,
                port.as_deref(),
                source.as_deref(),
                source_sg.as_deref(),
                description.as_deref(),
                priority,
            )
            .await
        }
        SgCommand::RemoveRule { sg, rule_id } => run_remove_rule(&sg, &rule_id).await,
        SgCommand::Rules { sg, json } => run_rules(&sg, json).await,
        SgCommand::Check {
            vm,
            port,
            protocol,
            source,
        } => run_check(&vm, port, &protocol, source.as_deref()).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_add_rule(
    sg: &str,
    direction: &str,
    protocol: &str,
    port: Option<&str>,
    source: Option<&str>,
    source_sg: Option<&str>,
    description: Option<&str>,
    priority: Option<u32>,
) -> Result<()> {
    let (dir, proto, port_range, traffic_source) =
        parse_add_rule_args(direction, protocol, port, source, source_sg)?;

    let priority = priority.unwrap_or(100);
    let description = description.unwrap_or("").to_string();

    let req = serde_json::json!({
        "type": "sg_add_rule",
        "sg": sg,
        "direction": dir,
        "protocol": proto,
        "port_range": port_range,
        "source": traffic_source,
        "priority": priority,
        "description": description,
    });

    let socket = control_socket_path();
    let resp = send_overlay_request(&socket, &req)
        .await
        .map_err(daemon_err)?;

    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    let rule_id = resp
        .get("rule_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    println!("Rule added to security group '{sg}':");
    println!("  Rule ID:     {rule_id}");
    println!("  Direction:   {dir}");
    println!("  Protocol:    {proto}");
    if let Some(ref pr) = port_range {
        println!("  Ports:       {pr}");
    } else {
        println!("  Ports:       -");
    }
    println!("  Source:      {traffic_source}");
    println!("  Priority:    {priority}");
    if !description.is_empty() {
        println!("  Description: {description}");
    }

    Ok(())
}

async fn run_remove_rule(sg: &str, rule_id: &str) -> Result<()> {
    let req = serde_json::json!({
        "type": "sg_remove_rule",
        "sg": sg,
        "rule_id": rule_id,
    });

    let socket = control_socket_path();
    let resp = send_overlay_request(&socket, &req)
        .await
        .map_err(daemon_err)?;

    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    println!("Rule '{rule_id}' removed from security group '{sg}'.");
    Ok(())
}

async fn run_rules(sg: &str, json: bool) -> Result<()> {
    let req = serde_json::json!({
        "type": "sg_rules",
        "sg": sg,
    });

    let socket = control_socket_path();
    let resp = send_overlay_request(&socket, &req)
        .await
        .map_err(daemon_err)?;

    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    let rules: Vec<SecurityGroupRule> = serde_json::from_value(
        resp.get("rules")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![])),
    )
    .unwrap_or_default();

    if json {
        println!("{}", serde_json::to_string_pretty(&rules)?);
        return Ok(());
    }

    if rules.is_empty() {
        println!("No rules in security group '{sg}'.");
        return Ok(());
    }

    // Table output
    print_rules_table(sg, &rules);
    Ok(())
}

/// Print rules in a formatted table.
fn print_rules_table(sg: &str, rules: &[SecurityGroupRule]) {
    println!("Rules for security group '{sg}':");
    println!();

    // Column headers
    let headers = [
        "ID",
        "DIRECTION",
        "PROTOCOL",
        "PORTS",
        "SOURCE",
        "DESCRIPTION",
    ];

    // Compute column widths
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();

    let rows: Vec<Vec<String>> = rules
        .iter()
        .map(|r| {
            vec![
                r.id.0.clone(),
                r.direction.to_string(),
                r.protocol.to_string(),
                r.port_range
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                r.source.to_string(),
                r.description.clone(),
            ]
        })
        .collect();

    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // Print header
    let header_line: String = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  {header_line}");

    // Print rows
    for row in &rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{:width$}", cell, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ");
        println!("  {line}");
    }
}

async fn run_check(vm: &str, port: u16, protocol: &str, source: Option<&str>) -> Result<()> {
    let proto: Protocol = protocol
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    let source_ip = source.unwrap_or("0.0.0.0");

    // Query the daemon for the VM's security group rules.
    let req = serde_json::json!({
        "type": "sg_check",
        "vm": vm,
        "port": port,
        "protocol": proto,
        "source": source_ip,
    });

    let socket = control_socket_path();
    let resp = send_overlay_request(&socket, &req)
        .await
        .map_err(daemon_err)?;

    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        bail!("{err}");
    }

    let verdict = resp
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    let reason = resp
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("no reason provided");

    println!("{verdict}: {reason}");
    Ok(())
}

/// Evaluate security group rules to determine if traffic is allowed.
///
/// Returns `(verdict, reason)` where verdict is "ALLOWED" or "DENIED".
/// This is a pure function for testability -- no daemon required.
pub fn evaluate_rules(
    rules: &[SecurityGroupRule],
    port: u16,
    protocol: Protocol,
    source_ip: &str,
) -> (String, String) {
    // Filter to ingress rules only, sort by priority.
    let mut ingress: Vec<&SecurityGroupRule> = rules
        .iter()
        .filter(|r| r.direction == Direction::Ingress)
        .collect();
    ingress.sort_by_key(|r| r.priority);

    for rule in &ingress {
        if rule_matches(rule, port, protocol, source_ip) {
            let reason = format!(
                "rule {} (priority {}, {} port {} from {})",
                rule.id, rule.priority, rule.protocol, port, rule.source
            );
            return ("ALLOWED".to_string(), reason);
        }
    }

    ("DENIED".to_string(), "no matching ingress rule".to_string())
}

/// Check if a single rule matches the given traffic.
fn rule_matches(rule: &SecurityGroupRule, port: u16, protocol: Protocol, source_ip: &str) -> bool {
    // Protocol match.
    if rule.protocol != Protocol::All && rule.protocol != protocol {
        return false;
    }

    // Port match.
    if let Some(ref pr) = rule.port_range {
        if port < pr.from || port > pr.to {
            return false;
        }
    }
    // No port_range = all ports match.

    // Source match.
    match &rule.source {
        TrafficSource::Cidr(cidr) if cidr == "0.0.0.0/0" => {
            // Matches any source.
        }
        TrafficSource::Cidr(cidr) => {
            if !cidr_contains(cidr, source_ip) {
                return false;
            }
        }
        TrafficSource::SecurityGroup(_) => {
            // SG-ref source requires IP membership check -- for CLI check
            // we cannot resolve this without the daemon, so we skip.
            // The daemon-side check should resolve SG members.
        }
    }

    true
}

/// Simple CIDR containment check.
fn cidr_contains(cidr: &str, ip: &str) -> bool {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    let net: std::net::Ipv4Addr = match parts[0].parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let prefix_len: u32 = match parts[1].parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let check: std::net::Ipv4Addr = match ip.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };

    if prefix_len == 0 {
        return true;
    }
    if prefix_len > 32 {
        return false;
    }

    let mask = !0u32 << (32 - prefix_len);
    let net_bits = u32::from(net) & mask;
    let check_bits = u32::from(check) & mask;
    net_bits == check_bits
}

/// Send a request to the overlay layer via the daemon's control socket.
async fn send_overlay_request(
    socket_path: &std::path::Path,
    request: &serde_json::Value,
) -> std::io::Result<serde_json::Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).await?;

    let payload = serde_json::to_vec(request)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

    // Write length-prefixed message
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    // Read response
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await?;

    serde_json::from_slice(&resp_buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sg::{PortRange, RuleId, SecurityGroupId};

    fn make_rule(
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
    fn test_evaluate_tcp_allowed() {
        let rules = vec![make_rule(
            Protocol::Tcp,
            Some(PortRange { from: 443, to: 443 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let (verdict, _reason) = evaluate_rules(&rules, 443, Protocol::Tcp, "10.0.0.1");
        assert_eq!(verdict, "ALLOWED");
    }

    #[test]
    fn test_evaluate_tcp_denied_wrong_port() {
        let rules = vec![make_rule(
            Protocol::Tcp,
            Some(PortRange { from: 443, to: 443 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let (verdict, _reason) = evaluate_rules(&rules, 80, Protocol::Tcp, "10.0.0.1");
        assert_eq!(verdict, "DENIED");
    }

    #[test]
    fn test_evaluate_denied_wrong_protocol() {
        let rules = vec![make_rule(
            Protocol::Tcp,
            Some(PortRange { from: 443, to: 443 }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let (verdict, _) = evaluate_rules(&rules, 443, Protocol::Udp, "10.0.0.1");
        assert_eq!(verdict, "DENIED");
    }

    #[test]
    fn test_evaluate_cidr_source_match() {
        let rules = vec![make_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("10.0.0.0/8".to_string()),
            100,
        )];
        let (verdict, _) = evaluate_rules(&rules, 22, Protocol::Tcp, "10.1.2.3");
        assert_eq!(verdict, "ALLOWED");
    }

    #[test]
    fn test_evaluate_cidr_source_no_match() {
        let rules = vec![make_rule(
            Protocol::Tcp,
            Some(PortRange { from: 22, to: 22 }),
            TrafficSource::Cidr("10.0.0.0/8".to_string()),
            100,
        )];
        let (verdict, _) = evaluate_rules(&rules, 22, Protocol::Tcp, "192.168.1.1");
        assert_eq!(verdict, "DENIED");
    }

    #[test]
    fn test_evaluate_no_rules() {
        let (verdict, reason) = evaluate_rules(&[], 80, Protocol::Tcp, "10.0.0.1");
        assert_eq!(verdict, "DENIED");
        assert!(reason.contains("no matching"));
    }

    #[test]
    fn test_evaluate_protocol_all() {
        let rules = vec![make_rule(
            Protocol::All,
            None,
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let (verdict, _) = evaluate_rules(&rules, 80, Protocol::Tcp, "10.0.0.1");
        assert_eq!(verdict, "ALLOWED");
    }

    #[test]
    fn test_cidr_contains_basic() {
        assert!(cidr_contains("10.0.0.0/8", "10.1.2.3"));
        assert!(!cidr_contains("10.0.0.0/8", "192.168.1.1"));
        assert!(cidr_contains("0.0.0.0/0", "1.2.3.4"));
        assert!(cidr_contains("192.168.1.0/24", "192.168.1.100"));
        assert!(!cidr_contains("192.168.1.0/24", "192.168.2.1"));
    }

    #[test]
    fn test_evaluate_port_range() {
        let rules = vec![make_rule(
            Protocol::Tcp,
            Some(PortRange {
                from: 8000,
                to: 9000,
            }),
            TrafficSource::Cidr("0.0.0.0/0".to_string()),
            100,
        )];
        let (v1, _) = evaluate_rules(&rules, 8500, Protocol::Tcp, "10.0.0.1");
        assert_eq!(v1, "ALLOWED");
        let (v2, _) = evaluate_rules(&rules, 7999, Protocol::Tcp, "10.0.0.1");
        assert_eq!(v2, "DENIED");
    }
}
