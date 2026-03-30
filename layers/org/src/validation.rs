use crate::error::OrgError;

/// Validate a name for an org, project, or environment.
///
/// Rules: lowercase alphanumeric and hyphens only, 3-63 characters.
/// Must not start or end with a hyphen.
///
/// The `context` parameter (e.g. "org", "project", "environment") is used
/// in error messages so users know which entity has the invalid name.
pub fn validate_name(name: &str, context: &str) -> Result<(), OrgError> {
    let len = name.len();
    if !(3..=63).contains(&len) {
        return Err(OrgError::InvalidName {
            context: context.to_string(),
            reason: format!("name must be 3-63 characters, got {len}"),
        });
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(OrgError::InvalidName {
            context: context.to_string(),
            reason: "name must contain only lowercase alphanumeric characters and hyphens"
                .to_string(),
        });
    }

    if name.starts_with('-') || name.ends_with('-') {
        return Err(OrgError::InvalidName {
            context: context.to_string(),
            reason: "name must not start or end with a hyphen".to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(validate_name("acme", "org").is_ok());
        assert!(validate_name("my-org", "org").is_ok());
        assert!(validate_name("org-123", "org").is_ok());
        assert!(validate_name("abc", "project").is_ok());
        assert!(validate_name(&"a".repeat(63), "environment").is_ok());
    }

    #[test]
    fn too_short() {
        assert!(validate_name("ab", "org").is_err());
        assert!(validate_name("a", "org").is_err());
        assert!(validate_name("", "org").is_err());
    }

    #[test]
    fn too_long() {
        assert!(validate_name(&"a".repeat(64), "org").is_err());
    }

    #[test]
    fn uppercase_rejected() {
        assert!(validate_name("Acme", "org").is_err());
        assert!(validate_name("ACME", "org").is_err());
        assert!(validate_name("aCmE", "org").is_err());
    }

    #[test]
    fn spaces_rejected() {
        assert!(validate_name("my org", "org").is_err());
        assert!(validate_name(" acme", "org").is_err());
    }

    #[test]
    fn special_chars_rejected() {
        assert!(validate_name("my_org", "org").is_err());
        assert!(validate_name("my.org", "org").is_err());
        assert!(validate_name("my@org", "org").is_err());
        assert!(validate_name("org!", "org").is_err());
    }

    #[test]
    fn leading_trailing_hyphen_rejected() {
        assert!(validate_name("-acme", "org").is_err());
        assert!(validate_name("acme-", "org").is_err());
        assert!(validate_name("-acme-", "org").is_err());
    }

    #[test]
    fn error_message_includes_context() {
        let err = validate_name("ab", "project").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("project"),
            "expected 'project' in error: {msg}"
        );

        let err = validate_name("ab", "environment").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("environment"),
            "expected 'environment' in error: {msg}"
        );

        let err = validate_name("ab", "org").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("org"), "expected 'org' in error: {msg}");
    }
}
