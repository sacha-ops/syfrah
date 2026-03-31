use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// ResourceState — unified lifecycle for network resources
// ---------------------------------------------------------------------------

/// Lifecycle state for mutable network resources (NIC, SG, NAT Gateway, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceState {
    Pending,
    Active,
    Failed,
    Deleting,
    Deleted,
}

impl fmt::Display for ResourceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceState::Pending => f.write_str("Pending"),
            ResourceState::Active => f.write_str("Active"),
            ResourceState::Failed => f.write_str("Failed"),
            ResourceState::Deleting => f.write_str("Deleting"),
            ResourceState::Deleted => f.write_str("Deleted"),
        }
    }
}

// ---------------------------------------------------------------------------
// SecurityGroup
// ---------------------------------------------------------------------------

/// Unique identifier for a security group.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecurityGroupId(pub String);

impl fmt::Display for SecurityGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A security group — a set of allow-only firewall rules attached to NICs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityGroup {
    pub id: SecurityGroupId,
    pub name: String,
    pub description: String,
    pub vpc_id: VpcId,
    pub state: ResourceState,
    pub created_at: u64,
    pub updated_at: u64,
}

// ---------------------------------------------------------------------------
// NetworkInterface (NIC)
// ---------------------------------------------------------------------------

/// Unique identifier for a network interface.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct NicId(pub String);

impl fmt::Display for NicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A network interface — the attachment point for security groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub id: NicId,
    pub name: String,
    pub vm_id: Option<String>,
    pub subnet_id: SubnetId,
    pub vpc_id: VpcId,
    pub private_ip: String,
    pub mac: String,
    pub security_groups: Vec<SecurityGroupId>,
    pub state: ResourceState,
    pub created_at: u64,
}

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
