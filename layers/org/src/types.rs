use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Unique identifier for a VPC.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct VpcId(pub String);

impl fmt::Display for VpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The owner of a VPC — either a project or an org (for shared VPCs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpcOwner {
    Project(ProjectId),
    Org(OrgId),
}

/// A Virtual Private Cloud — one VPC = one VXLAN VNI = one isolated L2 domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vpc {
    pub id: VpcId,
    pub name: String,
    pub cidr: String,
    pub vni: u32,
    pub owner: VpcOwner,
    pub shared: bool,
    pub created_at: u64,
}

/// Unique identifier for an organization.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct OrgId(pub String);

impl fmt::Display for OrgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub String);

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for an environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentId(pub String);

impl fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An organization — the root tenant in the Syfrah hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Org {
    pub id: OrgId,
    pub name: String,
    pub created_at: u64,
}

/// A project within an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub org_id: OrgId,
    pub created_at: u64,
}

/// A record of a shared VPC being attached to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpcAttachment {
    pub vpc_name: String,
    pub project_id: ProjectId,
    pub attached_at: u64,
}

/// Unique identifier for a subnet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubnetId(pub String);

impl fmt::Display for SubnetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A subnet within a VPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subnet {
    pub id: SubnetId,
    pub name: String,
    pub vpc_id: VpcId,
    pub env_id: EnvironmentId,
    pub cidr: String,
    pub gateway: String,
    pub created_at: u64,
}

/// Unique identifier for a VPC peering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeeringId(pub String);

impl fmt::Display for PeeringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Status of a VPC peering connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeeringStatus {
    Active,
    Pending,
    Deleted,
}

impl fmt::Display for PeeringStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeeringStatus::Active => f.write_str("Active"),
            PeeringStatus::Pending => f.write_str("Pending"),
            PeeringStatus::Deleted => f.write_str("Deleted"),
        }
    }
}

/// A VPC peering connection between two VPCs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpcPeering {
    pub id: PeeringId,
    pub vpc_a: String,
    pub vpc_b: String,
    pub status: PeeringStatus,
    pub created_at: u64,
}

/// An environment — a runtime context within a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id: EnvironmentId,
    pub name: String,
    pub project_id: ProjectId,
    pub ttl: Option<u64>,
    pub deletion_protection: bool,
    pub labels: HashMap<String, String>,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

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

/// Traffic direction for a security group rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Ingress,
    Egress,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Ingress => f.write_str("Ingress"),
            Direction::Egress => f.write_str("Egress"),
        }
    }
}

/// Network protocol for a security group rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
    Icmp,
    All,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Tcp => f.write_str("TCP"),
            Protocol::Udp => f.write_str("UDP"),
            Protocol::Icmp => f.write_str("ICMP"),
            Protocol::All => f.write_str("All"),
        }
    }
}

/// A port range (inclusive). Single port: from == to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRange {
    pub from: u16,
    pub to: u16,
}

/// The source (for ingress) or destination (for egress) of traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    Cidr(String),
    SecurityGroup(SecurityGroupId),
}

impl fmt::Display for RuleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleSource::Cidr(cidr) => write!(f, "{cidr}"),
            RuleSource::SecurityGroup(sg_id) => write!(f, "sg:{sg_id}"),
        }
    }
}

/// A rule within a security group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityGroupRule {
    pub id: RuleId,
    pub sg_id: SecurityGroupId,
    pub direction: Direction,
    pub protocol: Protocol,
    pub port_range: Option<PortRange>,
    pub source: RuleSource,
    pub priority: u32,
    pub description: Option<String>,
}

/// Whether a VM placement is being added or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementAction {
    Add,
    Remove,
}

impl fmt::Display for PlacementAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlacementAction::Add => f.write_str("Add"),
            PlacementAction::Remove => f.write_str("Remove"),
        }
    }
}

/// Tracks which node a VM is placed on, along with its network coordinates.
///
/// Persisted in the `vm_placements` redb table (key: "vpc_id/vm_id").
/// Used for FDB distribution: when a VM is created or deleted, all nodes
/// in the VPC must update their forwarding tables accordingly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmPlacement {
    pub vpc_id: String,
    pub vm_id: String,
    pub vm_mac: String,
    pub vm_ip: String,
    pub subnet_id: String,
    pub hosting_node: String,
    pub action: PlacementAction,
    pub created_at: u64,
}
