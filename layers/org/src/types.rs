use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for an organization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrgId(pub String);

/// Unique identifier for a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub String);

/// Unique identifier for an environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentId(pub String);

/// An organization — the root tenant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Org {
    pub id: OrgId,
    pub name: String,
    pub created_at: u64,
}

/// A project — a logical grouping within an organization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
