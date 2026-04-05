//! Input validation functions shared across all layers.
//!
//! Every user-facing input (names, CIDRs, ports, etc.) is validated here
//! so the rules are consistent and never duplicated.
//!
//! ```
//! use syfrah_core::validate;
//!
//! assert!(validate::name("my-vpc").is_ok());
//! assert!(validate::name("MY_VPC").is_err());  // uppercase not allowed
//! assert!(validate::name("ab").is_err());       // too short
//! ```

use crate::error::SyfrahError;

/// Validate a resource name.
///
/// Rules:
/// - 3-63 characters
/// - Lowercase alphanumeric and hyphens only
/// - Must start with a letter
/// - Must end with a letter or digit
/// - No consecutive hyphens
///
/// These rules match DNS label standards (RFC 1123) with a minimum length of 3.
pub fn name(input: &str) -> Result<(), SyfrahError> {
    if input.is_empty() {
        return Err(SyfrahError::invalid_name(input, "name cannot be empty"));
    }
    if input.len() < 3 {
        return Err(SyfrahError::invalid_name(
            input,
            "must be at least 3 characters",
        ));
    }
    if input.len() > 63 {
        return Err(SyfrahError::invalid_name(
            input,
            "must be at most 63 characters",
        ));
    }
    if !input.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(SyfrahError::invalid_name(
            input,
            "must start with a lowercase letter",
        ));
    }
    if !input.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return Err(SyfrahError::invalid_name(
            input,
            "must end with a lowercase letter or digit",
        ));
    }
    if input.contains("--") {
        return Err(SyfrahError::invalid_name(
            input,
            "must not contain consecutive hyphens",
        ));
    }
    for c in input.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(SyfrahError::invalid_name(
                input,
                &format!(
                    "invalid character '{c}' — only lowercase letters, digits, and hyphens allowed"
                ),
            ));
        }
    }
    Ok(())
}

/// Validate a CIDR block (IPv4 only for now).
///
/// Rules:
/// - Format: `A.B.C.D/N`
/// - Each octet 0-255
/// - Prefix length 0-32
/// - Network address matches prefix (e.g., 10.1.0.0/16 not 10.1.1.0/16 if 1.0 expected)
pub fn cidr(input: &str) -> Result<(), SyfrahError> {
    let parts: Vec<&str> = input.split('/').collect();
    if parts.len() != 2 {
        return Err(SyfrahError::validation(format!(
            "invalid CIDR '{input}': must be in format A.B.C.D/N (e.g., 10.0.0.0/16)"
        )));
    }

    let ip_str = parts[0];
    let prefix_str = parts[1];

    // Validate prefix length
    let prefix: u8 = prefix_str.parse().map_err(|_| {
        SyfrahError::validation(format!(
            "invalid CIDR '{input}': prefix length must be a number (0-32)"
        ))
    })?;
    if prefix > 32 {
        return Err(SyfrahError::validation(format!(
            "invalid CIDR '{input}': prefix length must be 0-32, got {prefix}"
        )));
    }

    // Validate IP octets
    let octets: Vec<&str> = ip_str.split('.').collect();
    if octets.len() != 4 {
        return Err(SyfrahError::validation(format!(
            "invalid CIDR '{input}': IP must have 4 octets (e.g., 10.0.0.0)"
        )));
    }
    let mut ip_bytes = [0u8; 4];
    for (i, octet) in octets.iter().enumerate() {
        ip_bytes[i] = octet.parse().map_err(|_| {
            SyfrahError::validation(format!(
                "invalid CIDR '{input}': octet '{octet}' is not a valid number (0-255)"
            ))
        })?;
    }

    // Validate network address: host bits must be zero
    let ip_u32 = u32::from_be_bytes(ip_bytes);
    let mask = if prefix == 0 {
        0u32
    } else {
        !0u32 << (32 - prefix)
    };
    if ip_u32 & !mask != 0 {
        let correct_network = ip_u32 & mask;
        let bytes = correct_network.to_be_bytes();
        return Err(SyfrahError::validation(format!(
            "invalid CIDR '{input}': host bits must be zero. Did you mean {}.{}.{}.{}/{prefix}?",
            bytes[0], bytes[1], bytes[2], bytes[3]
        )));
    }

    Ok(())
}

/// Validate a port number.
pub fn port(input: u16) -> Result<(), SyfrahError> {
    if input == 0 {
        return Err(SyfrahError::validation("port must be between 1 and 65535"));
    }
    Ok(())
}

/// Validate a port from a string.
pub fn port_str(input: &str) -> Result<u16, SyfrahError> {
    let p: u16 = input.parse().map_err(|_| {
        SyfrahError::validation(format!(
            "invalid port '{input}': must be a number between 1 and 65535"
        ))
    })?;
    port(p)?;
    Ok(p)
}

/// Validate a region label.
///
/// Rules: 1-32 characters, lowercase alphanumeric and hyphens.
pub fn region(input: &str) -> Result<(), SyfrahError> {
    if input.is_empty() || input.len() > 32 {
        return Err(SyfrahError::validation(format!(
            "invalid region '{input}': must be 1-32 characters"
        )));
    }
    for c in input.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(SyfrahError::validation(format!(
                "invalid region '{input}': only lowercase letters, digits, and hyphens allowed"
            )));
        }
    }
    Ok(())
}

/// Validate a zone label.
///
/// Rules: 1-32 characters, lowercase alphanumeric and hyphens.
pub fn zone(input: &str) -> Result<(), SyfrahError> {
    if input.is_empty() || input.len() > 32 {
        return Err(SyfrahError::validation(format!(
            "invalid zone '{input}': must be 1-32 characters"
        )));
    }
    for c in input.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(SyfrahError::validation(format!(
                "invalid zone '{input}': only lowercase letters, digits, and hyphens allowed"
            )));
        }
    }
    Ok(())
}

/// Validate a label key=value pair.
///
/// Key: 1-63 chars, alphanumeric + hyphens + dots + underscores, starts with letter.
/// Value: 0-63 chars, alphanumeric + hyphens + dots + underscores.
pub fn label(input: &str) -> Result<(&str, &str), SyfrahError> {
    let parts: Vec<&str> = input.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err(SyfrahError::validation(format!(
            "invalid label '{input}': must be in format key=value"
        )));
    }
    let key = parts[0];
    let value = parts[1];

    if key.is_empty() || key.len() > 63 {
        return Err(SyfrahError::validation(format!(
            "invalid label key '{key}': must be 1-63 characters"
        )));
    }
    if !key.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err(SyfrahError::validation(format!(
            "invalid label key '{key}': must start with a letter"
        )));
    }
    for c in key.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '.' && c != '_' {
            return Err(SyfrahError::validation(format!(
                "invalid label key '{key}': character '{c}' not allowed"
            )));
        }
    }
    if value.len() > 63 {
        return Err(SyfrahError::validation(format!(
            "invalid label value '{value}': must be at most 63 characters"
        )));
    }
    Ok((key, value))
}

/// Validate a size in GB.
pub fn size_gb(input: u64) -> Result<(), SyfrahError> {
    if input == 0 {
        return Err(SyfrahError::validation("size must be at least 1 GB"));
    }
    if input > 65536 {
        return Err(SyfrahError::validation(format!(
            "size {input} GB exceeds maximum of 65536 GB (64 TB)"
        )));
    }
    Ok(())
}

/// Validate memory in MB.
pub fn memory_mb(input: u64) -> Result<(), SyfrahError> {
    if input < 128 {
        return Err(SyfrahError::validation("memory must be at least 128 MB"));
    }
    if input > 1_048_576 {
        return Err(SyfrahError::validation(format!(
            "memory {input} MB exceeds maximum of 1048576 MB (1 TB)"
        )));
    }
    Ok(())
}

/// Validate vCPU count.
pub fn vcpus(input: u32) -> Result<(), SyfrahError> {
    if input == 0 {
        return Err(SyfrahError::validation("vCPUs must be at least 1"));
    }
    if input > 256 {
        return Err(SyfrahError::validation(format!(
            "vCPUs {input} exceeds maximum of 256"
        )));
    }
    Ok(())
}

/// Parse and validate a duration string (e.g., "30m", "2h", "7d").
/// Returns seconds.
pub fn duration(input: &str) -> Result<u64, SyfrahError> {
    if input.is_empty() {
        return Err(SyfrahError::validation("duration cannot be empty"));
    }

    let (num_str, unit) = input.split_at(input.len() - 1);
    let num: u64 = num_str.parse().map_err(|_| {
        SyfrahError::validation(format!(
            "invalid duration '{input}': must be a number followed by s/m/h/d (e.g., 30m, 2h, 7d)"
        ))
    })?;

    let seconds = match unit {
        "s" => num,
        "m" => num * 60,
        "h" => num * 3600,
        "d" => num * 86400,
        _ => {
            return Err(SyfrahError::validation(format!(
                "invalid duration '{input}': unknown unit '{unit}'. Use s (seconds), m (minutes), h (hours), d (days)"
            )))
        }
    };

    if seconds == 0 {
        return Err(SyfrahError::validation("duration must be greater than 0"));
    }

    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Name validation ──

    #[test]
    fn name_valid() {
        assert!(name("my-vpc").is_ok());
        assert!(name("web-1").is_ok());
        assert!(name("abc").is_ok());
        assert!(name("a-really-long-name-that-is-still-valid-123").is_ok());
    }

    #[test]
    fn name_too_short() {
        assert!(name("").is_err());
        assert!(name("a").is_err());
        assert!(name("ab").is_err());
    }

    #[test]
    fn name_too_long() {
        let long = "a".repeat(64);
        assert!(name(&long).is_err());
        let ok = "a".repeat(63);
        assert!(name(&ok).is_ok());
    }

    #[test]
    fn name_must_start_with_letter() {
        assert!(name("1abc").is_err());
        assert!(name("-abc").is_err());
    }

    #[test]
    fn name_must_end_with_letter_or_digit() {
        assert!(name("abc-").is_err());
    }

    #[test]
    fn name_no_uppercase() {
        assert!(name("MyVpc").is_err());
    }

    #[test]
    fn name_no_underscores() {
        assert!(name("my_vpc").is_err());
    }

    #[test]
    fn name_no_consecutive_hyphens() {
        assert!(name("my--vpc").is_err());
    }

    #[test]
    fn name_no_spaces() {
        assert!(name("my vpc").is_err());
    }

    #[test]
    fn name_error_message_is_actionable() {
        let err = name("MY_VPC").unwrap_err();
        assert!(err.message.contains("MY_VPC"));
        assert!(err.message.contains("lowercase") || err.message.contains("invalid character"));
    }

    // ── CIDR validation ──

    #[test]
    fn cidr_valid() {
        assert!(cidr("10.0.0.0/8").is_ok());
        assert!(cidr("10.1.0.0/16").is_ok());
        assert!(cidr("192.168.1.0/24").is_ok());
        assert!(cidr("0.0.0.0/0").is_ok());
    }

    #[test]
    fn cidr_no_prefix() {
        assert!(cidr("10.0.0.0").is_err());
    }

    #[test]
    fn cidr_prefix_too_large() {
        assert!(cidr("10.0.0.0/33").is_err());
    }

    #[test]
    fn cidr_bad_octets() {
        assert!(cidr("10.0.0.999/24").is_err());
        assert!(cidr("10.0/24").is_err());
    }

    #[test]
    fn cidr_host_bits_not_zero() {
        let err = cidr("10.1.1.0/16").unwrap_err();
        assert!(err.message.contains("10.1.0.0/16"), "got: {}", err.message);
    }

    #[test]
    fn cidr_suggests_correct_network() {
        let err = cidr("192.168.1.100/24").unwrap_err();
        assert!(err.message.contains("192.168.1.0/24"));
    }

    // ── Port validation ──

    #[test]
    fn port_valid() {
        assert!(port(1).is_ok());
        assert!(port(80).is_ok());
        assert!(port(443).is_ok());
        assert!(port(65535).is_ok());
    }

    #[test]
    fn port_zero_invalid() {
        assert!(port(0).is_err());
    }

    #[test]
    fn port_str_valid() {
        assert_eq!(port_str("80").unwrap(), 80);
        assert_eq!(port_str("443").unwrap(), 443);
    }

    #[test]
    fn port_str_invalid() {
        assert!(port_str("abc").is_err());
        assert!(port_str("0").is_err());
        assert!(port_str("99999").is_err());
    }

    // ── Region/Zone validation ──

    #[test]
    fn region_valid() {
        assert!(region("eu").is_ok());
        assert!(region("eu-west").is_ok());
        assert!(region("us-east-1").is_ok());
    }

    #[test]
    fn region_invalid() {
        assert!(region("").is_err());
        assert!(region("EU").is_err());
        assert!(region(&"a".repeat(33)).is_err());
    }

    #[test]
    fn zone_valid() {
        assert!(zone("fsn1").is_ok());
        assert!(zone("nbg1").is_ok());
        assert!(zone("eu-west-1a").is_ok());
    }

    #[test]
    fn zone_invalid() {
        assert!(zone("").is_err());
        assert!(zone("FSN1").is_err());
    }

    // ── Label validation ──

    #[test]
    fn label_valid() {
        assert_eq!(label("env=prod").unwrap(), ("env", "prod"));
        assert_eq!(label("tier=frontend").unwrap(), ("tier", "frontend"));
        assert_eq!(label("version=1.0").unwrap(), ("version", "1.0"));
        assert_eq!(label("empty=").unwrap(), ("empty", ""));
    }

    #[test]
    fn label_no_equals() {
        assert!(label("nope").is_err());
    }

    #[test]
    fn label_empty_key() {
        assert!(label("=value").is_err());
    }

    #[test]
    fn label_key_must_start_with_letter() {
        assert!(label("1key=val").is_err());
    }

    // ── Size validation ──

    #[test]
    fn size_gb_valid() {
        assert!(size_gb(1).is_ok());
        assert!(size_gb(100).is_ok());
        assert!(size_gb(65536).is_ok());
    }

    #[test]
    fn size_gb_zero() {
        assert!(size_gb(0).is_err());
    }

    #[test]
    fn size_gb_too_large() {
        assert!(size_gb(65537).is_err());
    }

    // ── Memory/vCPU validation ──

    #[test]
    fn memory_valid() {
        assert!(memory_mb(128).is_ok());
        assert!(memory_mb(2048).is_ok());
    }

    #[test]
    fn memory_too_small() {
        assert!(memory_mb(64).is_err());
    }

    #[test]
    fn vcpus_valid() {
        assert!(vcpus(1).is_ok());
        assert!(vcpus(256).is_ok());
    }

    #[test]
    fn vcpus_zero() {
        assert!(vcpus(0).is_err());
    }

    #[test]
    fn vcpus_too_many() {
        assert!(vcpus(257).is_err());
    }

    // ── Duration validation ──

    #[test]
    fn duration_valid() {
        assert_eq!(duration("30s").unwrap(), 30);
        assert_eq!(duration("5m").unwrap(), 300);
        assert_eq!(duration("2h").unwrap(), 7200);
        assert_eq!(duration("7d").unwrap(), 604800);
    }

    #[test]
    fn duration_invalid_unit() {
        assert!(duration("30x").is_err());
    }

    #[test]
    fn duration_empty() {
        assert!(duration("").is_err());
    }

    #[test]
    fn duration_zero() {
        assert!(duration("0s").is_err());
    }

    #[test]
    fn duration_not_a_number() {
        assert!(duration("abcm").is_err());
    }
}
