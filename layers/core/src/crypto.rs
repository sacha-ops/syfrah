//! Cryptographic primitives for Syfrah.
//!
//! - **WireGuard keypairs**: Curve25519 key generation
//! - **Mesh secret**: 32-byte shared secret with `syf_sk_` prefix
//! - **Key derivation**: HKDF-SHA256 for deriving sub-keys
//! - **Hashing**: SHA-256 convenience
//! - **Random**: Cryptographically secure random bytes
//!
//! ```
//! use syfrah_core::crypto;
//!
//! let (private, public) = crypto::generate_wg_keypair();
//! let secret = crypto::MeshSecret::generate();
//! assert!(secret.to_string().starts_with("syf_sk_"));
//! ```

use hkdf::Hkdf;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

use crate::error::SyfrahError;

// ═══════════════════════════════════════════════════
// WireGuard keypair (Curve25519)
// ═══════════════════════════════════════════════════

/// Generate a WireGuard keypair. Returns (private_key_base64, public_key_base64).
pub fn generate_wg_keypair() -> (String, String) {
    use x25519_dalek::{PublicKey, StaticSecret};

    let rng = rand::rngs::OsRng;
    let private = StaticSecret::random_from_rng(rng);
    let public = PublicKey::from(&private);

    let priv_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        private.as_bytes(),
    );
    let pub_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        public.as_bytes(),
    );

    (priv_b64, pub_b64)
}

/// Derive a WireGuard public key from a private key (both base64).
pub fn wg_public_from_private(private_b64: &str) -> Result<String, SyfrahError> {
    use x25519_dalek::{PublicKey, StaticSecret};

    let private_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, private_b64)
            .map_err(|e| SyfrahError::validation(format!("invalid base64 private key: {e}")))?;

    if private_bytes.len() != 32 {
        return Err(SyfrahError::validation(format!(
            "private key must be 32 bytes, got {}",
            private_bytes.len()
        )));
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&private_bytes);
    let private = StaticSecret::from(key);
    let public = PublicKey::from(&private);

    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        public.as_bytes(),
    ))
}

// ═══════════════════════════════════════════════════
// Mesh secret
// ═══════════════════════════════════════════════════

const SECRET_PREFIX: &str = "syf_sk_";
const SECRET_BYTES: usize = 32;

/// The shared secret for a mesh. Only credential needed to join.
/// Format: `syf_sk_{base58(32 bytes)}`
#[derive(Clone)]
pub struct MeshSecret {
    bytes: [u8; SECRET_BYTES],
}

impl MeshSecret {
    /// Generate a new random mesh secret.
    pub fn generate() -> Self {
        let mut bytes = [0u8; SECRET_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Create from raw bytes.
    pub fn from_bytes(bytes: [u8; SECRET_BYTES]) -> Self {
        Self { bytes }
    }

    /// Get raw bytes.
    pub fn as_bytes(&self) -> &[u8; SECRET_BYTES] {
        &self.bytes
    }

    /// Derive a sub-key using HKDF-SHA256.
    pub fn derive(&self, domain: &str, length: usize) -> Vec<u8> {
        let hk = Hkdf::<Sha256>::new(Some(b"syfrah-v2"), &self.bytes);
        let mut okm = vec![0u8; length];
        hk.expand(domain.as_bytes(), &mut okm)
            .expect("HKDF expand failed");
        okm
    }

    /// Derive a 4-digit PIN from the secret (for peering).
    pub fn derive_pin(&self) -> String {
        let derived = self.derive("peering-pin", 4);
        let num = u32::from_be_bytes([derived[0], derived[1], derived[2], derived[3]]) % 10000;
        format!("{num:04}")
    }
}

impl fmt::Display for MeshSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            SECRET_PREFIX,
            bs58::encode(&self.bytes).into_string()
        )
    }
}

impl fmt::Debug for MeshSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MeshSecret(****)")
    }
}

impl FromStr for MeshSecret {
    type Err = SyfrahError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let encoded = s.strip_prefix(SECRET_PREFIX).ok_or_else(|| {
            SyfrahError::validation(format!("invalid secret: must start with '{SECRET_PREFIX}'"))
        })?;

        let bytes = bs58::decode(encoded)
            .into_vec()
            .map_err(|e| SyfrahError::validation(format!("invalid secret encoding: {e}")))?;

        if bytes.len() != SECRET_BYTES {
            return Err(SyfrahError::validation(format!(
                "invalid secret: expected {SECRET_BYTES} bytes, got {}",
                bytes.len()
            )));
        }

        let mut arr = [0u8; SECRET_BYTES];
        arr.copy_from_slice(&bytes);
        Ok(Self { bytes: arr })
    }
}

// ═══════════════════════════════════════════════════
// Hashing
// ═══════════════════════════════════════════════════

/// SHA-256 hash, returns hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 hash, returns raw bytes.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let hash = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hash);
    out
}

// ═══════════════════════════════════════════════════
// Random
// ═══════════════════════════════════════════════════

/// Generate `n` cryptographically secure random bytes.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

/// Generate a random alphanumeric string of given length.
pub fn random_string(len: usize) -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_keypair() {
        let (priv_key, pub_key) = generate_wg_keypair();
        assert!(!priv_key.is_empty());
        assert!(!pub_key.is_empty());
        // Base64 of 32 bytes = 44 chars
        assert_eq!(priv_key.len(), 44);
        assert_eq!(pub_key.len(), 44);
    }

    #[test]
    fn keypair_unique() {
        let (a, _) = generate_wg_keypair();
        let (b, _) = generate_wg_keypair();
        assert_ne!(a, b);
    }

    #[test]
    fn public_from_private() {
        let (priv_key, pub_key) = generate_wg_keypair();
        let derived = wg_public_from_private(&priv_key).unwrap();
        assert_eq!(derived, pub_key);
    }

    #[test]
    fn public_from_private_invalid() {
        assert!(wg_public_from_private("not-base64!!").is_err());
        assert!(wg_public_from_private("dG9vc2hvcnQ=").is_err()); // too short
    }

    #[test]
    fn mesh_secret_generate() {
        let s = MeshSecret::generate();
        let str = s.to_string();
        assert!(str.starts_with("syf_sk_"));
    }

    #[test]
    fn mesh_secret_roundtrip() {
        let s = MeshSecret::generate();
        let str = s.to_string();
        let parsed: MeshSecret = str.parse().unwrap();
        assert_eq!(s.as_bytes(), parsed.as_bytes());
    }

    #[test]
    fn mesh_secret_parse_invalid() {
        assert!("garbage".parse::<MeshSecret>().is_err());
        assert!("syf_sk_".parse::<MeshSecret>().is_err());
        assert!("syf_sk_tooshort".parse::<MeshSecret>().is_err());
    }

    #[test]
    fn mesh_secret_debug_is_masked() {
        let s = MeshSecret::generate();
        let debug = format!("{s:?}");
        assert_eq!(debug, "MeshSecret(****)");
        assert!(!debug.contains("syf_sk_"));
    }

    #[test]
    fn mesh_secret_derive() {
        let s = MeshSecret::generate();
        let key1 = s.derive("wireguard-psk", 32);
        let key2 = s.derive("peering-pin", 32);
        assert_eq!(key1.len(), 32);
        assert_ne!(key1, key2); // different domains → different keys
    }

    #[test]
    fn mesh_secret_derive_deterministic() {
        let s = MeshSecret::generate();
        let a = s.derive("test", 16);
        let b = s.derive("test", 16);
        assert_eq!(a, b);
    }

    #[test]
    fn mesh_secret_pin() {
        let s = MeshSecret::generate();
        let pin = s.derive_pin();
        assert_eq!(pin.len(), 4);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn sha256_hex_works() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha256_raw() {
        let h = sha256(b"hello");
        assert_eq!(h.len(), 32);
    }

    #[test]
    fn random_bytes_length() {
        assert_eq!(random_bytes(16).len(), 16);
        assert_eq!(random_bytes(0).len(), 0);
    }

    #[test]
    fn random_bytes_unique() {
        let a = random_bytes(32);
        let b = random_bytes(32);
        assert_ne!(a, b);
    }

    #[test]
    fn random_string_length() {
        let s = random_string(12);
        assert_eq!(s.len(), 12);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
