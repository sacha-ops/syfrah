//! Core types for the organization model.
//!
//! Hierarchy: Org -> Project -> Environment.
//! Every resource belongs to exactly one environment.

use serde::{Deserialize, Serialize};

/// An organization — the root tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Org {
    /// Unique name (lowercase alphanumeric + hyphens, 3-63 chars).
    pub name: String,
    /// Unix timestamp (seconds) when this org was created.
    pub created_at: u64,
}

/// Validation error for org names.
#[derive(Debug, thiserror::Error)]
pub enum OrgValidationError {
    #[error("invalid name: must be between 3 and 63 characters (got {0})")]
    Length(usize),
    #[error("invalid name: must be lowercase alphanumeric and hyphens only (got '{0}')")]
    InvalidChars(String),
    #[error("invalid name: must start and end with an alphanumeric character (got '{0}')")]
    BadBoundary(String),
}

/// Validate an org name: lowercase alphanumeric + hyphens, 3-63 chars,
/// must start and end with alphanumeric.
pub fn validate_org_name(name: &str) -> Result<(), OrgValidationError> {
    let len = name.len();
    if !(3..=63).contains(&len) {
        return Err(OrgValidationError::Length(len));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(OrgValidationError::InvalidChars(name.to_string()));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(OrgValidationError::BadBoundary(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(validate_org_name("acme").is_ok());
        assert!(validate_org_name("my-org").is_ok());
        assert!(validate_org_name("org-123").is_ok());
        assert!(validate_org_name("abc").is_ok());
        // 63 chars
        let long = "a".repeat(63);
        assert!(validate_org_name(&long).is_ok());
    }

    #[test]
    fn too_short() {
        assert!(validate_org_name("ab").is_err());
        assert!(validate_org_name("").is_err());
    }

    #[test]
    fn too_long() {
        let long = "a".repeat(64);
        assert!(validate_org_name(&long).is_err());
    }

    #[test]
    fn uppercase_rejected() {
        assert!(validate_org_name("Acme").is_err());
        assert!(validate_org_name("ACME").is_err());
    }

    #[test]
    fn special_chars_rejected() {
        assert!(validate_org_name("my_org").is_err());
        assert!(validate_org_name("my org").is_err());
        assert!(validate_org_name("my.org").is_err());
    }

    #[test]
    fn leading_trailing_hyphen_rejected() {
        assert!(validate_org_name("-org").is_err());
        assert!(validate_org_name("org-").is_err());
    }
}
