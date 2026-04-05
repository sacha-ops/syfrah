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

/// Validate an IPv4 address.
pub fn ipv4(input: &str) -> Result<[u8; 4], SyfrahError> {
    let octets: Vec<&str> = input.split('.').collect();
    if octets.len() != 4 {
        return Err(SyfrahError::validation(format!(
            "invalid IPv4 address '{input}': must have 4 octets (e.g., 10.0.0.1)"
        )));
    }
    let mut bytes = [0u8; 4];
    for (i, octet) in octets.iter().enumerate() {
        bytes[i] = octet.parse().map_err(|_| {
            SyfrahError::validation(format!(
                "invalid IPv4 address '{input}': octet '{octet}' is not a valid number (0-255)"
            ))
        })?;
    }
    Ok(bytes)
}

/// Validate an IPv6 address (simplified — accepts standard and compressed forms).
pub fn ipv6(input: &str) -> Result<(), SyfrahError> {
    // Use Rust's built-in parser
    input
        .parse::<std::net::Ipv6Addr>()
        .map_err(|_| {
            SyfrahError::validation(format!(
                "invalid IPv6 address '{input}': must be a valid IPv6 address (e.g., fd01::1, 2001:db8::1)"
            ))
        })?;
    Ok(())
}

/// Validate an IP address (IPv4 or IPv6).
pub fn ip_addr(input: &str) -> Result<(), SyfrahError> {
    input.parse::<std::net::IpAddr>().map_err(|_| {
        SyfrahError::validation(format!(
            "invalid IP address '{input}': must be a valid IPv4 or IPv6 address"
        ))
    })?;
    Ok(())
}

/// Validate a CIDR block — IPv4 or IPv6.
pub fn cidr_any(input: &str) -> Result<(), SyfrahError> {
    if input.contains(':') {
        cidr_v6(input)
    } else {
        cidr(input)
    }
}

/// Validate an IPv6 CIDR block.
///
/// Rules:
/// - Format: `<ipv6>/N`
/// - Valid IPv6 address
/// - Prefix length 0-128
pub fn cidr_v6(input: &str) -> Result<(), SyfrahError> {
    let parts: Vec<&str> = input.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(SyfrahError::validation(format!(
            "invalid IPv6 CIDR '{input}': must be in format <ipv6>/N (e.g., fd00::/48)"
        )));
    }

    // Validate IPv6 address
    parts[0].parse::<std::net::Ipv6Addr>().map_err(|_| {
        SyfrahError::validation(format!("invalid IPv6 CIDR '{input}': invalid IPv6 address"))
    })?;

    // Validate prefix length
    let prefix: u8 = parts[1].parse().map_err(|_| {
        SyfrahError::validation(format!(
            "invalid IPv6 CIDR '{input}': prefix length must be a number (0-128)"
        ))
    })?;
    if prefix > 128 {
        return Err(SyfrahError::validation(format!(
            "invalid IPv6 CIDR '{input}': prefix length must be 0-128, got {prefix}"
        )));
    }

    Ok(())
}

/// Validate a MAC address (colon-separated, lowercase).
///
/// Format: `aa:bb:cc:dd:ee:ff`
pub fn mac_address(input: &str) -> Result<[u8; 6], SyfrahError> {
    let parts: Vec<&str> = input.split(':').collect();
    if parts.len() != 6 {
        return Err(SyfrahError::validation(format!(
            "invalid MAC address '{input}': must be 6 hex pairs separated by colons (e.g., aa:bb:cc:dd:ee:ff)"
        )));
    }
    let mut bytes = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        if part.len() != 2 {
            return Err(SyfrahError::validation(format!(
                "invalid MAC address '{input}': each octet must be exactly 2 hex digits"
            )));
        }
        bytes[i] = u8::from_str_radix(part, 16).map_err(|_| {
            SyfrahError::validation(format!(
                "invalid MAC address '{input}': '{part}' is not valid hex"
            ))
        })?;
    }
    Ok(bytes)
}

/// Validate a hostname (RFC 952 / RFC 1123).
///
/// Rules:
/// - 1-253 characters total
/// - Labels separated by dots, each 1-63 characters
/// - Each label: alphanumeric + hyphens, starts with alphanumeric
pub fn hostname(input: &str) -> Result<(), SyfrahError> {
    if input.is_empty() {
        return Err(SyfrahError::validation("hostname cannot be empty"));
    }
    if input.len() > 253 {
        return Err(SyfrahError::validation(format!(
            "invalid hostname '{input}': must be at most 253 characters"
        )));
    }
    for label in input.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(SyfrahError::validation(format!(
                "invalid hostname '{input}': each label must be 1-63 characters"
            )));
        }
        if !label.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(SyfrahError::validation(format!(
                "invalid hostname '{input}': label '{label}' must start with alphanumeric"
            )));
        }
        if label.len() > 1 && !label.ends_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(SyfrahError::validation(format!(
                "invalid hostname '{input}': label '{label}' must end with alphanumeric"
            )));
        }
        for c in label.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' {
                return Err(SyfrahError::validation(format!(
                    "invalid hostname '{input}': character '{c}' not allowed in label '{label}'"
                )));
            }
        }
    }
    Ok(())
}

/// Validate a URL/endpoint.
///
/// Must start with `http://` or `https://` and have a non-empty host.
pub fn url(input: &str) -> Result<(), SyfrahError> {
    if !input.starts_with("http://") && !input.starts_with("https://") {
        return Err(SyfrahError::validation(format!(
            "invalid URL '{input}': must start with http:// or https://"
        )));
    }
    let after_scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or("");
    if after_scheme.is_empty() {
        return Err(SyfrahError::validation(format!(
            "invalid URL '{input}': missing host"
        )));
    }
    // Extract host (before first / or end)
    let host = after_scheme.split('/').next().unwrap_or("");
    let host_no_port = host.split(':').next().unwrap_or("");
    if host_no_port.is_empty() {
        return Err(SyfrahError::validation(format!(
            "invalid URL '{input}': missing host"
        )));
    }
    Ok(())
}

/// Validate a file path exists and is readable.
pub fn path_exists(input: &str) -> Result<(), SyfrahError> {
    let path = std::path::Path::new(input);
    if !path.exists() {
        return Err(SyfrahError::validation(format!(
            "path '{input}' does not exist"
        )));
    }
    Ok(())
}

/// Validate a file path exists and is a regular file.
pub fn file_exists(input: &str) -> Result<(), SyfrahError> {
    let path = std::path::Path::new(input);
    if !path.exists() {
        return Err(SyfrahError::validation(format!(
            "file '{input}' does not exist"
        )));
    }
    if !path.is_file() {
        return Err(SyfrahError::validation(format!(
            "'{input}' is not a regular file"
        )));
    }
    Ok(())
}

/// Parse and validate a port range (e.g., "8080-8090").
/// Returns (start, end) inclusive.
pub fn port_range(input: &str) -> Result<(u16, u16), SyfrahError> {
    let parts: Vec<&str> = input.split('-').collect();
    if parts.len() != 2 {
        return Err(SyfrahError::validation(format!(
            "invalid port range '{input}': must be in format START-END (e.g., 8080-8090)"
        )));
    }
    let start = port_str(parts[0])?;
    let end = port_str(parts[1])?;
    if start > end {
        return Err(SyfrahError::validation(format!(
            "invalid port range '{input}': start ({start}) must be <= end ({end})"
        )));
    }
    Ok((start, end))
}

/// Validate an email address (basic RFC 5321).
pub fn email(input: &str) -> Result<(), SyfrahError> {
    let parts: Vec<&str> = input.splitn(2, '@').collect();
    if parts.len() != 2 {
        return Err(SyfrahError::validation(format!(
            "invalid email '{input}': must contain exactly one @"
        )));
    }
    let local = parts[0];
    let domain = parts[1];
    if local.is_empty() || local.len() > 64 {
        return Err(SyfrahError::validation(format!(
            "invalid email '{input}': local part must be 1-64 characters"
        )));
    }
    if domain.is_empty() || !domain.contains('.') {
        return Err(SyfrahError::validation(format!(
            "invalid email '{input}': domain must contain at least one dot"
        )));
    }
    // Validate domain as hostname
    hostname(domain)?;
    Ok(())
}

/// Validate an endpoint string — either `IP:PORT` or `HOST:PORT`.
pub fn endpoint(input: &str) -> Result<(&str, u16), SyfrahError> {
    // Handle IPv6 [addr]:port
    if input.starts_with('[') {
        let close = input.find(']').ok_or_else(|| {
            SyfrahError::validation(format!(
                "invalid endpoint '{input}': missing closing ']' for IPv6 address"
            ))
        })?;
        let addr = &input[1..close];
        ipv6(addr)?;
        let rest = &input[close + 1..];
        if !rest.starts_with(':') {
            return Err(SyfrahError::validation(format!(
                "invalid endpoint '{input}': expected ':PORT' after IPv6 address"
            )));
        }
        let p = port_str(&rest[1..])?;
        return Ok((addr, p));
    }

    // IPv4 or hostname:port
    let last_colon = input.rfind(':').ok_or_else(|| {
        SyfrahError::validation(format!(
            "invalid endpoint '{input}': must be in format HOST:PORT or IP:PORT"
        ))
    })?;
    let host = &input[..last_colon];
    let port_s = &input[last_colon + 1..];
    let p = port_str(port_s)?;

    // Validate host is either IP or hostname
    if host.parse::<std::net::IpAddr>().is_err() {
        hostname(host)?;
    }

    Ok((host, p))
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

    // ── IPv4 validation ──

    #[test]
    fn ipv4_valid() {
        assert_eq!(ipv4("10.0.0.1").unwrap(), [10, 0, 0, 1]);
        assert_eq!(ipv4("192.168.1.100").unwrap(), [192, 168, 1, 100]);
        assert_eq!(ipv4("0.0.0.0").unwrap(), [0, 0, 0, 0]);
        assert_eq!(ipv4("255.255.255.255").unwrap(), [255, 255, 255, 255]);
    }

    #[test]
    fn ipv4_invalid() {
        assert!(ipv4("10.0.0").is_err());
        assert!(ipv4("10.0.0.999").is_err());
        assert!(ipv4("abc").is_err());
        assert!(ipv4("").is_err());
    }

    // ── IPv6 validation ──

    #[test]
    fn ipv6_valid() {
        assert!(ipv6("::1").is_ok());
        assert!(ipv6("fd01:2bf2:852d::1").is_ok());
        assert!(ipv6("2001:db8::1").is_ok());
        assert!(ipv6("fe80::1%eth0").is_err()); // scoped — not accepted by std parser
    }

    #[test]
    fn ipv6_invalid() {
        assert!(ipv6("not-ipv6").is_err());
        assert!(ipv6("10.0.0.1").is_err());
        assert!(ipv6("").is_err());
    }

    // ── IP address (any) ──

    #[test]
    fn ip_addr_valid() {
        assert!(ip_addr("10.0.0.1").is_ok());
        assert!(ip_addr("::1").is_ok());
        assert!(ip_addr("2001:db8::1").is_ok());
    }

    #[test]
    fn ip_addr_invalid() {
        assert!(ip_addr("nope").is_err());
    }

    // ── IPv6 CIDR ──

    #[test]
    fn cidr_v6_valid() {
        assert!(cidr_v6("fd00::/48").is_ok());
        assert!(cidr_v6("2001:db8::/32").is_ok());
        assert!(cidr_v6("::/0").is_ok());
        assert!(cidr_v6("::1/128").is_ok());
    }

    #[test]
    fn cidr_v6_invalid() {
        assert!(cidr_v6("fd00::").is_err()); // no prefix
        assert!(cidr_v6("fd00::/129").is_err()); // prefix > 128
        assert!(cidr_v6("not-ipv6/48").is_err());
    }

    #[test]
    fn cidr_any_dispatches() {
        assert!(cidr_any("10.0.0.0/8").is_ok());
        assert!(cidr_any("fd00::/48").is_ok());
    }

    // ── MAC address ──

    #[test]
    fn mac_valid() {
        let bytes = mac_address("aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(bytes, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert!(mac_address("00:11:22:33:44:55").is_ok());
    }

    #[test]
    fn mac_invalid() {
        assert!(mac_address("aa:bb:cc:dd:ee").is_err()); // 5 parts
        assert!(mac_address("aa:bb:cc:dd:ee:gg").is_err()); // bad hex
        assert!(mac_address("aabb.ccdd.eeff").is_err()); // wrong format
        assert!(mac_address("aa:bb:cc:dd:ee:fff").is_err()); // 3 chars
    }

    // ── Hostname ──

    #[test]
    fn hostname_valid() {
        assert!(hostname("example.com").is_ok());
        assert!(hostname("my-server").is_ok());
        assert!(hostname("a.b.c.d").is_ok());
        assert!(hostname("node-1").is_ok());
        assert!(hostname("A").is_ok()); // single char, uppercase OK for hostname
    }

    #[test]
    fn hostname_invalid() {
        assert!(hostname("").is_err());
        assert!(hostname("-start").is_err());
        assert!(hostname("a.b..c").is_err()); // empty label
        assert!(hostname(&"a".repeat(254)).is_err()); // too long
    }

    // ── URL ──

    #[test]
    fn url_valid() {
        assert!(url("https://example.com").is_ok());
        assert!(url("http://10.0.0.1:8080").is_ok());
        assert!(url("https://s3.eu-west.amazonaws.com/bucket").is_ok());
    }

    #[test]
    fn url_invalid() {
        assert!(url("ftp://example.com").is_err()); // wrong scheme
        assert!(url("https://").is_err()); // no host
        assert!(url("example.com").is_err()); // no scheme
    }

    // ── Path ──

    #[test]
    fn path_exists_valid() {
        // /tmp should always exist
        assert!(path_exists("/tmp").is_ok());
    }

    #[test]
    fn path_exists_invalid() {
        assert!(path_exists("/nonexistent/path/xyz").is_err());
    }

    #[test]
    fn file_exists_not_a_file() {
        // /tmp exists but is a directory
        assert!(file_exists("/tmp").is_err());
    }

    // ── Port range ──

    #[test]
    fn port_range_valid() {
        assert_eq!(port_range("8080-8090").unwrap(), (8080, 8090));
        assert_eq!(port_range("80-80").unwrap(), (80, 80)); // single port
    }

    #[test]
    fn port_range_invalid() {
        assert!(port_range("8090-8080").is_err()); // start > end
        assert!(port_range("80").is_err()); // no dash
        assert!(port_range("0-80").is_err()); // port 0
    }

    // ── Email ──

    #[test]
    fn email_valid() {
        assert!(email("user@example.com").is_ok());
        assert!(email("admin@sub.domain.org").is_ok());
    }

    #[test]
    fn email_invalid() {
        assert!(email("noat").is_err());
        assert!(email("@domain.com").is_err()); // empty local
        assert!(email("user@").is_err()); // empty domain
        assert!(email("user@nodot").is_err()); // no dot in domain
    }

    // ── Endpoint ──

    #[test]
    fn endpoint_ipv4() {
        let (host, p) = endpoint("10.0.0.1:8080").unwrap();
        assert_eq!(host, "10.0.0.1");
        assert_eq!(p, 8080);
    }

    #[test]
    fn endpoint_hostname() {
        let (host, p) = endpoint("example.com:443").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(p, 443);
    }

    #[test]
    fn endpoint_ipv6() {
        let (host, p) = endpoint("[::1]:8080").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(p, 8080);
    }

    #[test]
    fn endpoint_ipv6_full() {
        let (host, p) = endpoint("[fd01:2bf2:852d::1]:7200").unwrap();
        assert_eq!(host, "fd01:2bf2:852d::1");
        assert_eq!(p, 7200);
    }

    #[test]
    fn endpoint_invalid() {
        assert!(endpoint("noport").is_err());
        assert!(endpoint("[::1]").is_err()); // missing :port
        assert!(endpoint("10.0.0.1:0").is_err()); // port 0
    }
}
