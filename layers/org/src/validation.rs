use crate::error::OrgError;

/// Validate an org name.
///
/// Rules: lowercase alphanumeric and hyphens only, 3-63 characters.
/// Must not start or end with a hyphen.
pub fn validate_name(name: &str) -> Result<(), OrgError> {
    let len = name.len();
    if !(3..=63).contains(&len) {
        return Err(OrgError::InvalidName(format!(
            "name must be 3-63 characters, got {len}"
        )));
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(OrgError::InvalidName(
            "name must contain only lowercase alphanumeric characters and hyphens".to_string(),
        ));
    }

    if name.starts_with('-') || name.ends_with('-') {
        return Err(OrgError::InvalidName(
            "name must not start or end with a hyphen".to_string(),
        ));
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
        assert!(validate_name("org-123").is_ok());
        assert!(validate_name("abc").is_ok());
        assert!(validate_name(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn too_short() {
        assert!(validate_name("ab").is_err());
        assert!(validate_name("a").is_err());
        assert!(validate_name("").is_err());
    }

    #[test]
    fn too_long() {
        assert!(validate_name(&"a".repeat(64)).is_err());
    }

    #[test]
    fn uppercase_rejected() {
        assert!(validate_name("Acme").is_err());
        assert!(validate_name("ACME").is_err());
        assert!(validate_name("aCmE").is_err());
    }

    #[test]
    fn spaces_rejected() {
        assert!(validate_name("my org").is_err());
        assert!(validate_name(" acme").is_err());
    }

    #[test]
    fn special_chars_rejected() {
        assert!(validate_name("my_org").is_err());
        assert!(validate_name("my.org").is_err());
        assert!(validate_name("my@org").is_err());
        assert!(validate_name("org!").is_err());
    }

    #[test]
    fn leading_trailing_hyphen_rejected() {
        assert!(validate_name("-acme").is_err());
        assert!(validate_name("acme-").is_err());
        assert!(validate_name("-acme-").is_err());
    }
}
