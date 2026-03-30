use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for an organization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrgId(pub String);

impl fmt::Display for OrgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub String);

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An organization — the root tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Validate a resource name.
///
/// Names must be lowercase alphanumeric with hyphens and forward slashes,
/// between 3 and 63 characters. Must start and end with alphanumeric.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.len() < 3 {
        return Err(format!(
            "name '{}' is too short (minimum 3 characters)",
            name
        ));
    }
    if name.len() > 63 {
        return Err(format!(
            "name '{}' is too long (maximum 63 characters)",
            name
        ));
    }

    let chars: Vec<char> = name.chars().collect();

    if !chars[0].is_ascii_lowercase() && !chars[0].is_ascii_digit() {
        return Err(format!(
            "name '{}' must start with a lowercase letter or digit",
            name
        ));
    }

    if !chars[chars.len() - 1].is_ascii_lowercase() && !chars[chars.len() - 1].is_ascii_digit() {
        return Err(format!(
            "name '{}' must end with a lowercase letter or digit",
            name
        ));
    }

    for ch in &chars {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && *ch != '-' && *ch != '/' {
            return Err(format!(
                "name '{}' contains invalid character '{}' (allowed: lowercase alphanumeric, hyphens, forward slashes)",
                name, ch
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(validate_name("my-org").is_ok());
        assert!(validate_name("abc").is_ok());
        assert!(validate_name("my-project/sub").is_ok());
        assert!(validate_name("a1b2c3").is_ok());
        assert!(validate_name("123").is_ok());
    }

    #[test]
    fn name_too_short() {
        assert!(validate_name("ab").is_err());
        assert!(validate_name("a").is_err());
        assert!(validate_name("").is_err());
    }

    #[test]
    fn name_too_long() {
        let long_name = "a".repeat(64);
        assert!(validate_name(&long_name).is_err());
    }

    #[test]
    fn name_invalid_chars() {
        assert!(validate_name("My-Org").is_err()); // uppercase
        assert!(validate_name("my org").is_err()); // space
        assert!(validate_name("my_org").is_err()); // underscore
        assert!(validate_name("my@org").is_err()); // special char
    }

    #[test]
    fn name_must_start_end_alphanumeric() {
        assert!(validate_name("-my-org").is_err());
        assert!(validate_name("my-org-").is_err());
        assert!(validate_name("/my-org").is_err());
    }
}
