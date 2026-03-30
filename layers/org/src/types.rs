use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for an organization.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct OrgId(pub String);

impl fmt::Display for OrgId {
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
