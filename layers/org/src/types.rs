use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Unique identifier for an organization.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct OrgId(pub String);

/// Unique identifier for a project.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectId(pub String);

/// Unique identifier for an environment.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnvironmentId(pub String);

/// An organization — the root tenant.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Org {
    pub id: OrgId,
    pub name: String,
    pub created_at: u64,
}

/// A project within an organization.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub org_id: OrgId,
    pub created_at: u64,
}

/// An environment within a project.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub id: EnvironmentId,
    pub name: String,
    pub project_id: ProjectId,
    /// Time-to-live in seconds. None means permanent.
    pub ttl_secs: Option<u64>,
    /// Prevent accidental deletion.
    pub deletion_protection: bool,
    /// Arbitrary key-value labels.
    pub labels: HashMap<String, String>,
    pub created_at: u64,
}

/// Validate a resource name: lowercase alphanumeric, hyphens, forward slashes, 3-63 chars.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.len() < 3 || name.len() > 63 {
        return Err(format!("name must be 3-63 characters, got {}", name.len()));
    }
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' && ch != '/' {
            return Err(format!(
                "name must be lowercase alphanumeric, hyphens, or forward slashes; got '{ch}'"
            ));
        }
    }
    Ok(())
}

impl Org {
    pub fn new(name: String) -> Result<Self, String> {
        validate_name(&name)?;
        Ok(Self {
            id: OrgId(name.clone()),
            name,
            created_at: now_epoch(),
        })
    }
}

impl Project {
    pub fn new(name: String, org_id: OrgId) -> Result<Self, String> {
        validate_name(&name)?;
        Ok(Self {
            id: ProjectId(format!("{}/{}", org_id.0, name)),
            name,
            org_id,
            created_at: now_epoch(),
        })
    }
}

impl Environment {
    pub fn new(
        name: String,
        project_id: ProjectId,
        ttl: Option<Duration>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    ) -> Result<Self, String> {
        validate_name(&name)?;
        Ok(Self {
            id: EnvironmentId(format!("{}/{}", project_id.0, name)),
            name,
            project_id,
            ttl_secs: ttl.map(|d| d.as_secs()),
            deletion_protection,
            labels,
            created_at: now_epoch(),
        })
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
