use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Unique identifier for an organization.
pub type OrgId = String;

/// Unique identifier for a project.
pub type ProjectId = String;

/// Unique identifier for an environment.
pub type EnvironmentId = String;

/// An organization — the root tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Org {
    pub id: OrgId,
    pub name: String,
    pub created_at: u64,
}

/// A project within an organization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub org_id: OrgId,
    pub created_at: u64,
}

/// An environment within a project. Where resources actually live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Environment {
    pub id: EnvironmentId,
    pub name: String,
    pub project_id: ProjectId,
    pub org_id: OrgId,
    /// Optional TTL in seconds. When set, `expires_at` is computed.
    pub ttl_secs: Option<u64>,
    /// Unix timestamp when this environment expires. `None` means permanent.
    pub expires_at: Option<u64>,
    /// Prevent accidental deletion.
    pub deletion_protection: bool,
    /// Arbitrary key-value labels.
    pub labels: HashMap<String, String>,
    pub created_at: u64,
}

impl Environment {
    /// Returns true if this environment has expired based on the given timestamp.
    pub fn is_expired(&self, now_epoch: u64) -> bool {
        match self.expires_at {
            Some(expires) => now_epoch > expires,
            None => false,
        }
    }
}

/// Returns the current Unix epoch in seconds.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
