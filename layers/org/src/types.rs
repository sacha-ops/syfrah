//! Core types for the organization model: Org -> Project -> Environment.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An organization — the root tenant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Org {
    pub name: String,
    pub created_at: u64,
}

/// A project within an organization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub org: String,
    pub created_at: u64,
}

/// An environment within a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    pub project: String,
    pub org: String,
    /// Optional TTL in seconds. When set, the environment auto-destroys after this duration.
    pub ttl: Option<u64>,
    /// When true, the environment cannot be deleted without first disabling protection.
    pub deletion_protection: bool,
    /// Arbitrary key-value labels for grouping, filtering, and cost reporting.
    pub labels: HashMap<String, String>,
    pub created_at: u64,
}
