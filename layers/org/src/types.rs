//! Core types for the organization model.
//!
//! Hierarchy: Org -> Project -> Environment (future).
//! Names: lowercase alphanumeric + hyphens, 3-63 chars, unique within parent.

use serde::{Deserialize, Serialize};

/// An organization — the root tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Org {
    pub name: String,
    pub created_at: u64,
}

/// A project within an organization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub org: String,
    pub created_at: u64,
}

/// Validation error for org/project names.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("name must be between 3 and 63 characters (got {0})")]
    Length(usize),
    #[error("name must contain only lowercase alphanumeric characters and hyphens, got '{0}'")]
    InvalidChars(String),
    #[error("name must not start or end with a hyphen")]
    LeadingTrailingHyphen,
}

/// Validate an org or project name.
///
/// Rules: lowercase alphanumeric + hyphens, 3-63 chars, no leading/trailing hyphens.
pub fn validate_name(name: &str) -> Result<(), ValidationError> {
    let len = name.len();
    if !(3..=63).contains(&len) {
        return Err(ValidationError::Length(len));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ValidationError::InvalidChars(name.to_string()));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ValidationError::LeadingTrailingHyphen);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(validate_name("acme").is_ok());
        assert!(validate_name("my-org").is_ok());
        assert!(validate_name("abc").is_ok());
        assert!(validate_name("a1b2c3").is_ok());
        let long = "a".repeat(63);
        assert!(validate_name(&long).is_ok());
    }

    #[test]
    fn too_short() {
        assert!(matches!(
            validate_name("ab"),
            Err(ValidationError::Length(2))
        ));
    }

    #[test]
    fn too_long() {
        let long = "a".repeat(64);
        assert!(matches!(
            validate_name(&long),
            Err(ValidationError::Length(64))
        ));
    }

    #[test]
    fn uppercase_rejected() {
        assert!(matches!(
            validate_name("Acme"),
            Err(ValidationError::InvalidChars(_))
        ));
    }

    #[test]
    fn leading_hyphen_rejected() {
        assert!(matches!(
            validate_name("-acme"),
            Err(ValidationError::LeadingTrailingHyphen)
        ));
    }

    #[test]
    fn trailing_hyphen_rejected() {
        assert!(matches!(
            validate_name("acme-"),
            Err(ValidationError::LeadingTrailingHyphen)
        ));
    }

    #[test]
    fn spaces_rejected() {
        assert!(matches!(
            validate_name("my org"),
            Err(ValidationError::InvalidChars(_))
        ));
    }
}
