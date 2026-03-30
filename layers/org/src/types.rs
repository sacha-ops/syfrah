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

/// A VPC peering connection between two VPCs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpcPeering {
    pub id: PeeringId,
    pub vpc_a: VpcId,
    pub vpc_b: VpcId,
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
