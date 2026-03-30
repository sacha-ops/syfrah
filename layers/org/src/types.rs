use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

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

// ── VPC types ──────────────────────────────────────────────────────

/// Unique identifier for a VPC.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VpcId(pub String);

impl fmt::Display for VpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who owns a VPC — either a specific project or an entire org (shared).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpcOwner {
    /// VPC scoped to a single project.
    Project { org: String, project: String },
    /// Shared VPC owned by an org, attachable to multiple projects.
    Org(String),
}

impl fmt::Display for VpcOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VpcOwner::Project { org, project } => write!(f, "{org}/{project}"),
            VpcOwner::Org(org) => write!(f, "{org} (shared)"),
        }
    }
}

/// A Virtual Private Cloud — one VPC = one VXLAN VNI = one isolated L2 domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vpc {
    pub id: VpcId,
    pub name: String,
    /// CIDR block, stored as a string (e.g. "10.1.0.0/16").
    pub cidr: String,
    /// VXLAN Network Identifier — unique per VPC.
    pub vni: u32,
    pub owner: VpcOwner,
    pub shared: bool,
    pub created_at: u64,
}
