//! Security Group types.
//!
//! Scaffolded from ADR-002 for issue #864. These types represent the
//! security group model: groups, rules, directions, protocols, port
//! ranges, and traffic sources.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Unique identifier for a security group.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecurityGroupId(pub String);

impl fmt::Display for SecurityGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a security group rule.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Traffic direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Ingress,
    Egress,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Ingress => f.write_str("ingress"),
            Direction::Egress => f.write_str("egress"),
        }
    }
}

impl FromStr for Direction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ingress" => Ok(Direction::Ingress),
            "egress" => Ok(Direction::Egress),
            _ => Err(format!(
                "invalid direction '{s}': expected 'ingress' or 'egress'"
            )),
        }
    }
}

/// Network protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
    Icmp,
    All,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Tcp => f.write_str("tcp"),
            Protocol::Udp => f.write_str("udp"),
            Protocol::Icmp => f.write_str("icmp"),
            Protocol::All => f.write_str("all"),
        }
    }
}

impl FromStr for Protocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tcp" => Ok(Protocol::Tcp),
            "udp" => Ok(Protocol::Udp),
            "icmp" => Ok(Protocol::Icmp),
            "all" => Ok(Protocol::All),
            _ => Err(format!(
                "invalid protocol '{s}': expected 'tcp', 'udp', 'icmp', or 'all'"
            )),
        }
    }
}

/// Inclusive port range. Single port: `from == to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRange {
    pub from: u16,
    pub to: u16,
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.from == self.to {
            write!(f, "{}", self.from)
        } else {
            write!(f, "{}-{}", self.from, self.to)
        }
    }
}

impl FromStr for PortRange {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((from_s, to_s)) = s.split_once('-') {
            let from: u16 = from_s
                .trim()
                .parse()
                .map_err(|_| format!("invalid port range start: '{from_s}'"))?;
            let to: u16 = to_s
                .trim()
                .parse()
                .map_err(|_| format!("invalid port range end: '{to_s}'"))?;
            if from > to {
                return Err(format!(
                    "invalid port range: start ({from}) must be <= end ({to})"
                ));
            }
            if from == 0 {
                return Err("port number must be >= 1".to_string());
            }
            Ok(PortRange { from, to })
        } else {
            let port: u16 = s
                .trim()
                .parse()
                .map_err(|_| format!("invalid port number: '{s}'"))?;
            if port == 0 {
                return Err("port number must be >= 1".to_string());
            }
            Ok(PortRange {
                from: port,
                to: port,
            })
        }
    }
}

/// Where traffic comes from (ingress) or goes to (egress).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrafficSource {
    /// CIDR block, e.g. `0.0.0.0/0` or `10.1.0.0/16`.
    Cidr(String),
    /// Another security group, referenced by name.
    SecurityGroup(String),
}

impl fmt::Display for TrafficSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrafficSource::Cidr(cidr) => f.write_str(cidr),
            TrafficSource::SecurityGroup(name) => write!(f, "sg:{name}"),
        }
    }
}

impl FromStr for TrafficSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(sg_name) = s.strip_prefix("sg:") {
            if sg_name.is_empty() {
                return Err("security group name cannot be empty after 'sg:'".to_string());
            }
            Ok(TrafficSource::SecurityGroup(sg_name.to_string()))
        } else {
            // Validate as CIDR
            validate_cidr(s)?;
            Ok(TrafficSource::Cidr(s.to_string()))
        }
    }
}

/// Validate that a string looks like a valid CIDR (IPv4).
fn validate_cidr(s: &str) -> Result<(), String> {
    if let Some((ip_part, prefix_part)) = s.split_once('/') {
        let _prefix: u8 = prefix_part
            .parse()
            .map_err(|_| format!("invalid CIDR prefix: '{prefix_part}'"))?;
        let octets: Vec<&str> = ip_part.split('.').collect();
        if octets.len() != 4 {
            return Err(format!("invalid CIDR address: '{ip_part}'"));
        }
        for oct in &octets {
            let _n: u8 = oct
                .parse()
                .map_err(|_| format!("invalid CIDR octet: '{oct}'"))?;
        }
        Ok(())
    } else {
        Err(format!(
            "invalid source '{s}': expected CIDR (e.g. 0.0.0.0/0) or sg:<name>"
        ))
    }
}

/// A security group rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityGroupRule {
    pub id: RuleId,
    pub sg_id: SecurityGroupId,
    pub direction: Direction,
    pub protocol: Protocol,
    pub port_range: Option<PortRange>,
    pub source: TrafficSource,
    pub priority: u32,
    pub description: String,
    pub created_at: u64,
}

/// A security group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityGroup {
    pub id: SecurityGroupId,
    pub name: String,
    pub description: String,
    pub vpc_id: String,
    pub rules: Vec<SecurityGroupRule>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_range_single() {
        let pr: PortRange = "443".parse().unwrap();
        assert_eq!(pr, PortRange { from: 443, to: 443 });
        assert_eq!(pr.to_string(), "443");
    }

    #[test]
    fn port_range_range() {
        let pr: PortRange = "8000-9000".parse().unwrap();
        assert_eq!(
            pr,
            PortRange {
                from: 8000,
                to: 9000
            }
        );
        assert_eq!(pr.to_string(), "8000-9000");
    }

    #[test]
    fn port_range_invalid_reversed() {
        let err = "9000-8000".parse::<PortRange>().unwrap_err();
        assert!(err.contains("start (9000) must be <= end (8000)"));
    }

    #[test]
    fn port_range_zero_rejected() {
        let err = "0".parse::<PortRange>().unwrap_err();
        assert!(err.contains("must be >= 1"));
    }

    #[test]
    fn port_range_invalid_string() {
        assert!("abc".parse::<PortRange>().is_err());
    }

    #[test]
    fn direction_parse() {
        assert_eq!("ingress".parse::<Direction>().unwrap(), Direction::Ingress);
        assert_eq!("Egress".parse::<Direction>().unwrap(), Direction::Egress);
        assert!("invalid".parse::<Direction>().is_err());
    }

    #[test]
    fn protocol_parse() {
        assert_eq!("tcp".parse::<Protocol>().unwrap(), Protocol::Tcp);
        assert_eq!("UDP".parse::<Protocol>().unwrap(), Protocol::Udp);
        assert_eq!("icmp".parse::<Protocol>().unwrap(), Protocol::Icmp);
        assert_eq!("all".parse::<Protocol>().unwrap(), Protocol::All);
        assert!("invalid".parse::<Protocol>().is_err());
    }

    #[test]
    fn traffic_source_cidr() {
        let ts: TrafficSource = "10.0.0.0/8".parse().unwrap();
        assert_eq!(ts, TrafficSource::Cidr("10.0.0.0/8".to_string()));
        assert_eq!(ts.to_string(), "10.0.0.0/8");
    }

    #[test]
    fn traffic_source_sg() {
        let ts: TrafficSource = "sg:web-sg".parse().unwrap();
        assert_eq!(ts, TrafficSource::SecurityGroup("web-sg".to_string()));
        assert_eq!(ts.to_string(), "sg:web-sg");
    }

    #[test]
    fn traffic_source_invalid() {
        assert!("not-cidr".parse::<TrafficSource>().is_err());
        assert!("sg:".parse::<TrafficSource>().is_err());
    }
}
