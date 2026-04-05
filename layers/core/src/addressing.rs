//! IPv6 ULA addressing for the mesh.
//!
//! Each mesh has a random `/48` ULA prefix: `fdXX:XXXX:XXXX::/48`
//! Each node gets a `/128` address derived from the prefix + its WireGuard public key.
//!
//! ```
//! use syfrah_core::addressing;
//!
//! let prefix = addressing::generate_mesh_prefix();
//! let node_addr = addressing::derive_node_address(&prefix, b"wg-public-key-bytes");
//! assert!(node_addr.to_string().starts_with("fd"));
//! ```

use sha2::{Digest, Sha256};
use std::net::Ipv6Addr;

/// Generate a random ULA /48 mesh prefix: `fdXX:XXXX:XXXX::/48`
///
/// Uses OS entropy (OsRng) for cryptographic addressing material.
pub fn generate_mesh_prefix() -> Ipv6Addr {
    let mut rng_bytes = [0u8; 5];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut rng_bytes);

    Ipv6Addr::new(
        0xfd00 | (rng_bytes[0] as u16),
        ((rng_bytes[1] as u16) << 8) | (rng_bytes[2] as u16),
        ((rng_bytes[3] as u16) << 8) | (rng_bytes[4] as u16),
        0,
        0,
        0,
        0,
        0,
    )
}

/// Derive a node's ULA /128 address from the mesh prefix and WireGuard public key.
///
/// Takes SHA256(wg_pubkey) and fills the lower 80 bits (segments 3-7),
/// keeping the /48 prefix (segments 0-2) intact.
pub fn derive_node_address(mesh_prefix: &Ipv6Addr, wg_public_key: &[u8]) -> Ipv6Addr {
    let hash = Sha256::digest(wg_public_key);
    let prefix = mesh_prefix.segments();

    Ipv6Addr::new(
        prefix[0],
        prefix[1],
        prefix[2],
        ((hash[0] as u16) << 8) | (hash[1] as u16),
        ((hash[2] as u16) << 8) | (hash[3] as u16),
        ((hash[4] as u16) << 8) | (hash[5] as u16),
        ((hash[6] as u16) << 8) | (hash[7] as u16),
        ((hash[8] as u16) << 8) | (hash[9] as u16),
    )
}

/// Extract the /48 prefix from a full /128 address.
pub fn extract_prefix(addr: &Ipv6Addr) -> Ipv6Addr {
    let s = addr.segments();
    Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0)
}

/// Format a /48 prefix for display: `fdXX:XXXX:XXXX::/48`
pub fn format_prefix(prefix: &Ipv6Addr) -> String {
    let s = prefix.segments();
    format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])
}

/// Check if an address is within a given /48 prefix.
pub fn is_in_prefix(addr: &Ipv6Addr, prefix: &Ipv6Addr) -> bool {
    let a = addr.segments();
    let p = prefix.segments();
    a[0] == p[0] && a[1] == p[1] && a[2] == p[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_ula() {
        let p = generate_mesh_prefix();
        let first = p.segments()[0];
        assert!((0xfd00..=0xfdff).contains(&first), "not ULA: {p}");
    }

    #[test]
    fn prefix_unique() {
        let a = generate_mesh_prefix();
        let b = generate_mesh_prefix();
        assert_ne!(a, b);
    }

    #[test]
    fn prefix_lower_bits_zero() {
        let p = generate_mesh_prefix();
        let s = p.segments();
        assert_eq!(s[3], 0);
        assert_eq!(s[4], 0);
        assert_eq!(s[5], 0);
        assert_eq!(s[6], 0);
        assert_eq!(s[7], 0);
    }

    #[test]
    fn derive_address_keeps_prefix() {
        let prefix = generate_mesh_prefix();
        let addr = derive_node_address(&prefix, b"test-key");
        assert!(is_in_prefix(&addr, &prefix));
    }

    #[test]
    fn derive_address_deterministic() {
        let prefix = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0, 0, 0, 0, 0);
        let a = derive_node_address(&prefix, b"key1");
        let b = derive_node_address(&prefix, b"key1");
        assert_eq!(a, b);
    }

    #[test]
    fn derive_address_different_keys() {
        let prefix = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0, 0, 0, 0, 0);
        let a = derive_node_address(&prefix, b"key1");
        let b = derive_node_address(&prefix, b"key2");
        assert_ne!(a, b);
    }

    #[test]
    fn derive_has_nonzero_lower_bits() {
        let prefix = generate_mesh_prefix();
        let addr = derive_node_address(&prefix, b"some-key");
        let s = addr.segments();
        // At least some of the lower segments should be non-zero
        assert!(s[3] != 0 || s[4] != 0 || s[5] != 0);
    }

    #[test]
    fn extract_prefix_works() {
        let prefix = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0, 0, 0, 0, 0);
        let addr = derive_node_address(&prefix, b"key");
        let extracted = extract_prefix(&addr);
        assert_eq!(extracted, prefix);
    }

    #[test]
    fn format_prefix_display() {
        let p = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0, 0, 0, 0, 0);
        assert_eq!(format_prefix(&p), "fd01:2bf2:852d::/48");
    }

    #[test]
    fn is_in_prefix_positive() {
        let p = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0, 0, 0, 0, 0);
        let a = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0x1234, 0x5678, 0, 0, 1);
        assert!(is_in_prefix(&a, &p));
    }

    #[test]
    fn is_in_prefix_negative() {
        let p = Ipv6Addr::new(0xfd01, 0x2bf2, 0x852d, 0, 0, 0, 0, 0);
        let a = Ipv6Addr::new(0xfd02, 0x2bf2, 0x852d, 0, 0, 0, 0, 1);
        assert!(!is_in_prefix(&a, &p));
    }
}
