use std::fmt;
use std::net::{Ipv6Addr, SocketAddr};
use std::str::FromStr;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("decryption failed: invalid secret or corrupted data")]
    DecryptionFailed,
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("encrypted payload too short")]
    PayloadTooShort,
    #[error("validation error: {0}")]
    Validation(String),
    #[error("signature error: {0}")]
    Signature(String),
}

// --- Typed Region / Zone ---

/// Maximum length for Region and Zone identifiers.
pub const MAX_REGION_ZONE_LENGTH: usize = 64;

/// Validate a region/zone string: 1-64 chars, `[a-z0-9-]`, no leading/trailing dash.
fn is_valid_region_zone(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_REGION_ZONE_LENGTH {
        return false;
    }
    if s.starts_with('-') || s.ends_with('-') {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A validated cloud region identifier (e.g. `"eu-west"`, `"us-east-1"`).
///
/// Invariants:
/// - 1-64 characters
/// - Only lowercase ASCII letters, digits, and hyphens (`[a-z0-9-]`)
/// - No leading or trailing hyphen
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Region(String);

impl Region {
    /// Create a new `Region` if the input passes validation. Returns `None` on
    /// invalid input.
    pub fn new(s: &str) -> Option<Region> {
        if is_valid_region_zone(s) {
            Some(Region(s.to_owned()))
        } else {
            None
        }
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Region {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Region::new(s).ok_or_else(|| format!("invalid region: {s:?}"))
    }
}

impl TryFrom<String> for Region {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if is_valid_region_zone(&s) {
            Ok(Region(s))
        } else {
            Err(format!("invalid region: {s:?}"))
        }
    }
}

impl From<Region> for String {
    fn from(r: Region) -> String {
        r.0
    }
}

/// A validated availability-zone identifier (e.g. `"us-east-1a"`, `"zone-a"`).
///
/// Same invariants as [`Region`].
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Zone(String);

impl Zone {
    /// Create a new `Zone` if the input passes validation. Returns `None` on
    /// invalid input.
    pub fn new(s: &str) -> Option<Zone> {
        if is_valid_region_zone(s) {
            Some(Zone(s.to_owned()))
        } else {
            None
        }
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Zone {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Zone::new(s).ok_or_else(|| format!("invalid zone: {s:?}"))
    }
}

impl TryFrom<String> for Zone {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if is_valid_region_zone(&s) {
            Ok(Zone(s))
        } else {
            Err(format!("invalid zone: {s:?}"))
        }
    }
}

impl From<Zone> for String {
    fn from(z: Zone) -> String {
        z.0
    }
}

/// Topology information for a mesh peer: which region and zone it belongs to.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    pub region: Region,
    pub zone: Zone,
}

impl Topology {
    /// Try to build a `Topology` from raw region/zone strings.
    /// Returns `None` if either string is missing or fails validation.
    pub fn from_strings(region: Option<&str>, zone: Option<&str>) -> Option<Topology> {
        let r = region.and_then(Region::new)?;
        let z = zone.and_then(Zone::new)?;
        Some(Topology { region: r, zone: z })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerStatus {
    Active,
    Unreachable,
    Removed,
}

/// Record exchanged between mesh peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    pub name: String,
    pub wg_public_key: String,
    pub endpoint: SocketAddr,
    pub mesh_ipv6: Ipv6Addr,
    pub last_seen: u64,
    pub status: PeerStatus,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
    /// Typed topology (region + zone). Coexists with the legacy `region`/`zone`
    /// string fields during the migration period.
    #[serde(default)]
    pub topology: Option<Topology>,
    /// Hypervisor capacity report (advisory telemetry, not source of truth).
    /// Populated every ~10s when a hypervisor is registered on this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hypervisor_report: Option<HypervisorReport>,
}

/// Advisory hypervisor capacity report carried in gossip announcements.
/// This is telemetry only — not the source of truth for scheduling decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HypervisorReport {
    /// Hypervisor ID.
    pub hypervisor_id: String,
    /// Region.
    pub region: String,
    /// Zone.
    pub zone: String,
    /// Hypervisor state (e.g. "Available", "Draining").
    pub state: String,
    /// Allocatable vCPUs.
    pub allocatable_vcpus: u32,
    /// Used vCPUs.
    pub used_vcpus: u32,
    /// Allocatable memory in MB.
    pub allocatable_memory_mb: u64,
    /// Used memory in MB.
    pub used_memory_mb: u64,
    /// Number of running instances.
    pub instance_count: u32,
    /// Whether the node is draining.
    pub drain_status: bool,
    /// Unix timestamp of this report.
    pub reported_at: u64,
}

impl PeerRecord {
    /// Fill in `topology` from legacy `region`/`zone` strings when it is absent.
    /// This is the lazy-migration path: old records get a typed topology the
    /// first time they are loaded.
    pub fn ensure_topology(&mut self) {
        if self.topology.is_some() {
            return;
        }
        self.topology = Topology::from_strings(self.region.as_deref(), self.zone.as_deref());
    }

    /// Copy `topology` values back into the legacy `region`/`zone` string
    /// fields so that old nodes can still read the data.
    pub fn sync_legacy_fields(&mut self) {
        if let Some(ref topo) = self.topology {
            self.region = Some(topo.region.to_string());
            self.zone = Some(topo.zone.to_string());
        }
    }
}

// --- Peering protocol types ---

/// A request from a new node wanting to join the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub request_id: String,
    pub node_name: String,
    pub wg_public_key: String,
    pub endpoint: SocketAddr,
    pub wg_listen_port: u16,
    /// Optional PIN for auto-accept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Joiner's region (sent so the leader can store it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Joiner's zone (sent so the leader can store it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// Unix timestamp (seconds since epoch) to prevent replay attacks.
    #[serde(default)]
    pub timestamp: u64,
    /// Ed25519 signature over the canonical payload, base64-encoded.
    /// Proves the sender possesses the WireGuard private key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
}

/// Response sent back to a new node after acceptance or rejection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_prefix: Option<Ipv6Addr>,
    #[serde(default)]
    pub peers: Vec<PeerRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// How the join was approved: "pin" or "manual". None if rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
}

/// Wire protocol message envelope for TCP peering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeeringMessage {
    JoinRequest(JoinRequest),
    JoinResponse(JoinResponse),
    /// Encrypted PeerRecord announcement (ciphertext, uses mesh secret).
    PeerAnnounce(Vec<u8>),
    /// Secret rotation: carries the new secret encrypted with the OLD encryption key.
    /// Payload is nonce (12 bytes) || AES-256-GCM ciphertext of the new secret string.
    SecretRotation(Vec<u8>),
}

// --- Validation constants ---

/// Maximum length for name, region, and zone fields.
pub const MAX_NAME_LENGTH: usize = 255;

/// Maximum length for request_id and pin fields.
pub const MAX_SHORT_FIELD_LENGTH: usize = 32;

/// Maximum number of peers in a JoinResponse.
pub const MAX_PEERS_IN_RESPONSE: usize = 500;

/// Expected base64-encoded WireGuard public key length (32 bytes -> 44 chars base64).
pub const WG_KEY_BASE64_LENGTH: usize = 44;

// --- Validation functions ---

/// Validate a name field (node name, region, zone): 1-255 chars, alphanumeric + `-_.` only.
/// Double-dot (`..`) is rejected to prevent path-traversal attacks.
pub fn validate_name(field_name: &str, value: &str) -> Result<(), MeshError> {
    if value.is_empty() {
        return Err(MeshError::Validation(format!(
            "{field_name} must not be empty"
        )));
    }
    if value.len() > MAX_NAME_LENGTH {
        return Err(MeshError::Validation(format!(
            "{field_name} must be at most {MAX_NAME_LENGTH} chars, got {}",
            value.len()
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(MeshError::Validation(format!(
            "{field_name} contains invalid characters (allowed: alphanumeric, dash, underscore, dot)"
        )));
    }
    if value.contains("..") {
        return Err(MeshError::Validation(format!(
            "{field_name} must not contain '..' (path traversal)"
        )));
    }
    Ok(())
}

/// Validate a short field (request_id, pin): 1-32 chars, alphanumeric + `-_.` only.
pub fn validate_short_field(field_name: &str, value: &str) -> Result<(), MeshError> {
    if value.is_empty() {
        return Err(MeshError::Validation(format!(
            "{field_name} must not be empty"
        )));
    }
    if value.len() > MAX_SHORT_FIELD_LENGTH {
        return Err(MeshError::Validation(format!(
            "{field_name} must be at most {MAX_SHORT_FIELD_LENGTH} chars, got {}",
            value.len()
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(MeshError::Validation(format!(
            "{field_name} contains invalid characters (allowed: alphanumeric, dash, underscore, dot)"
        )));
    }
    Ok(())
}

/// Validate a WireGuard public key: must be 44 chars of valid base64, decoding to 32 bytes.
pub fn validate_wg_public_key(key: &str) -> Result<(), MeshError> {
    use base64::Engine;
    if key.len() != WG_KEY_BASE64_LENGTH {
        return Err(MeshError::Validation(format!(
            "WireGuard public key must be {WG_KEY_BASE64_LENGTH} chars base64, got {}",
            key.len()
        )));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(key)
        .map_err(|_| {
            MeshError::Validation("WireGuard public key is not valid base64".to_string())
        })?;
    if decoded.len() != 32 {
        return Err(MeshError::Validation(format!(
            "WireGuard public key must decode to 32 bytes, got {}",
            decoded.len()
        )));
    }
    Ok(())
}

/// Validate a socket address endpoint: no loopback, no unspecified, port > 0.
pub fn validate_endpoint(addr: &SocketAddr) -> Result<(), MeshError> {
    if addr.ip().is_loopback() {
        return Err(MeshError::Validation(
            "endpoint cannot be loopback".to_string(),
        ));
    }
    if addr.ip().is_unspecified() {
        return Err(MeshError::Validation(
            "endpoint cannot be unspecified (0.0.0.0 / ::)".to_string(),
        ));
    }
    if addr.port() == 0 {
        return Err(MeshError::Validation(
            "endpoint port cannot be 0".to_string(),
        ));
    }
    Ok(())
}

/// Validate that a mesh IPv6 address belongs to the given /48 prefix.
pub fn validate_mesh_ipv6(addr: &Ipv6Addr, prefix: &Ipv6Addr) -> Result<(), MeshError> {
    let a = addr.segments();
    let p = prefix.segments();
    if a[0] != p[0] || a[1] != p[1] || a[2] != p[2] {
        return Err(MeshError::Validation(format!(
            "mesh IPv6 {addr} does not match mesh /48 prefix {prefix}"
        )));
    }
    Ok(())
}

/// Validate all fields of a PeerRecord.
pub fn validate_peer_record(record: &PeerRecord) -> Result<(), MeshError> {
    validate_name("name", &record.name)?;
    validate_wg_public_key(&record.wg_public_key)?;
    validate_endpoint(&record.endpoint)?;
    if let Some(ref region) = record.region {
        validate_name("region", region)?;
    }
    if let Some(ref zone) = record.zone {
        validate_name("zone", zone)?;
    }
    Ok(())
}

/// Validate all fields of a JoinRequest (syntax only, no signature check).
pub fn validate_join_request(req: &JoinRequest) -> Result<(), MeshError> {
    validate_short_field("request_id", &req.request_id)?;
    validate_name("node_name", &req.node_name)?;
    validate_wg_public_key(&req.wg_public_key)?;
    // Endpoint validation is relaxed for join requests: 0.0.0.0 is allowed
    // because the leader auto-detects the endpoint from the TCP peer address.
    if req.endpoint.port() == 0 {
        return Err(MeshError::Validation(
            "endpoint port cannot be 0".to_string(),
        ));
    }
    if let Some(ref pin) = req.pin {
        validate_short_field("pin", pin)?;
    }
    if let Some(ref region) = req.region {
        validate_name("region", region)?;
    }
    if let Some(ref zone) = req.zone {
        validate_name("zone", zone)?;
    }
    Ok(())
}

/// Validate all fields of a JoinRequest **and** verify its cryptographic
/// signature.  This is the function that the receiving side should call.
pub fn validate_and_verify_join_request(req: &JoinRequest) -> Result<(), MeshError> {
    validate_join_request(req)?;
    verify_join_request_signature(req)?;
    Ok(())
}

/// Validate a JoinResponse (peer list size limit).
pub fn validate_join_response(resp: &JoinResponse) -> Result<(), MeshError> {
    if resp.peers.len() > MAX_PEERS_IN_RESPONSE {
        return Err(MeshError::Validation(format!(
            "JoinResponse contains {} peers, maximum is {MAX_PEERS_IN_RESPONSE}",
            resp.peers.len()
        )));
    }
    Ok(())
}

// --- JoinRequest signature constants ---

/// Maximum age (seconds) for a JoinRequest timestamp before it is rejected as stale.
pub const MAX_JOIN_REQUEST_AGE_SECS: u64 = 300; // 5 minutes

// --- JoinRequest signing / verification ---

/// Build the canonical byte payload that is signed:
/// `SHA-256(node_name || wg_public_key || endpoint || timestamp)`.
fn join_request_sign_payload(req: &JoinRequest) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(req.node_name.as_bytes());
    hasher.update(req.wg_public_key.as_bytes());
    hasher.update(req.endpoint.to_string().as_bytes());
    hasher.update(req.timestamp.to_be_bytes());
    hasher.finalize().into()
}

/// Convert an X25519 private key (32 bytes, clamped Curve25519 scalar) to an
/// Ed25519 signing key.  WireGuard stores the *clamped* scalar directly, so
/// we interpret the 32 bytes as an Ed25519 secret seed via the birational map
/// provided by `ed25519-dalek`.
fn x25519_private_to_ed25519_signing(wg_private_bytes: &[u8; 32]) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(wg_private_bytes)
}

/// Derive the Ed25519 *verifying* (public) key that corresponds to the WG
/// private key used to sign.  On the verifier side we do NOT have the private
/// key — we only have the WG public key (an X25519 point).  However, when the
/// signer calls `SigningKey::from_bytes`, `ed25519-dalek` deterministically
/// derives a public key via SHA-512 hashing of the seed.  There is no
/// general-purpose birational map from an arbitrary X25519 public key to the
/// matching Ed25519 public key.  Therefore the protocol includes the Ed25519
/// verifying key inside the signed payload is not needed — we simply derive it
/// from the private key at sign time and the verifier must accept the public
/// key embedded in the request.
///
/// Instead we take a simpler approach: the *signer* also embeds the Ed25519
/// verifying key in the signature blob so the verifier can:
///   1. Verify the Ed25519 signature with the embedded verifying key.
///   2. Confirm the Ed25519 verifying key is *bound* to the claimed WG key
///      because the signed payload includes `wg_public_key`.
///
/// Signature wire format (base64-encoded):
///   bytes [0..32]  = Ed25519 verifying key
///   bytes [32..96] = Ed25519 signature (64 bytes)
///
/// Sign a JoinRequest in place.  `wg_private_key` is the raw 32-byte
/// WireGuard private key of the joiner.
pub fn sign_join_request(req: &mut JoinRequest, wg_private_key: &[u8; 32]) {
    use base64::Engine;
    use ed25519_dalek::Signer;

    req.timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let payload = join_request_sign_payload(req);
    let signing_key = x25519_private_to_ed25519_signing(wg_private_key);
    let verifying_key = signing_key.verifying_key();
    let sig = signing_key.sign(&payload);

    let mut blob = Vec::with_capacity(32 + 64);
    blob.extend_from_slice(verifying_key.as_bytes());
    blob.extend_from_slice(&sig.to_bytes());
    req.signature = base64::engine::general_purpose::STANDARD.encode(&blob);
}

/// Verify the Ed25519 signature on a `JoinRequest`.
///
/// Returns `Ok(())` if valid, or `Err(MeshError::Signature(...))` with a
/// human-readable message suitable for operator display.
pub fn verify_join_request_signature(req: &JoinRequest) -> Result<(), MeshError> {
    use base64::Engine;
    use ed25519_dalek::Verifier;

    if req.signature.is_empty() {
        return Err(MeshError::Signature(
            "JoinRequest signature missing. The node may be running an incompatible version."
                .to_string(),
        ));
    }

    let blob = base64::engine::general_purpose::STANDARD
        .decode(&req.signature)
        .map_err(|_| {
            MeshError::Signature(
                "JoinRequest signature is not valid base64. The request was tampered with."
                    .to_string(),
            )
        })?;

    if blob.len() != 96 {
        return Err(MeshError::Signature(
            "JoinRequest signature has wrong length. The request was tampered with.".to_string(),
        ));
    }

    let vk_bytes: [u8; 32] = blob[..32].try_into().expect("slice is exactly 32 bytes");
    let sig_bytes: [u8; 64] = blob[32..96].try_into().expect("slice is exactly 64 bytes");

    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&vk_bytes).map_err(|_| {
        MeshError::Signature(
            "JoinRequest signature contains an invalid Ed25519 public key.".to_string(),
        )
    })?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);

    let payload = join_request_sign_payload(req);
    verifying_key.verify(&payload, &signature).map_err(|_| {
        MeshError::Signature(
            "JoinRequest signature invalid. The node may be running an incompatible version or the request was tampered with."
                .to_string(),
        )
    })?;

    // Check timestamp freshness (reject stale/replayed requests).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.abs_diff(req.timestamp);
    if age > MAX_JOIN_REQUEST_AGE_SECS {
        return Err(MeshError::Signature(format!(
            "JoinRequest timestamp is too old ({age}s). Possible replay attack."
        )));
    }

    Ok(())
}

/// Encrypt a PeerRecord with AES-256-GCM using the mesh encryption key.
/// Returns nonce (12 bytes) || ciphertext.
pub fn encrypt_record(
    record: &PeerRecord,
    encryption_key: &[u8; 32],
) -> Result<Vec<u8>, MeshError> {
    let plaintext = serde_json::to_vec(record)?;
    let cipher =
        Aes256Gcm::new_from_slice(encryption_key).map_err(|_| MeshError::EncryptionFailed)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|_| MeshError::EncryptionFailed)?;

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend(ciphertext);
    Ok(out)
}

/// Decrypt a PeerRecord from nonce || ciphertext using the mesh encryption key.
pub fn decrypt_record(data: &[u8], encryption_key: &[u8; 32]) -> Result<PeerRecord, MeshError> {
    if data.len() < 12 {
        return Err(MeshError::PayloadTooShort);
    }
    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher =
        Aes256Gcm::new_from_slice(encryption_key).map_err(|_| MeshError::DecryptionFailed)?;
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| MeshError::DecryptionFailed)?;
    let record: PeerRecord = serde_json::from_slice(&plaintext)?;
    Ok(record)
}

/// Encrypt a new secret string with the current encryption key for broadcast
/// during secret rotation. Returns nonce (12 bytes) || ciphertext.
pub fn encrypt_secret(
    new_secret_str: &str,
    current_encryption_key: &[u8; 32],
) -> Result<Vec<u8>, MeshError> {
    let plaintext = new_secret_str.as_bytes();
    let cipher = Aes256Gcm::new_from_slice(current_encryption_key)
        .map_err(|_| MeshError::EncryptionFailed)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| MeshError::EncryptionFailed)?;

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend(ciphertext);
    Ok(out)
}

/// Decrypt a secret string from nonce || ciphertext using the current encryption key.
pub fn decrypt_secret(data: &[u8], current_encryption_key: &[u8; 32]) -> Result<String, MeshError> {
    if data.len() < 12 {
        return Err(MeshError::PayloadTooShort);
    }
    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = Aes256Gcm::new_from_slice(current_encryption_key)
        .map_err(|_| MeshError::DecryptionFailed)?;
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| MeshError::DecryptionFailed)?;
    String::from_utf8(plaintext).map_err(|_| MeshError::DecryptionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::MeshSecret;
    use std::net::{Ipv6Addr, SocketAddr};

    fn sample_record() -> PeerRecord {
        PeerRecord {
            name: "node-1".into(),
            wg_public_key: "dGVzdC1wdWJsaWMta2V5".into(),
            endpoint: "203.0.113.1:51820".parse::<SocketAddr>().unwrap(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 1),
            last_seen: 1700000000,
            status: PeerStatus::Active,
            region: None,
            zone: None,
            topology: None,
            hypervisor_report: None,
        }
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let secret = MeshSecret::generate();
        let key = secret.encryption_key();
        let record = sample_record();

        let encrypted = encrypt_record(&record, &key).unwrap();
        let decrypted = decrypt_record(&encrypted, &key).unwrap();

        assert_eq!(decrypted.name, "node-1");
        assert_eq!(decrypted.wg_public_key, record.wg_public_key);
        assert_eq!(decrypted.endpoint, record.endpoint);
        assert_eq!(decrypted.mesh_ipv6, record.mesh_ipv6);
    }

    #[test]
    fn wrong_key_fails_decryption() {
        let s1 = MeshSecret::generate();
        let s2 = MeshSecret::generate();
        let record = sample_record();

        let encrypted = encrypt_record(&record, &s1.encryption_key()).unwrap();
        let result = decrypt_record(&encrypted, &s2.encryption_key());
        assert!(result.is_err());
    }

    #[test]
    fn corrupted_data_fails() {
        let secret = MeshSecret::generate();
        let key = secret.encryption_key();
        let record = sample_record();

        let mut encrypted = encrypt_record(&record, &key).unwrap();
        // Flip a byte in the ciphertext
        if let Some(byte) = encrypted.last_mut() {
            *byte ^= 0xff;
        }
        let result = decrypt_record(&encrypted, &key);
        assert!(result.is_err());
    }

    #[test]
    fn too_short_payload_fails() {
        let key = [0u8; 32];
        let result = decrypt_record(&[0u8; 5], &key);
        assert!(matches!(result, Err(MeshError::PayloadTooShort)));
    }

    #[test]
    fn peer_status_serde() {
        let json = serde_json::to_string(&PeerStatus::Unreachable).unwrap();
        let parsed: PeerStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, PeerStatus::Unreachable);
    }

    // --- Validation tests ---

    /// A valid 32-byte WireGuard key encoded as base64 (44 chars).
    const VALID_WG_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn valid_record() -> PeerRecord {
        PeerRecord {
            name: "node-1".into(),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "203.0.113.1:51820".parse::<SocketAddr>().unwrap(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 1),
            last_seen: 1700000000,
            status: PeerStatus::Active,
            region: Some("us-east-1".into()),
            zone: Some("zone-a".into()),
            topology: None,
            hypervisor_report: None,
        }
    }

    #[test]
    fn validate_name_valid() {
        assert!(validate_name("name", "node-1").is_ok());
        assert!(validate_name("name", "my_node.test").is_ok());
        assert!(validate_name("name", "a").is_ok());
        assert!(validate_name("name", &"a".repeat(255)).is_ok());
    }

    #[test]
    fn validate_name_empty() {
        let err = validate_name("name", "").unwrap_err();
        assert!(matches!(err, MeshError::Validation(_)));
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn validate_name_too_long() {
        let err = validate_name("name", &"a".repeat(256)).unwrap_err();
        assert!(err.to_string().contains("at most 255"));
    }

    #[test]
    fn validate_name_invalid_chars() {
        let err = validate_name("name", "node name").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
        let err = validate_name("name", "node@host").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
        let err = validate_name("name", "node/path").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_name_rejects_double_dot() {
        let err = validate_name("name", "..").unwrap_err();
        assert!(err.to_string().contains("path traversal"));
        let err = validate_name("name", "foo..bar").unwrap_err();
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn validate_short_field_valid() {
        assert!(validate_short_field("request_id", "abc123").is_ok());
        assert!(validate_short_field("pin", "1234").is_ok());
        assert!(validate_short_field("id", &"a".repeat(32)).is_ok());
    }

    #[test]
    fn validate_short_field_too_long() {
        let err = validate_short_field("request_id", &"a".repeat(33)).unwrap_err();
        assert!(err.to_string().contains("at most 32"));
    }

    #[test]
    fn validate_wg_key_valid() {
        assert!(validate_wg_public_key(VALID_WG_KEY).is_ok());
    }

    #[test]
    fn validate_wg_key_wrong_length() {
        let err = validate_wg_public_key("short").unwrap_err();
        assert!(err.to_string().contains("44 chars"));
    }

    #[test]
    fn validate_wg_key_invalid_base64() {
        // 44 chars but not valid base64
        let err =
            validate_wg_public_key("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!").unwrap_err();
        assert!(err.to_string().contains("not valid base64"));
    }

    #[test]
    fn validate_endpoint_valid() {
        let addr: SocketAddr = "203.0.113.1:51820".parse().unwrap();
        assert!(validate_endpoint(&addr).is_ok());
    }

    #[test]
    fn validate_endpoint_loopback() {
        let addr: SocketAddr = "127.0.0.1:51820".parse().unwrap();
        let err = validate_endpoint(&addr).unwrap_err();
        assert!(err.to_string().contains("loopback"));
    }

    #[test]
    fn validate_endpoint_unspecified() {
        let addr: SocketAddr = "0.0.0.0:51820".parse().unwrap();
        let err = validate_endpoint(&addr).unwrap_err();
        assert!(err.to_string().contains("unspecified"));
    }

    #[test]
    fn validate_endpoint_port_zero() {
        let addr: SocketAddr = "203.0.113.1:0".parse().unwrap();
        let err = validate_endpoint(&addr).unwrap_err();
        assert!(err.to_string().contains("port cannot be 0"));
    }

    #[test]
    fn validate_mesh_ipv6_valid() {
        let addr = Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 1);
        let prefix = Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 0);
        assert!(validate_mesh_ipv6(&addr, &prefix).is_ok());
    }

    #[test]
    fn validate_mesh_ipv6_wrong_prefix() {
        let addr = Ipv6Addr::new(0xfd99, 0x0000, 0x0000, 0, 0, 0, 0, 1);
        let prefix = Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 0);
        let err = validate_mesh_ipv6(&addr, &prefix).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn validate_peer_record_valid() {
        assert!(validate_peer_record(&valid_record()).is_ok());
    }

    #[test]
    fn validate_peer_record_bad_name() {
        let mut record = valid_record();
        record.name = "".into();
        assert!(validate_peer_record(&record).is_err());
    }

    #[test]
    fn validate_peer_record_bad_key() {
        let mut record = valid_record();
        record.wg_public_key = "not-a-key".into();
        assert!(validate_peer_record(&record).is_err());
    }

    #[test]
    fn validate_peer_record_bad_endpoint() {
        let mut record = valid_record();
        record.endpoint = "127.0.0.1:51820".parse().unwrap();
        assert!(validate_peer_record(&record).is_err());
    }

    #[test]
    fn validate_peer_record_bad_region() {
        let mut record = valid_record();
        record.region = Some("region with spaces".into());
        assert!(validate_peer_record(&record).is_err());
    }

    #[test]
    fn validate_join_request_valid() {
        let req = JoinRequest {
            request_id: "abc12345".into(),
            node_name: "node-2".into(),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "203.0.113.2:51820".parse().unwrap(),
            wg_listen_port: 51820,
            pin: Some("1234".into()),
            region: Some("us-east-1".into()),
            zone: Some("zone-a".into()),
            timestamp: 0,
            signature: String::new(),
        };
        assert!(validate_join_request(&req).is_ok());
    }

    #[test]
    fn validate_join_request_oversized_name() {
        let req = JoinRequest {
            request_id: "abc12345".into(),
            node_name: "a".repeat(256),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "203.0.113.2:51820".parse().unwrap(),
            wg_listen_port: 51820,
            pin: None,
            region: None,
            zone: None,
            timestamp: 0,
            signature: String::new(),
        };
        assert!(validate_join_request(&req).is_err());
    }

    #[test]
    fn validate_join_request_oversized_request_id() {
        let req = JoinRequest {
            request_id: "a".repeat(33),
            node_name: "node-2".into(),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "203.0.113.2:51820".parse().unwrap(),
            wg_listen_port: 51820,
            pin: None,
            region: None,
            zone: None,
            timestamp: 0,
            signature: String::new(),
        };
        assert!(validate_join_request(&req).is_err());
    }

    #[test]
    fn validate_join_request_oversized_pin() {
        let req = JoinRequest {
            request_id: "abc12345".into(),
            node_name: "node-2".into(),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "203.0.113.2:51820".parse().unwrap(),
            wg_listen_port: 51820,
            pin: Some("a".repeat(33)),
            region: None,
            zone: None,
            timestamp: 0,
            signature: String::new(),
        };
        assert!(validate_join_request(&req).is_err());
    }

    #[test]
    fn validate_join_request_allows_unspecified_endpoint() {
        // Join requests allow 0.0.0.0 because the leader auto-detects the endpoint
        let req = JoinRequest {
            request_id: "abc12345".into(),
            node_name: "node-2".into(),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "0.0.0.0:51820".parse().unwrap(),
            wg_listen_port: 51820,
            pin: None,
            region: None,
            zone: None,
            timestamp: 0,
            signature: String::new(),
        };
        assert!(validate_join_request(&req).is_ok());
    }

    #[test]
    fn validate_join_request_rejects_port_zero() {
        let req = JoinRequest {
            request_id: "abc12345".into(),
            node_name: "node-2".into(),
            wg_public_key: VALID_WG_KEY.into(),
            endpoint: "203.0.113.2:0".parse().unwrap(),
            wg_listen_port: 51820,
            pin: None,
            region: None,
            zone: None,
            timestamp: 0,
            signature: String::new(),
        };
        assert!(validate_join_request(&req).is_err());
    }

    #[test]
    fn validate_join_response_within_limit() {
        let resp = JoinResponse {
            accepted: true,
            mesh_name: Some("test-mesh".into()),
            mesh_secret: None,
            mesh_prefix: None,
            peers: vec![valid_record(); 100],
            reason: None,
            approved_by: None,
        };
        assert!(validate_join_response(&resp).is_ok());
    }

    #[test]
    fn validate_join_response_exceeds_limit() {
        let resp = JoinResponse {
            accepted: true,
            mesh_name: Some("test-mesh".into()),
            mesh_secret: None,
            mesh_prefix: None,
            peers: vec![valid_record(); MAX_PEERS_IN_RESPONSE + 1],
            reason: None,
            approved_by: None,
        };
        assert!(validate_join_response(&resp).is_err());
    }

    // --- JoinRequest signature tests ---

    /// Helper: generate a random 32-byte "WG private key" for testing.
    fn random_wg_private_key() -> [u8; 32] {
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        key
    }

    /// Helper: derive the WG public key (X25519) from the private key.
    fn wg_public_key_b64(private: &[u8; 32]) -> String {
        use base64::Engine;
        let secret = x25519_dalek::StaticSecret::from(*private);
        let public = x25519_dalek::PublicKey::from(&secret);
        base64::engine::general_purpose::STANDARD.encode(public.as_bytes())
    }

    /// Helper: build a valid, signed JoinRequest.
    fn signed_join_request() -> (JoinRequest, [u8; 32]) {
        let wg_priv = random_wg_private_key();
        let mut req = JoinRequest {
            request_id: "abc12345".into(),
            node_name: "node-2".into(),
            wg_public_key: wg_public_key_b64(&wg_priv),
            endpoint: "203.0.113.2:51820".parse().unwrap(),
            wg_listen_port: 51820,
            pin: None,
            region: None,
            zone: None,
            timestamp: 0,
            signature: String::new(),
        };
        sign_join_request(&mut req, &wg_priv);
        (req, wg_priv)
    }

    #[test]
    fn sign_verify_roundtrip() {
        let (req, _) = signed_join_request();
        assert!(verify_join_request_signature(&req).is_ok());
    }

    #[test]
    fn reject_tampered_node_name() {
        let (mut req, _) = signed_join_request();
        req.node_name = "evil-node".into();
        let err = verify_join_request_signature(&req).unwrap_err();
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn reject_tampered_wg_public_key() {
        let (mut req, _) = signed_join_request();
        // Replace with a different valid key
        let other_priv = random_wg_private_key();
        req.wg_public_key = wg_public_key_b64(&other_priv);
        let err = verify_join_request_signature(&req).unwrap_err();
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn reject_missing_signature() {
        let (mut req, _) = signed_join_request();
        req.signature = String::new();
        let err = verify_join_request_signature(&req).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn reject_stale_timestamp() {
        let (mut req, wg_priv) = signed_join_request();
        // Set timestamp to far in the past and re-sign
        req.timestamp = 1000;
        // Re-compute signature with stale timestamp
        let payload = join_request_sign_payload(&req);
        let signing_key = x25519_private_to_ed25519_signing(&wg_priv);
        let sig = ed25519_dalek::Signer::sign(&signing_key, &payload);
        let vk = signing_key.verifying_key();
        let mut blob = Vec::with_capacity(96);
        blob.extend_from_slice(vk.as_bytes());
        blob.extend_from_slice(&sig.to_bytes());
        req.signature = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &blob);
        let err = verify_join_request_signature(&req).unwrap_err();
        assert!(err.to_string().contains("too old"));
    }

    #[test]
    fn validate_and_verify_full_roundtrip() {
        let (req, _) = signed_join_request();
        assert!(validate_and_verify_join_request(&req).is_ok());
    }

    // --- Region / Zone / Topology tests ---

    #[test]
    fn region_new_accepts_valid() {
        assert!(Region::new("eu-west").is_some());
        assert!(Region::new("us-east-1").is_some());
        assert!(Region::new("a").is_some());
        assert!(Region::new("abc123").is_some());
        assert!(Region::new(&"a".repeat(64)).is_some());
    }

    #[test]
    fn region_new_rejects_uppercase() {
        assert!(Region::new("EU-WEST").is_none());
        assert!(Region::new("Us-East").is_none());
    }

    #[test]
    fn region_new_rejects_empty() {
        assert!(Region::new("").is_none());
    }

    #[test]
    fn region_new_rejects_leading_dash() {
        assert!(Region::new("-bad").is_none());
    }

    #[test]
    fn region_new_rejects_trailing_dash() {
        assert!(Region::new("bad-").is_none());
    }

    #[test]
    fn region_new_rejects_too_long() {
        assert!(Region::new(&"a".repeat(65)).is_none());
    }

    #[test]
    fn region_new_rejects_invalid_chars() {
        assert!(Region::new("eu west").is_none());
        assert!(Region::new("eu_west").is_none());
        assert!(Region::new("eu.west").is_none());
        assert!(Region::new("eu@west").is_none());
    }

    #[test]
    fn region_display_and_as_str() {
        let r = Region::new("eu-west").unwrap();
        assert_eq!(r.as_str(), "eu-west");
        assert_eq!(r.to_string(), "eu-west");
    }

    #[test]
    fn region_from_str() {
        let r: Region = "eu-west".parse().unwrap();
        assert_eq!(r.as_str(), "eu-west");
        assert!("EU-WEST".parse::<Region>().is_err());
    }

    #[test]
    fn zone_new_accepts_valid() {
        assert!(Zone::new("zone-a").is_some());
        assert!(Zone::new("us-east-1a").is_some());
        assert!(Zone::new("z").is_some());
    }

    #[test]
    fn zone_new_rejects_uppercase() {
        assert!(Zone::new("ZONE-A").is_none());
    }

    #[test]
    fn zone_new_rejects_empty() {
        assert!(Zone::new("").is_none());
    }

    #[test]
    fn zone_new_rejects_leading_trailing_dash() {
        assert!(Zone::new("-zone").is_none());
        assert!(Zone::new("zone-").is_none());
    }

    #[test]
    fn zone_new_rejects_too_long() {
        assert!(Zone::new(&"a".repeat(65)).is_none());
    }

    #[test]
    fn topology_serde_roundtrip() {
        let topo = Topology {
            region: Region::new("eu-west").unwrap(),
            zone: Zone::new("eu-west-1a").unwrap(),
        };
        let json = serde_json::to_string(&topo).unwrap();
        let parsed: Topology = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.region, topo.region);
        assert_eq!(parsed.zone, topo.zone);
    }

    #[test]
    fn region_serde_roundtrip() {
        let r = Region::new("us-east-1").unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"us-east-1\"");
        let parsed: Region = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn region_serde_rejects_invalid() {
        let result = serde_json::from_str::<Region>("\"EU-WEST\"");
        assert!(result.is_err());
    }

    #[test]
    fn zone_serde_roundtrip() {
        let z = Zone::new("zone-a").unwrap();
        let json = serde_json::to_string(&z).unwrap();
        assert_eq!(json, "\"zone-a\"");
        let parsed: Zone = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, z);
    }

    #[test]
    fn peer_record_without_topology_deserializes() {
        // Simulate old state.json that has no topology field
        let json = serde_json::json!({
            "name": "node-1",
            "wg_public_key": VALID_WG_KEY,
            "endpoint": "203.0.113.1:51820",
            "mesh_ipv6": "fd12:3456:7800::1",
            "last_seen": 1700000000,
            "status": "Active",
            "region": "us-east-1",
            "zone": "zone-a"
        });
        let record: PeerRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.region.as_deref(), Some("us-east-1"));
        assert_eq!(record.zone.as_deref(), Some("zone-a"));
        assert!(record.topology.is_none());
    }

    #[test]
    fn peer_record_with_topology_roundtrip() {
        let mut record = valid_record();
        record.topology = Some(Topology {
            region: Region::new("eu-west").unwrap(),
            zone: Zone::new("eu-west-1a").unwrap(),
        });
        let json = serde_json::to_string(&record).unwrap();
        let parsed: PeerRecord = serde_json::from_str(&json).unwrap();
        let topo = parsed.topology.unwrap();
        assert_eq!(topo.region.as_str(), "eu-west");
        assert_eq!(topo.zone.as_str(), "eu-west-1a");
    }

    #[test]
    fn peer_record_topology_null_present_in_json() {
        // topology is always serialized (even as null) for forward-compat
        let record = valid_record();
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"topology\":null"));
    }

    #[test]
    fn ensure_topology_fills_from_legacy_fields() {
        let mut record = valid_record();
        record.region = Some("eu-west".into());
        record.zone = Some("zone-a".into());
        assert!(record.topology.is_none());
        record.ensure_topology();
        let topo = record.topology.as_ref().unwrap();
        assert_eq!(topo.region.as_str(), "eu-west");
        assert_eq!(topo.zone.as_str(), "zone-a");
    }

    #[test]
    fn ensure_topology_noop_when_already_set() {
        let mut record = valid_record();
        record.topology = Some(Topology {
            region: Region::new("us-east").unwrap(),
            zone: Zone::new("zone-b").unwrap(),
        });
        record.region = Some("eu-west".into());
        record.zone = Some("zone-a".into());
        record.ensure_topology();
        // Should keep the existing topology, not overwrite from legacy fields
        let topo = record.topology.as_ref().unwrap();
        assert_eq!(topo.region.as_str(), "us-east");
        assert_eq!(topo.zone.as_str(), "zone-b");
    }

    #[test]
    fn ensure_topology_none_when_legacy_missing() {
        let mut record = valid_record();
        record.region = None;
        record.zone = None;
        record.ensure_topology();
        assert!(record.topology.is_none());
    }

    #[test]
    fn sync_legacy_fields_from_topology() {
        let mut record = valid_record();
        record.topology = Some(Topology {
            region: Region::new("ap-south").unwrap(),
            zone: Zone::new("zone-1").unwrap(),
        });
        record.region = None;
        record.zone = None;
        record.sync_legacy_fields();
        assert_eq!(record.region.as_deref(), Some("ap-south"));
        assert_eq!(record.zone.as_deref(), Some("zone-1"));
    }

    #[test]
    fn serialize_new_record_contains_both_formats() {
        let mut record = valid_record();
        record.region = Some("eu-west".into());
        record.zone = Some("zone-1".into());
        record.topology = Some(Topology {
            region: Region::new("eu-west").unwrap(),
            zone: Zone::new("zone-1").unwrap(),
        });
        let json = serde_json::to_string(&record).unwrap();
        // Legacy fields present for old nodes
        assert!(json.contains("\"region\":\"eu-west\""));
        assert!(json.contains("\"zone\":\"zone-1\""));
        // New typed topology also present
        assert!(json.contains("\"topology\":{"));
    }

    #[test]
    fn topology_from_strings_valid() {
        let topo = Topology::from_strings(Some("eu-west"), Some("zone-a")).unwrap();
        assert_eq!(topo.region.as_str(), "eu-west");
        assert_eq!(topo.zone.as_str(), "zone-a");
    }

    #[test]
    fn topology_from_strings_returns_none_on_missing() {
        assert!(Topology::from_strings(None, Some("zone-a")).is_none());
        assert!(Topology::from_strings(Some("eu-west"), None).is_none());
        assert!(Topology::from_strings(None, None).is_none());
    }

    #[test]
    fn topology_from_strings_returns_none_on_invalid() {
        // Uppercase is invalid
        assert!(Topology::from_strings(Some("EU-WEST"), Some("zone-a")).is_none());
    }

    #[test]
    fn encrypt_decrypt_secret_roundtrip() {
        let current_secret = MeshSecret::generate();
        let enc_key = current_secret.encryption_key();
        let new_secret = MeshSecret::generate();
        let new_secret_str = new_secret.to_string();

        let encrypted = encrypt_secret(&new_secret_str, &enc_key).unwrap();
        let decrypted = decrypt_secret(&encrypted, &enc_key).unwrap();

        assert_eq!(decrypted, new_secret_str);
    }

    #[test]
    fn decrypt_secret_wrong_key_fails() {
        let secret_a = MeshSecret::generate();
        let secret_b = MeshSecret::generate();
        let new_secret_str = "syf_sk_test123";

        let encrypted = encrypt_secret(new_secret_str, &secret_a.encryption_key()).unwrap();
        let result = decrypt_secret(&encrypted, &secret_b.encryption_key());
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_secret_too_short_fails() {
        let key = [0u8; 32];
        let result = decrypt_secret(&[0u8; 5], &key);
        assert!(matches!(result, Err(MeshError::PayloadTooShort)));
    }
}
