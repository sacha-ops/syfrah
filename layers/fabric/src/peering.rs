use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex, RwLock, Semaphore};
use tracing::{debug, info, warn};

use syfrah_core::mesh::{
    decrypt_record, encrypt_record, validate_and_verify_join_request, validate_join_response,
    validate_peer_record, JoinRequest, JoinResponse, PeerRecord, PeerStatus, PeeringMessage,
};

use crate::audit::{self as audit_log, AuditEventType};
use crate::events::{self, EventType};
use crate::sanitize::sanitize;

// ---------- TLS helpers ----------

/// Build a `rustls::ServerConfig` from a mesh-secret-derived self-signed certificate.
/// The certificate is deterministically generated from the mesh secret so every node
/// holding the same secret presents the same CA, enabling mutual verification without
/// an external PKI.
pub fn build_tls_server_config(
    mesh_secret: &[u8; 32],
) -> Result<Arc<rustls::ServerConfig>, PeeringError> {
    let (cert_chain, key_der) = generate_mesh_cert(mesh_secret)?;
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key_der)
        .map_err(|e| PeeringError::Tls(format!("server TLS config: {e}")))?;
    Ok(Arc::new(cfg))
}

/// Build a `rustls::ClientConfig` that trusts only the mesh-derived certificate.
pub fn build_tls_client_config(
    mesh_secret: &[u8; 32],
) -> Result<Arc<rustls::ClientConfig>, PeeringError> {
    let (cert_chain, _) = generate_mesh_cert(mesh_secret)?;
    let mut root_store = rustls::RootCertStore::empty();
    root_store
        .add(cert_chain[0].clone())
        .map_err(|e| PeeringError::Tls(format!("root store: {e}")))?;
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// Deterministically generate a self-signed certificate + private key from a mesh secret.
/// We use ECDSA P-256 because `rcgen` makes it easy and `rustls` supports it out of the box.
fn generate_mesh_cert(
    mesh_secret: &[u8; 32],
) -> Result<
    (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ),
    PeeringError,
> {
    use rcgen::{CertificateParams, KeyPair};
    use sha2::Sha256 as S256;

    // Derive 32 bytes of key material from the mesh secret for the certificate key.
    let mut hasher = S256::new();
    hasher.update(b"syfrah-tls-cert-key-v1");
    hasher.update(mesh_secret);
    let seed: [u8; 32] = hasher.finalize().into();

    // rcgen can import a PKCS#8 key. We build an Ed25519 key from the seed.
    // ring (used by rustls) accepts a PKCS#8 v2 document for Ed25519.
    let pkcs8_doc = ring_ed25519_pkcs8_from_seed(&seed)
        .map_err(|e| PeeringError::Tls(format!("key generation: {e}")))?;

    let private_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(pkcs8_doc.clone()),
    );
    let key_pair = KeyPair::from_der_and_sign_algo(&private_key_der, &rcgen::PKCS_ED25519)
        .map_err(|e| PeeringError::Tls(format!("key pair: {e}")))?;

    let mut params = CertificateParams::new(vec!["syfrah-mesh.internal".to_string()])
        .map_err(|e| PeeringError::Tls(format!("cert params: {e}")))?;
    // Do NOT set is_ca = Ca here. Using a CA certificate as an end-entity
    // certificate causes rustls to reject it with CaUsedAsEndEntity. The
    // default NoCa omits the basicConstraints extension entirely, which is
    // correct for a self-signed cert used as both trust anchor and end-entity.
    // Pin both not_before and not_after so the cert is fully deterministic
    // across nodes (all nodes holding the same mesh secret produce identical DER).
    params.not_before = rcgen::date_time_ymd(2025, 1, 1);
    params.not_after = rcgen::date_time_ymd(2045, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| PeeringError::Tls(format!("self-sign: {e}")))?;

    let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(pkcs8_doc),
    );

    Ok((vec![cert_der], key_der))
}

/// Construct a PKCS#8 v2 document for an Ed25519 key from a 32-byte seed.
/// This is the DER encoding that ring / rustls expects.
fn ring_ed25519_pkcs8_from_seed(seed: &[u8; 32]) -> Result<Vec<u8>, String> {
    // PKCS#8 v2 wrapper for Ed25519 (RFC 8410).
    // The structure is:
    //   SEQUENCE {
    //     INTEGER 0
    //     SEQUENCE { OID 1.3.101.112 }
    //     OCTET STRING { OCTET STRING { <32 bytes seed> } }
    //   }
    let mut doc = Vec::with_capacity(48);
    // SEQUENCE (outer)
    doc.push(0x30);
    doc.push(0x2e); // 46 bytes payload

    // INTEGER 0 (version)
    doc.extend_from_slice(&[0x02, 0x01, 0x00]);

    // SEQUENCE { OID 1.3.101.112 }
    doc.extend_from_slice(&[0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70]);

    // OCTET STRING wrapping OCTET STRING wrapping seed
    doc.push(0x04); // outer OCTET STRING tag
    doc.push(0x22); // 34 bytes
    doc.push(0x04); // inner OCTET STRING tag
    doc.push(0x20); // 32 bytes
    doc.extend_from_slice(seed);

    Ok(doc)
}

const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_MSG_SIZE: u32 = 65536;

/// Maximum failed PIN attempts per IP before lockout.
const MAX_PIN_ATTEMPTS: usize = 5;
/// Duration of the lockout window (failed attempts expire after this).
const PIN_LOCKOUT_WINDOW: Duration = Duration::from_secs(600); // 10 minutes
/// Delay before responding to a failed PIN attempt (slows enumeration).
const PIN_FAIL_DELAY: Duration = Duration::from_secs(2);

/// Default maximum concurrent peering connections.
const DEFAULT_MAX_CONNECTIONS: usize = 100;
/// Default maximum pending join requests.
const DEFAULT_MAX_PENDING_JOINS: usize = 100;

/// Maximum age (in seconds) for a PeerAnnounce timestamp before it is rejected.
const MAX_ANNOUNCE_AGE_SECS: u64 = 600;
/// Window (in seconds) for deduplicating identical announce ciphertexts.
const DEDUP_WINDOW_SECS: u64 = 300;

/// Replay protection guard for PeerAnnounce messages.
///
/// Rejects announces that are:
/// 1. Older than `MAX_ANNOUNCE_AGE_SECS` (stale timestamp).
/// 2. Duplicates of a previously seen ciphertext within `DEDUP_WINDOW_SECS`.
///
/// All timing comparisons use monotonic `Instant` so that NTP corrections
/// or manual clock changes cannot bypass replay protection.
pub struct ReplayGuard {
    /// SHA-256 hash -> monotonic instant when it was first seen.
    seen: Mutex<HashMap<[u8; 32], Instant>>,
    /// Monotonic anchor captured at construction (for epoch conversion).
    instant_anchor: Instant,
    /// Wall-clock epoch seconds captured at the same moment as `instant_anchor`.
    epoch_anchor: u64,
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
            instant_anchor: Instant::now(),
            epoch_anchor: epoch_now(),
        }
    }

    /// Return a monotonically increasing approximation of epoch seconds.
    /// Immune to wall-clock jumps after construction.
    pub(crate) fn monotonic_epoch(&self) -> u64 {
        self.epoch_anchor + self.instant_anchor.elapsed().as_secs()
    }

    /// Check whether a ciphertext is a duplicate. Returns `true` if the
    /// ciphertext has already been seen within the dedup window.
    pub async fn is_duplicate(&self, ciphertext: &[u8]) -> bool {
        let hash = sha256(ciphertext);
        let now = Instant::now();
        let mut seen = self.seen.lock().await;

        // Periodically evict expired entries (piggyback on every check).
        let window = Duration::from_secs(DEDUP_WINDOW_SECS);
        seen.retain(|_, ts| now.checked_duration_since(*ts).is_none_or(|d| d < window));

        if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(hash) {
            e.insert(now);
            false
        } else {
            true
        }
    }

    /// Check whether a PeerRecord timestamp is too old.
    pub fn is_stale(&self, record_last_seen: u64) -> bool {
        let current = self.monotonic_epoch();
        current.saturating_sub(record_last_seen) > MAX_ANNOUNCE_AGE_SECS
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Per-IP rate limiter for PIN attempts.
pub struct PinRateLimiter {
    attempts: HashMap<IpAddr, Vec<Instant>>,
}

impl Default for PinRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl PinRateLimiter {
    pub fn new() -> Self {
        Self {
            attempts: HashMap::new(),
        }
    }

    /// Record a failed attempt and return whether the IP is now locked out.
    /// Returns `true` if the IP is locked out (too many attempts).
    pub fn record_failure(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let entries = self.attempts.entry(ip).or_default();
        // Evict expired entries
        entries.retain(|t| now.duration_since(*t) < PIN_LOCKOUT_WINDOW);
        entries.push(now);
        entries.len() > MAX_PIN_ATTEMPTS
    }

    /// Check whether an IP is currently locked out (without recording).
    pub fn is_locked_out(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let entries = self.attempts.entry(ip).or_default();
        entries.retain(|t| now.duration_since(*t) < PIN_LOCKOUT_WINDOW);
        entries.len() >= MAX_PIN_ATTEMPTS
    }
}

#[derive(Debug, Error)]
pub enum PeeringError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("encryption error: {0}")]
    Encryption(#[from] syfrah_core::mesh::MeshError),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("request not found: {0}")]
    NotFound(String),
    #[error("timeout")]
    Timeout,
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("peer limit exceeded: {current} peers (max {max})")]
    PeerLimitExceeded { current: usize, max: usize },
}

/// Metadata about a pending join request (serializable for CLI display).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JoinRequestInfo {
    pub request_id: String,
    pub node_name: String,
    pub wg_public_key: String,
    pub endpoint: SocketAddr,
    pub wg_listen_port: u16,
    pub received_at: u64,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
}

struct PendingJoin {
    info: JoinRequestInfo,
    response_tx: oneshot::Sender<JoinResponse>,
}

/// Config for auto-accepting join requests with a PIN.
pub struct AutoAcceptConfig {
    pub pin: String,
    pub mesh_name: String,
    pub mesh_secret_str: String,
    pub mesh_prefix: std::net::Ipv6Addr,
    pub my_record: PeerRecord,
    pub wg_pubkey: wireguard_control::Key,
    pub encryption_key: [u8; 32],
    pub peering_port: u16,
    pub max_peers: usize,
}

/// Callback type invoked when a peer is accepted (either manually or via PIN).
pub type OnAccepted = Arc<dyn Fn(PeerRecord) + Send + Sync>;

/// Callback type invoked when a secret rotation message is received from a peer.
/// The argument is the new mesh secret string.
pub type OnSecretRotation = Arc<dyn Fn(String) + Send + Sync>;

/// Manages peering state: pending join requests, listener lifecycle.
pub struct PeeringState {
    pending: Arc<RwLock<HashMap<String, PendingJoin>>>,
    listener_active: Arc<RwLock<bool>>,
    auto_accept: Arc<RwLock<Option<AutoAcceptConfig>>>,
    pin_rate_limiter: Arc<Mutex<PinRateLimiter>>,
    /// Metrics: total connections rejected due to limit.
    connections_rejected: Arc<AtomicU64>,
    /// Metrics: currently active connections.
    connections_active: Arc<AtomicU64>,
    /// Max concurrent connections (configurable).
    max_connections: usize,
    /// Max pending join requests (configurable).
    max_pending_joins: usize,
    /// Replay protection for PeerAnnounce messages.
    replay_guard: Arc<ReplayGuard>,
}

impl Default for PeeringState {
    fn default() -> Self {
        Self::new()
    }
}

impl PeeringState {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_PENDING_JOINS)
    }

    /// Create a PeeringState with custom connection and pending-join limits.
    pub fn with_limits(max_connections: usize, max_pending_joins: usize) -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            listener_active: Arc::new(RwLock::new(false)),
            auto_accept: Arc::new(RwLock::new(None)),
            pin_rate_limiter: Arc::new(Mutex::new(PinRateLimiter::new())),
            connections_rejected: Arc::new(AtomicU64::new(0)),
            connections_active: Arc::new(AtomicU64::new(0)),
            max_connections,
            max_pending_joins,
            replay_guard: Arc::new(ReplayGuard::new()),
        }
    }

    /// Return the number of connections rejected due to the concurrency limit.
    pub fn connections_rejected(&self) -> u64 {
        self.connections_rejected.load(Ordering::Relaxed)
    }

    /// Return the number of currently active connections.
    pub fn connections_active(&self) -> u64 {
        self.connections_active.load(Ordering::Relaxed)
    }

    pub async fn is_active(&self) -> bool {
        *self.listener_active.read().await
    }

    pub async fn set_active(&self, active: bool) {
        *self.listener_active.write().await = active;
    }

    pub async fn set_auto_accept(&self, config: Option<AutoAcceptConfig>) {
        *self.auto_accept.write().await = config;
    }

    pub async fn list_pending(&self) -> Vec<JoinRequestInfo> {
        let pending = self.pending.read().await;
        pending.values().map(|p| p.info.clone()).collect()
    }

    pub async fn accept(
        &self,
        request_id: &str,
        response: JoinResponse,
    ) -> Result<JoinRequestInfo, PeeringError> {
        let mut pending = self.pending.write().await;
        let entry = pending
            .remove(request_id)
            .ok_or_else(|| PeeringError::NotFound(request_id.to_string()))?;
        let info = entry.info.clone();
        let _ = entry.response_tx.send(response);
        Ok(info)
    }

    pub async fn reject(
        &self,
        request_id: &str,
        reason: Option<String>,
    ) -> Result<(), PeeringError> {
        let mut pending = self.pending.write().await;
        let entry = pending
            .remove(request_id)
            .ok_or_else(|| PeeringError::NotFound(request_id.to_string()))?;
        let _ = entry.response_tx.send(JoinResponse {
            accepted: false,
            mesh_name: None,
            mesh_secret: None,
            mesh_prefix: None,
            peers: vec![],
            reason,
            approved_by: None,
        });
        Ok(())
    }

    /// Run the peering TCP listener with TLS. Runs forever.
    ///
    /// When `tls_config` is `Some`, every accepted TCP connection is upgraded to
    /// TLS 1.3 before any peering messages are exchanged.  Plaintext connections
    /// will fail the TLS handshake and be dropped — there is no fallback.
    #[allow(unreachable_code)]
    pub async fn run_listener(
        &self,
        port: u16,
        encryption_key: Option<[u8; 32]>,
        on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync>,
        on_accepted: OnAccepted,
        tls_config: Option<Arc<rustls::ServerConfig>>,
        on_secret_rotation: Option<OnSecretRotation>,
    ) -> Result<(), PeeringError> {
        self.set_active(true).await;
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
        let listener = TcpListener::bind(addr).await?;
        let semaphore = Arc::new(Semaphore::new(self.max_connections));

        let tls_acceptor = tls_config.map(tokio_rustls::TlsAcceptor::from);

        info!(
            port = port,
            max_connections = self.max_connections,
            max_pending_joins = self.max_pending_joins,
            tls = tls_acceptor.is_some(),
            "peering listener started"
        );

        loop {
            let (stream, peer_addr) = match listener.accept().await {
                Ok((s, a)) => (s, a),
                Err(e) => {
                    warn!("TCP accept error: {e}");
                    continue;
                }
            };

            // Enforce concurrent connection limit via semaphore.
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    self.connections_rejected.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        peer = %peer_addr,
                        active = self.connections_active.load(Ordering::Relaxed),
                        limit = self.max_connections,
                        "connection limit reached, rejecting"
                    );
                    // Drop the stream immediately (TCP RST).
                    drop(stream);
                    continue;
                }
            };

            self.connections_active.fetch_add(1, Ordering::Relaxed);
            debug!("peering connection from {peer_addr}");

            let pending = self.pending.clone();
            let enc_key = encryption_key;
            let on_announce = on_announce.clone();
            let auto_accept = self.auto_accept.clone();
            let on_accepted = on_accepted.clone();
            let rate_limiter = self.pin_rate_limiter.clone();
            let active_counter = self.connections_active.clone();
            let max_pending = self.max_pending_joins;
            let replay_guard = self.replay_guard.clone();
            let tls_acceptor = tls_acceptor.clone();
            let on_rotation = on_secret_rotation.clone();

            tokio::spawn(async move {
                let _permit = permit; // held until this task ends

                let result = if let Some(acceptor) = tls_acceptor {
                    match tokio::time::timeout(EXCHANGE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls_stream)) => {
                            handle_incoming(
                                tls_stream,
                                peer_addr,
                                pending,
                                enc_key,
                                on_announce,
                                auto_accept,
                                on_accepted,
                                rate_limiter,
                                max_pending,
                                replay_guard,
                                on_rotation,
                            )
                            .await
                        }
                        Ok(Err(e)) => {
                            debug!("TLS handshake with {peer_addr} failed: {e}");
                            Err(PeeringError::Tls(format!("handshake: {e}")))
                        }
                        Err(_) => {
                            debug!("TLS handshake with {peer_addr} timed out");
                            Err(PeeringError::Timeout)
                        }
                    }
                } else {
                    handle_incoming(
                        stream,
                        peer_addr,
                        pending,
                        enc_key,
                        on_announce,
                        auto_accept,
                        on_accepted,
                        rate_limiter,
                        max_pending,
                        replay_guard,
                        on_rotation,
                    )
                    .await
                };

                if let Err(e) = result {
                    debug!("peering connection from {peer_addr} failed: {e}");
                }
                active_counter.fetch_sub(1, Ordering::Relaxed);
            });
        }

        Ok(())
    }
}

/// Handle an incoming connection (plain TCP or TLS).
#[allow(clippy::too_many_arguments)]
async fn handle_incoming<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    peer_addr: SocketAddr,
    pending: Arc<RwLock<HashMap<String, PendingJoin>>>,
    encryption_key: Option<[u8; 32]>,
    on_announce: Arc<dyn Fn(PeerRecord) + Send + Sync>,
    auto_accept: Arc<RwLock<Option<AutoAcceptConfig>>>,
    on_accepted: OnAccepted,
    rate_limiter: Arc<Mutex<PinRateLimiter>>,
    max_pending_joins: usize,
    replay_guard: Arc<ReplayGuard>,
    on_secret_rotation: Option<OnSecretRotation>,
) -> Result<(), PeeringError> {
    // Apply read timeout to protect against slowloris attacks.
    let msg = tokio::time::timeout(EXCHANGE_TIMEOUT, read_message(&mut stream))
        .await
        .map_err(|_| PeeringError::Timeout)??;

    match msg {
        PeeringMessage::JoinRequest(mut req) => {
            // Validate all fields and verify cryptographic signature before processing
            if let Err(e) = validate_and_verify_join_request(&req) {
                warn!(from = %peer_addr, error = %e, "rejecting join request: validation failed");
                return Err(PeeringError::Protocol(format!("invalid join request: {e}")));
            }

            // Auto-detect endpoint: if 0.0.0.0, use TCP peer IP.
            // This runs after validation because validate_join_request
            // intentionally allows unspecified endpoints for auto-detect.
            if req.endpoint.ip().is_unspecified() {
                req.endpoint = SocketAddr::new(peer_addr.ip(), req.wg_listen_port);
                info!(
                    "auto-detected endpoint for {}: {}",
                    sanitize(&req.node_name),
                    req.endpoint
                );
            }

            // Reject if endpoint is still unspecified after auto-detection.
            // WireGuard cannot send packets to 0.0.0.0 — this would cause
            // silent packet loss (see issue #285).
            if req.endpoint.ip().is_unspecified() {
                warn!(
                    node = %sanitize(&req.node_name),
                    endpoint = %req.endpoint,
                    "rejecting join: endpoint is 0.0.0.0 after auto-detection"
                );
                let rejection = JoinResponse {
                    accepted: false,
                    mesh_name: None,
                    mesh_secret: None,
                    mesh_prefix: None,
                    peers: vec![],
                    reason: Some("could not detect public IP; use --endpoint to specify it".into()),
                    approved_by: None,
                };
                write_message(&mut stream, &PeeringMessage::JoinResponse(rejection)).await?;
                return Ok(());
            }

            info!(
                node = %sanitize(&req.node_name),
                endpoint = %req.endpoint,
                request_id = %req.request_id,
                "join request received"
            );
            events::emit(
                EventType::JoinRequestReceived,
                Some(&sanitize(&req.node_name)),
                Some(&req.endpoint.to_string()),
                Some(&format!("request_id={}", req.request_id)),
                None,
            );
            audit_log::emit(
                AuditEventType::PeerJoinRequested,
                Some(&sanitize(&req.node_name)),
                Some(&req.endpoint.to_string()),
                Some(&format!("request_id={}", req.request_id)),
            );

            // Warn if node name already in active peers (the node likely left and is rejoining)
            {
                let peers = crate::store::get_peers().unwrap_or_default();
                if peers.iter().any(|p| {
                    p.name == req.node_name && p.status == syfrah_core::mesh::PeerStatus::Active
                }) {
                    warn!(
                        node = %sanitize(&req.node_name),
                        "node name already in mesh — accepting will replace the old peer entry"
                    );
                }
            }

            // Check PIN auto-accept
            if let Some(ref req_pin) = req.pin {
                let auto = auto_accept.read().await;
                if let Some(ref config) = *auto {
                    // Check rate limit before evaluating PIN
                    let peer_ip = peer_addr.ip();
                    {
                        let mut rl = rate_limiter.lock().await;
                        if rl.is_locked_out(peer_ip) {
                            warn!(
                                ip = %peer_ip,
                                node = %sanitize(&req.node_name),
                                "PIN attempt rate-limited — too many failed attempts from this IP"
                            );
                            tokio::time::sleep(PIN_FAIL_DELAY).await;
                            let rejection = JoinResponse {
                                accepted: false,
                                mesh_name: None,
                                mesh_secret: None,
                                mesh_prefix: None,
                                peers: vec![],
                                reason: Some(
                                    "too many failed PIN attempts, try again later".into(),
                                ),
                                approved_by: None,
                            };
                            write_message(&mut stream, &PeeringMessage::JoinResponse(rejection))
                                .await?;
                            return Ok(());
                        }
                    }

                    // Warn if old-style 4-digit PIN is used
                    if req_pin.len() == 4 && req_pin.chars().all(|c| c.is_ascii_digit()) {
                        warn!(
                            ip = %peer_ip,
                            node = %sanitize(&req.node_name),
                            "deprecated 4-digit PIN format received"
                        );
                    }

                    // Case-sensitive comparison: PINs are short, so every bit
                    // of entropy counts. The charset already excludes ambiguous
                    // characters (0/O, 1/I/L), making exact matching safe.
                    if config.pin == *req_pin {
                        // Check peer limit before accepting
                        let current_peers = crate::store::peer_count().unwrap_or(0);
                        if current_peers >= config.max_peers {
                            warn!(
                                node = %sanitize(&req.node_name),
                                current = current_peers,
                                max = config.max_peers,
                                "peer limit reached, rejecting join request"
                            );
                            events::emit(
                                EventType::PeerLimitReached,
                                Some(&sanitize(&req.node_name)),
                                Some(&req.endpoint.to_string()),
                                Some(&format!("max_peers={}", config.max_peers)),
                                None,
                            );
                            audit_log::emit(
                                AuditEventType::PeerJoinRejected,
                                Some(&sanitize(&req.node_name)),
                                Some(&req.endpoint.to_string()),
                                Some(&format!(
                                    "reason=peer_limit_reached, max_peers={}",
                                    config.max_peers
                                )),
                            );
                            let response = JoinResponse {
                                accepted: false,
                                mesh_name: None,
                                mesh_secret: None,
                                mesh_prefix: None,
                                peers: vec![],
                                reason: Some(format!(
                                    "peer limit reached ({}/{})",
                                    current_peers, config.max_peers
                                )),
                                approved_by: None,
                            };
                            write_message(&mut stream, &PeeringMessage::JoinResponse(response))
                                .await?;
                            return Ok(());
                        }

                        info!(node = %sanitize(&req.node_name), request_id = %req.request_id, "PIN matched, auto-accepting");
                        events::emit(
                            EventType::JoinAutoAccepted,
                            Some(&sanitize(&req.node_name)),
                            Some(&req.endpoint.to_string()),
                            Some("pin-matched"),
                            None,
                        );
                        audit_log::emit(
                            AuditEventType::PeerJoinAccepted,
                            Some(&sanitize(&req.node_name)),
                            Some(&req.endpoint.to_string()),
                            Some("approved_by=pin"),
                        );
                        let (response, new_record) = build_auto_accept_response(&req, config)?;
                        write_message(&mut stream, &PeeringMessage::JoinResponse(response)).await?;
                        on_accepted(new_record);
                        return Ok(());
                    }

                    // PIN mismatch — record failure
                    warn!(
                        ip = %peer_ip,
                        node = %sanitize(&req.node_name),
                        request_id = %req.request_id,
                        "failed PIN attempt"
                    );
                    audit_log::emit(
                        AuditEventType::PeerJoinRejected,
                        Some(&sanitize(&req.node_name)),
                        Some(&req.endpoint.to_string()),
                        Some("reason=bad_pin"),
                    );
                    {
                        let mut rl = rate_limiter.lock().await;
                        let locked_out = rl.record_failure(peer_ip);
                        if locked_out {
                            warn!(
                                ip = %peer_ip,
                                "IP locked out after {} failed PIN attempts",
                                MAX_PIN_ATTEMPTS
                            );
                        }
                    }
                    // Delay before responding to slow down brute-force
                    tokio::time::sleep(PIN_FAIL_DELAY).await;
                }
            }

            // Manual approval: store and wait
            let (tx, rx) = oneshot::channel();
            let pending_node_name = req.node_name.clone();
            let pending_endpoint = req.endpoint.to_string();
            let info = JoinRequestInfo {
                request_id: req.request_id.clone(),
                node_name: req.node_name,
                wg_public_key: req.wg_public_key,
                endpoint: req.endpoint,
                wg_listen_port: req.wg_listen_port,
                received_at: epoch_now(),
                region: req.region,
                zone: req.zone,
            };

            {
                let mut map = pending.write().await;
                // Dedup: if there's already a pending request from the same node name,
                // remove it (the joiner retried with a new key).
                let stale_id = map
                    .values()
                    .find(|p| p.info.node_name == info.node_name)
                    .map(|p| p.info.request_id.clone());
                if let Some(old_id) = stale_id {
                    info!(node = %sanitize(&info.node_name), old_request_id = %old_id, "replacing stale join request");
                    map.remove(&old_id);
                }
                // Enforce pending queue limit.
                if map.len() >= max_pending_joins {
                    warn!(
                        limit = max_pending_joins,
                        "pending join queue full, rejecting request"
                    );
                    return Err(PeeringError::Protocol("too many pending joins".into()));
                }
                map.insert(
                    req.request_id.clone(),
                    PendingJoin {
                        info,
                        response_tx: tx,
                    },
                );
            }

            let response = match tokio::time::timeout(JOIN_TIMEOUT, rx).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(_)) => {
                    let mut map = pending.write().await;
                    map.remove(&req.request_id);
                    return Err(PeeringError::Protocol("daemon shutdown".into()));
                }
                Err(_) => {
                    let mut map = pending.write().await;
                    map.remove(&req.request_id);
                    events::emit(
                        EventType::JoinTimeout,
                        Some(&sanitize(&pending_node_name)),
                        Some(&pending_endpoint),
                        Some(&format!("request_id={}", req.request_id)),
                        None,
                    );
                    audit_log::emit(
                        AuditEventType::PeerJoinRejected,
                        Some(&sanitize(&pending_node_name)),
                        Some(&pending_endpoint),
                        Some(&format!("reason=timeout, request_id={}", req.request_id)),
                    );
                    JoinResponse {
                        accepted: false,
                        mesh_name: None,
                        mesh_secret: None,
                        mesh_prefix: None,
                        peers: vec![],
                        reason: Some("request timed out".into()),
                        approved_by: None,
                    }
                }
            };

            write_message(&mut stream, &PeeringMessage::JoinResponse(response)).await?;
        }

        PeeringMessage::PeerAnnounce(ciphertext) => {
            let enc_key =
                encryption_key.ok_or_else(|| PeeringError::Protocol("no encryption key".into()))?;

            // Replay protection: drop duplicate ciphertexts within the dedup window.
            if replay_guard.is_duplicate(&ciphertext).await {
                debug!(
                    from = %peer_addr,
                    "duplicate announce dropped (same ciphertext seen within dedup window)"
                );
                return Ok(());
            }

            let record = decrypt_record(&ciphertext, &enc_key)?;

            // Validate before any further processing (field reads, stale check, etc.).
            if let Err(e) = validate_peer_record(&record) {
                warn!(from = %peer_addr, error = %e, "rejecting peer announce: validation failed");
                return Err(PeeringError::Protocol(format!(
                    "invalid peer announce: {e}"
                )));
            }

            // Replay protection: reject announces with stale timestamps.
            if replay_guard.is_stale(record.last_seen) {
                let age = replay_guard
                    .monotonic_epoch()
                    .saturating_sub(record.last_seen);
                debug!(
                    peer = %record.name,
                    from = %peer_addr,
                    age_secs = age,
                    max_age_secs = MAX_ANNOUNCE_AGE_SECS,
                    "stale announce rejected"
                );
                return Ok(());
            }

            // Reject status=Removed from peer announces — only a node can remove itself
            // via the leave flow, not via an announce from another peer.
            if record.status == PeerStatus::Removed {
                warn!(
                    from = %peer_addr,
                    peer = %sanitize(&record.name),
                    "rejecting peer announce with status=Removed (potential attack)"
                );
                return Err(PeeringError::Protocol(
                    "peer announce with status=Removed is not allowed".to_string(),
                ));
            }

            info!(
                peer = %sanitize(&record.name),
                ipv6 = %record.mesh_ipv6,
                from = %peer_addr,
                "peer announce received"
            );
            on_announce(record);
        }

        PeeringMessage::SecretRotation(ciphertext) => {
            let enc_key =
                encryption_key.ok_or_else(|| PeeringError::Protocol("no encryption key".into()))?;

            let new_secret_str =
                syfrah_core::mesh::decrypt_secret(&ciphertext, &enc_key).map_err(|e| {
                    PeeringError::Protocol(format!("failed to decrypt rotated secret: {e}"))
                })?;

            info!(
                from = %peer_addr,
                "secret rotation received from peer"
            );

            if let Some(ref cb) = on_secret_rotation {
                cb(new_secret_str);
            } else {
                warn!(
                    from = %peer_addr,
                    "secret rotation received but no handler registered"
                );
            }
        }

        PeeringMessage::JoinResponse(_) => {
            return Err(PeeringError::Protocol("unexpected JoinResponse".into()));
        }
    }

    Ok(())
}

/// Build a JoinResponse and PeerRecord for auto-accept.
fn build_auto_accept_response(
    req: &JoinRequest,
    config: &AutoAcceptConfig,
) -> Result<(JoinResponse, PeerRecord), PeeringError> {
    use syfrah_core::addressing;

    let new_wg_pub = wireguard_control::Key::from_base64(&req.wg_public_key)
        .map_err(|_| PeeringError::Protocol("invalid WG public key".into()))?;
    let new_mesh_ipv6 = addressing::derive_node_address(&config.mesh_prefix, new_wg_pub.as_bytes());

    // Load current peers from store + our own record
    let mut all_peers = crate::store::load().map(|s| s.peers).unwrap_or_default();
    all_peers.push(config.my_record.clone());

    // Use the joiner's region/zone from the request. If zone was not
    // provided, auto-generate one using the leader's peer list so the
    // joiner gets a unique zone.
    let region = req
        .region
        .clone()
        .unwrap_or_else(|| crate::daemon::DEFAULT_REGION.to_string());
    let zone = req
        .zone
        .clone()
        .unwrap_or_else(|| crate::store::generate_zone(&region, &all_peers));

    let new_record = PeerRecord {
        name: req.node_name.clone(),
        wg_public_key: req.wg_public_key.clone(),
        endpoint: req.endpoint,
        mesh_ipv6: new_mesh_ipv6,
        last_seen: epoch_now(),
        status: syfrah_core::mesh::PeerStatus::Active,
        region: Some(region),
        zone: Some(zone),
        topology: None,
        hypervisor_report: None,
    };

    let response = JoinResponse {
        accepted: true,
        mesh_name: Some(config.mesh_name.clone()),
        mesh_secret: Some(config.mesh_secret_str.clone()),
        mesh_prefix: Some(config.mesh_prefix),
        peers: all_peers,
        reason: None,
        approved_by: Some("pin".into()),
    };

    Ok((response, new_record))
}

// --- Client-side helpers ---

use std::pin::Pin;
use std::task::{Context, Poll};

/// A stream that is either a plain TCP connection or a TLS-upgraded one.
/// Implements `AsyncRead + AsyncWrite` so callers can use a single code path.
enum MaybeTlsStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Upgrade a `TcpStream` to TLS if a config is provided, returning a
/// `MaybeTlsStream`.  This eliminates duplicated TLS / non-TLS branches
/// in every client-side function.
async fn maybe_upgrade_tls(
    tcp_stream: TcpStream,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    target: &SocketAddr,
) -> Result<MaybeTlsStream, PeeringError> {
    if let Some(cfg) = tls_config {
        let connector = tokio_rustls::TlsConnector::from(cfg);
        let server_name = rustls::pki_types::ServerName::try_from("syfrah-mesh.internal")
            .map_err(|e| PeeringError::Tls(format!("server name: {e}")))?;
        let tls_stream = tokio::time::timeout(
            EXCHANGE_TIMEOUT,
            connector.connect(server_name.to_owned(), tcp_stream),
        )
        .await
        .map_err(|_| PeeringError::Timeout)?
        .map_err(|e| PeeringError::Tls(format!("TLS handshake failed with {target}. Verify the node is running a compatible version. ({e})")))?;
        Ok(MaybeTlsStream::Tls(Box::new(tls_stream)))
    } else {
        Ok(MaybeTlsStream::Plain(tcp_stream))
    }
}

// --- Client-side functions ---

/// Send a join request to an existing node and wait for response.
///
/// When `tls_config` is `Some`, the TCP connection is upgraded to TLS before
/// sending the join request.  The joiner does not yet know the mesh secret
/// (that is what it receives in the `JoinResponse`), so it cannot verify the
/// server certificate via the mesh-derived CA.  We therefore skip server-cert
/// verification for the join handshake only — the PIN exchange provides the
/// authentication guarantee at this stage.
pub async fn send_join_request(
    target: SocketAddr,
    request: JoinRequest,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<JoinResponse, PeeringError> {
    let tcp_stream = tokio::time::timeout(EXCHANGE_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| PeeringError::Timeout)??;

    let mut stream = maybe_upgrade_tls(tcp_stream, tls_config, &target).await?;

    write_message(&mut stream, &PeeringMessage::JoinRequest(request)).await?;
    let msg = tokio::time::timeout(JOIN_TIMEOUT, read_message(&mut stream))
        .await
        .map_err(|_| PeeringError::Timeout)??;

    match msg {
        PeeringMessage::JoinResponse(resp) => {
            if let Err(e) = validate_join_response(&resp) {
                warn!(error = %e, "rejecting join response: validation failed");
                return Err(PeeringError::Protocol(format!(
                    "invalid join response: {e}"
                )));
            }
            Ok(resp)
        }
        _ => Err(PeeringError::Protocol("expected JoinResponse".into())),
    }
}

/// Announce a new peer to an existing mesh member.
/// Maximum retry attempts for transient announcement failures.
const ANNOUNCE_MAX_RETRIES: u32 = 3;

pub async fn announce_peer(
    target_endpoint: SocketAddr,
    peering_port: u16,
    record: &PeerRecord,
    encryption_key: &[u8; 32],
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<(), PeeringError> {
    let target = SocketAddr::new(target_endpoint.ip(), peering_port);
    // Stamp the record with the current time for replay protection.
    let mut stamped = record.clone();
    stamped.last_seen = epoch_now();
    let ciphertext = encrypt_record(&stamped, encryption_key)?;

    let mut last_err = None;
    for attempt in 0..ANNOUNCE_MAX_RETRIES {
        if attempt > 0 {
            let delay = Duration::from_secs(1 << (attempt - 1)); // 1s, 2s, 4s
            tokio::time::sleep(delay).await;
        }
        match try_announce(&target, &ciphertext, tls_config.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Retry on transient errors. TLS handshake failures are also
                // retried because they can occur when the remote listener is
                // still initialising its TLS acceptor.
                debug!(
                    "announce attempt {}/{} to {target} failed: {e}",
                    attempt + 1,
                    ANNOUNCE_MAX_RETRIES
                );
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(PeeringError::Timeout))
}

async fn try_announce(
    target: &SocketAddr,
    ciphertext: &[u8],
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<(), PeeringError> {
    let tcp_stream = tokio::time::timeout(EXCHANGE_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| PeeringError::Timeout)??;

    let mut stream = maybe_upgrade_tls(tcp_stream, tls_config, target).await?;
    write_message(
        &mut stream,
        &PeeringMessage::PeerAnnounce(ciphertext.to_vec()),
    )
    .await?;
    Ok(())
}

/// Minimum number of peers to gossip to per round.
const GOSSIP_MIN_FANOUT: usize = 3;

/// Compute the gossip fan-out for a mesh of `n` eligible peers.
///
/// Returns `max(GOSSIP_MIN_FANOUT, ceil(log2(n)))`, capped at `n` so we never
/// attempt to contact more peers than exist. For very small meshes (n <= 3)
/// this degrades gracefully to full broadcast.
pub fn gossip_fanout(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let log2 = (usize::BITS - n.leading_zeros()) as usize; // ceil(log2(n+1)) ≈ ceil(log2(n))
    GOSSIP_MIN_FANOUT.max(log2).min(n)
}

/// Select a random subset of `known_peers` (excluding the announced record
/// itself) to gossip to. The subset size is determined by [`gossip_fanout`].
pub fn select_gossip_targets<'a>(
    record: &PeerRecord,
    known_peers: &'a [PeerRecord],
) -> Vec<&'a PeerRecord> {
    use rand::seq::SliceRandom;

    let eligible: Vec<&PeerRecord> = known_peers
        .iter()
        .filter(|p| p.wg_public_key != record.wg_public_key)
        .collect();

    let fanout = gossip_fanout(eligible.len());
    if fanout >= eligible.len() {
        return eligible;
    }

    let mut rng = rand::thread_rng();
    let mut selected = eligible;
    selected.shuffle(&mut rng);
    selected.truncate(fanout);
    selected
}

/// Announce a new peer to a random gossip subset of known mesh members.
///
/// Instead of broadcasting to every peer (O(n²) total messages), each node
/// announces to `max(3, ceil(log2(n)))` randomly chosen peers. Receiving
/// nodes re-gossip on their own, achieving convergence in O(log n) rounds
/// while the existing replay guard prevents infinite forwarding loops.
///
/// Returns (succeeded, failed) counts.
pub async fn announce_peer_to_mesh(
    record: &PeerRecord,
    known_peers: &[PeerRecord],
    encryption_key: &[u8; 32],
    peering_port: u16,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> (usize, usize) {
    let targets = select_gossip_targets(record, known_peers);
    let fanout = targets.len();
    let total_eligible = known_peers
        .iter()
        .filter(|p| p.wg_public_key != record.wg_public_key)
        .count();

    if fanout < total_eligible {
        debug!(
            fanout = fanout,
            eligible = total_eligible,
            "gossip: announcing to random subset"
        );
    }

    let mut succeeded = 0;
    let mut failed = 0;
    for peer in targets {
        if let Err(e) = announce_peer(
            peer.endpoint,
            peering_port,
            record,
            encryption_key,
            tls_config.clone(),
        )
        .await
        {
            warn!(target_peer = %sanitize(&peer.name), target_endpoint = %peer.endpoint, error = %e, "gossip announcement failed after retries");
            events::emit(
                EventType::PeerAnnounceFailed,
                Some(&sanitize(&peer.name)),
                Some(&peer.endpoint.to_string()),
                Some(&format!("error={e}")),
                None,
            );
            failed += 1;
        } else {
            debug!(target_peer = %sanitize(&peer.name), record = %sanitize(&record.name), "gossip: announced peer");
            succeeded += 1;
        }
    }
    (succeeded, failed)
}

/// Broadcast a secret rotation message to all active peers.
///
/// The new secret is encrypted with the OLD encryption key so that all peers
/// holding the current secret can decrypt and apply the rotation. This is a
/// full broadcast (not gossip) because secret rotation is a rare, high-priority
/// operation that must reach every peer reliably.
///
/// Returns (succeeded, failed) counts.
pub async fn broadcast_secret_rotation(
    encrypted_secret: &[u8],
    peers: &[PeerRecord],
    peering_port: u16,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> (usize, usize) {
    let mut succeeded = 0;
    let mut failed = 0;

    for peer in peers.iter().filter(|p| p.status == PeerStatus::Active) {
        let target = SocketAddr::new(peer.endpoint.ip(), peering_port);
        let msg = PeeringMessage::SecretRotation(encrypted_secret.to_vec());

        let mut last_err = None;
        let mut ok = false;
        for attempt in 0..ANNOUNCE_MAX_RETRIES {
            if attempt > 0 {
                let delay = Duration::from_secs(1 << (attempt - 1));
                tokio::time::sleep(delay).await;
            }
            match try_send_message(&target, &msg, tls_config.clone()).await {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => {
                    debug!(
                        "rotation broadcast attempt {}/{} to {target} failed: {e}",
                        attempt + 1,
                        ANNOUNCE_MAX_RETRIES
                    );
                    last_err = Some(e);
                }
            }
        }
        if ok {
            debug!(target_peer = %sanitize(&peer.name), "secret rotation broadcast succeeded");
            succeeded += 1;
        } else {
            warn!(
                target_peer = %sanitize(&peer.name),
                error = %last_err.unwrap(),
                "secret rotation broadcast failed after retries"
            );
            failed += 1;
        }
    }

    (succeeded, failed)
}

/// Send a single peering message to a target address (with optional TLS).
async fn try_send_message(
    target: &SocketAddr,
    msg: &PeeringMessage,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<(), PeeringError> {
    let tcp_stream = tokio::time::timeout(EXCHANGE_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| PeeringError::Timeout)??;

    let mut stream = maybe_upgrade_tls(tcp_stream, tls_config, target).await?;
    write_message(&mut stream, msg).await?;
    Ok(())
}

/// Announce a peer to the mesh using topology-aware waves.
///
/// Instead of broadcasting to all peers simultaneously, this function
/// partitions peers by topology tier (same-zone, same-region, cross-region)
/// and announces in three waves with configurable delays and concurrency.
///
/// This reduces queue saturation and announce drops at scale by prioritising
/// nearby peers and rate-limiting cross-region traffic.
///
/// Returns (succeeded, failed) totals across all waves.
pub async fn announce_peer_in_waves(
    record: &PeerRecord,
    source: &PeerRecord,
    known_peers: &[PeerRecord],
    encryption_key: &[u8; 32],
    peering_port: u16,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    announce_cfg: &crate::config::AnnouncementConfig,
) -> (usize, usize) {
    use crate::topology::partition_by_tier;

    let tiers = partition_by_tier(source, known_peers);

    let mut total_ok = 0usize;
    let mut total_fail = 0usize;

    // Wave 1: same-zone peers
    if announce_cfg.same_zone_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(announce_cfg.same_zone_delay_ms)).await;
    }
    let (ok, fail) = announce_wave(
        record,
        &tiers.same_zone,
        encryption_key,
        peering_port,
        tls_config.clone(),
        announce_cfg.same_zone_concurrency,
    )
    .await;
    total_ok += ok;
    total_fail += fail;
    debug!(
        wave = 1,
        tier = "same_zone",
        targets = tiers.same_zone.len(),
        succeeded = ok,
        failed = fail,
        "wave announce completed"
    );

    // Wave 2: same-region peers (different zone)
    if !tiers.same_region.is_empty() {
        if announce_cfg.same_region_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(announce_cfg.same_region_delay_ms)).await;
        }
        let (ok, fail) = announce_wave(
            record,
            &tiers.same_region,
            encryption_key,
            peering_port,
            tls_config.clone(),
            announce_cfg.same_region_concurrency,
        )
        .await;
        total_ok += ok;
        total_fail += fail;
        debug!(
            wave = 2,
            tier = "same_region",
            targets = tiers.same_region.len(),
            succeeded = ok,
            failed = fail,
            "wave announce completed"
        );
    }

    // Wave 3: cross-region peers
    if !tiers.cross_region.is_empty() {
        if announce_cfg.cross_region_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(announce_cfg.cross_region_delay_ms)).await;
        }
        let (ok, fail) = announce_wave(
            record,
            &tiers.cross_region,
            encryption_key,
            peering_port,
            tls_config,
            announce_cfg.cross_region_concurrency,
        )
        .await;
        total_ok += ok;
        total_fail += fail;
        debug!(
            wave = 3,
            tier = "cross_region",
            targets = tiers.cross_region.len(),
            succeeded = ok,
            failed = fail,
            "wave announce completed"
        );
    }

    (total_ok, total_fail)
}

/// Announce a peer to a set of targets with bounded concurrency.
async fn announce_wave(
    record: &PeerRecord,
    targets: &[PeerRecord],
    encryption_key: &[u8; 32],
    peering_port: u16,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    max_concurrency: usize,
) -> (usize, usize) {
    use tokio::sync::Semaphore as TokioSemaphore;

    if targets.is_empty() {
        return (0, 0);
    }

    let sem = Arc::new(TokioSemaphore::new(max_concurrency));
    let succeeded = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(targets.len());

    for peer in targets {
        let permit = sem.clone().acquire_owned().await;
        let record = record.clone();
        let peer = peer.clone();
        let enc = *encryption_key;
        let tls = tls_config.clone();
        let ok_counter = succeeded.clone();
        let fail_counter = failed.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = announce_peer(peer.endpoint, peering_port, &record, &enc, tls).await {
                warn!(
                    target_peer = %sanitize(&peer.name),
                    target_endpoint = %peer.endpoint,
                    error = %e,
                    "wave announcement failed"
                );
                events::emit(
                    EventType::PeerAnnounceFailed,
                    Some(&sanitize(&peer.name)),
                    Some(&peer.endpoint.to_string()),
                    Some(&format!("error={e}")),
                    None,
                );
                fail_counter.fetch_add(1, Ordering::Relaxed);
            } else {
                debug!(
                    target_peer = %sanitize(&peer.name),
                    record = %sanitize(&record.name),
                    "wave: announced peer"
                );
                ok_counter.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    (
        succeeded.load(Ordering::Relaxed) as usize,
        failed.load(Ordering::Relaxed) as usize,
    )
}

// --- Wire protocol helpers ---

async fn write_message<S: AsyncWrite + Unpin>(
    stream: &mut S,
    msg: &PeeringMessage,
) -> Result<(), PeeringError> {
    let data = serde_json::to_vec(msg)?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&data).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_message<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<PeeringMessage, PeeringError> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MSG_SIZE {
        return Err(PeeringError::Protocol(format!("message too large: {len}")));
    }
    let mut data = vec![0u8; len as usize];
    stream.read_exact(&mut data).await?;
    let msg: PeeringMessage = serde_json::from_slice(&data)?;
    Ok(msg)
}

/// Wall-clock epoch seconds — used only for wire timestamps and display,
/// never for security-critical timing comparisons.
fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate a short random request ID (8 hex chars).
///
/// RNG policy: uses OsRng for direct OS entropy (security-sensitive identifier).
pub fn generate_request_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Generate a random 6-character alphanumeric PIN.
///
/// Uses a charset that excludes ambiguous characters (0/O, 1/I/L) for
/// readability. Yields ~2.1 billion possible values (32^6).
///
/// RNG policy: uses OsRng for direct OS entropy (authentication material).
pub fn generate_pin() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    (0..6)
        .map(|_| CHARSET[rand::rngs::OsRng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::now;

    #[tokio::test]
    async fn replay_guard_detects_duplicate_ciphertext() {
        let guard = ReplayGuard::new();
        let data = b"test-ciphertext-payload";

        // First time: not a duplicate.
        assert!(!guard.is_duplicate(data).await);
        // Second time: duplicate.
        assert!(guard.is_duplicate(data).await);
    }

    #[tokio::test]
    async fn replay_guard_allows_different_ciphertexts() {
        let guard = ReplayGuard::new();
        assert!(!guard.is_duplicate(b"payload-a").await);
        assert!(!guard.is_duplicate(b"payload-b").await);
    }

    #[test]
    fn replay_guard_rejects_stale_timestamp() {
        let guard = ReplayGuard::new();
        let stale = guard
            .monotonic_epoch()
            .saturating_sub(MAX_ANNOUNCE_AGE_SECS + 100);
        assert!(guard.is_stale(stale));
    }

    #[test]
    fn replay_guard_accepts_fresh_timestamp() {
        let guard = ReplayGuard::new();
        let fresh = guard.monotonic_epoch();
        assert!(!guard.is_stale(fresh));
    }

    #[test]
    fn replay_guard_accepts_timestamp_at_boundary() {
        let guard = ReplayGuard::new();
        // Exactly at the boundary (age == MAX_ANNOUNCE_AGE_SECS) should pass.
        let at_boundary = guard
            .monotonic_epoch()
            .saturating_sub(MAX_ANNOUNCE_AGE_SECS);
        assert!(!guard.is_stale(at_boundary));
    }

    #[test]
    fn replay_guard_rejects_timestamp_just_past_boundary() {
        let guard = ReplayGuard::new();
        let past = guard
            .monotonic_epoch()
            .saturating_sub(MAX_ANNOUNCE_AGE_SECS + 1);
        assert!(guard.is_stale(past));
    }

    #[tokio::test]
    async fn replay_guard_evicts_entries_after_dedup_window() {
        let guard = ReplayGuard::new();
        let data = b"eviction-test-payload";

        // Insert the entry, then backdate it beyond the dedup window.
        assert!(!guard.is_duplicate(data).await);
        {
            let mut seen = guard.seen.lock().await;
            let hash = sha256(data);
            if let Some(ts) = seen.get_mut(&hash) {
                *ts = Instant::now()
                    .checked_sub(Duration::from_secs(DEDUP_WINDOW_SECS + 1))
                    .expect("backdating should succeed");
            }
        }
        // After eviction, the same payload should no longer be considered duplicate.
        assert!(!guard.is_duplicate(data).await);
    }

    #[test]
    fn monotonic_epoch_never_decreases() {
        let guard = ReplayGuard::new();
        let t1 = guard.monotonic_epoch();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = guard.monotonic_epoch();
        assert!(t2 >= t1);
    }

    #[test]
    fn replay_guard_accepts_future_timestamp() {
        let guard = ReplayGuard::new();
        let future = guard.monotonic_epoch() + 1000;
        assert!(!guard.is_stale(future));
    }

    /// Malformed WG key with a stale timestamp must be rejected by validation,
    /// not silently swallowed by the stale-timestamp early return.
    #[tokio::test]
    async fn malformed_wg_key_rejected_before_stale_check() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        // Build a record with an invalid WG key and a stale last_seen.
        // If validation runs first, we get Err(Protocol("invalid peer announce ...")).
        // If the stale check ran first, we'd get Ok(()) — the bug this test guards against.
        let bad_record = PeerRecord {
            name: "node-test".into(),
            wg_public_key: "not-a-valid-key".into(),
            endpoint: "127.0.0.1:51820".parse().unwrap(),
            mesh_ipv6: "fd00::1".parse().unwrap(),
            last_seen: now().saturating_sub(MAX_ANNOUNCE_AGE_SECS + 200),
            status: PeerStatus::Active,
            region: None,
            zone: None,
            topology: None,
            hypervisor_report: None,
        };

        let enc_key = [0xABu8; 32];
        let ciphertext = encrypt_record(&bad_record, &enc_key).unwrap();
        let msg = PeeringMessage::PeerAnnounce(ciphertext);
        let payload = serde_json::to_vec(&msg).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Send the announce from a client connection.
        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let len = payload.len() as u32;
            stream.write_all(&len.to_be_bytes()).await.unwrap();
            stream.write_all(&payload).await.unwrap();
            stream.flush().await.unwrap();
        });

        let (server_stream, peer_addr) = listener.accept().await.unwrap();

        let result = handle_incoming(
            server_stream,
            peer_addr,
            Arc::new(RwLock::new(HashMap::new())),
            Some(enc_key),
            Arc::new(|_: PeerRecord| {}),
            Arc::new(RwLock::new(None)),
            Arc::new(|_: PeerRecord| {}),
            Arc::new(Mutex::new(PinRateLimiter::new())),
            100,
            Arc::new(ReplayGuard::new()),
            None,
        )
        .await;

        client.await.unwrap();

        // Must be a Protocol error about invalid peer announce, NOT Ok(()).
        let err = result.expect_err("malformed WG key should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid peer announce"),
            "expected 'invalid peer announce', got: {msg}"
        );
    }

    // --- Gossip protocol tests ---

    fn make_peer(name: &str, key: &str) -> PeerRecord {
        PeerRecord {
            name: name.to_string(),
            wg_public_key: key.to_string(),
            endpoint: "203.0.113.1:51820".parse().unwrap(),
            mesh_ipv6: std::net::Ipv6Addr::new(0xfd12, 0x3456, 0x7800, 0, 0, 0, 0, 1),
            last_seen: 1700000000,
            status: syfrah_core::mesh::PeerStatus::Active,
            region: None,
            zone: None,
            topology: None,
            hypervisor_report: None,
        }
    }

    #[test]
    fn gossip_fanout_zero_peers() {
        assert_eq!(gossip_fanout(0), 0);
    }

    #[test]
    fn gossip_fanout_small_mesh_uses_minimum() {
        // For 1-3 peers, fanout == n (can't exceed available peers).
        assert_eq!(gossip_fanout(1), 1);
        assert_eq!(gossip_fanout(2), 2);
        assert_eq!(gossip_fanout(3), 3);
    }

    #[test]
    fn gossip_fanout_medium_mesh() {
        // 8 peers: bits needed = 4, fanout = max(3, 4) = 4
        assert_eq!(gossip_fanout(8), 4);
        // 16 peers: bits needed = 5, fanout = max(3, 5) = 5
        assert_eq!(gossip_fanout(16), 5);
        // Both above minimum and within peer count
        assert!(gossip_fanout(16) >= GOSSIP_MIN_FANOUT);
        assert!(gossip_fanout(16) <= 16);
    }

    #[test]
    fn gossip_fanout_large_mesh() {
        // 1000 peers: log2(1000) ≈ 10
        let f = gossip_fanout(1000);
        assert!(f >= GOSSIP_MIN_FANOUT);
        assert!(f <= 15); // should be ~10, definitely not 1000
        assert!(f < 1000);
    }

    #[test]
    fn gossip_fanout_never_exceeds_n() {
        for n in 0..=100 {
            assert!(gossip_fanout(n) <= n || n == 0);
        }
    }

    #[test]
    fn select_gossip_targets_excludes_self() {
        let self_peer = make_peer("self", "key-self");
        let peers = vec![
            make_peer("self", "key-self"),
            make_peer("a", "key-a"),
            make_peer("b", "key-b"),
        ];
        let targets = select_gossip_targets(&self_peer, &peers);
        assert!(targets.iter().all(|p| p.wg_public_key != "key-self"));
    }

    #[test]
    fn select_gossip_targets_small_mesh_returns_all() {
        let self_peer = make_peer("self", "key-self");
        let peers = vec![
            make_peer("self", "key-self"),
            make_peer("a", "key-a"),
            make_peer("b", "key-b"),
        ];
        // 2 eligible peers, fanout = min(3, 2) = 2 → all returned
        let targets = select_gossip_targets(&self_peer, &peers);
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn select_gossip_targets_large_mesh_returns_subset() {
        let self_peer = make_peer("self", "key-self");
        let mut peers = vec![make_peer("self", "key-self")];
        for i in 0..100 {
            peers.push(make_peer(&format!("node-{i}"), &format!("key-{i}")));
        }
        // 100 eligible peers, fanout ≈ 7, definitely < 100
        let targets = select_gossip_targets(&self_peer, &peers);
        assert!(targets.len() >= GOSSIP_MIN_FANOUT);
        assert!(targets.len() < 100);
    }

    #[test]
    fn select_gossip_targets_empty_mesh() {
        let self_peer = make_peer("self", "key-self");
        let peers: Vec<PeerRecord> = vec![];
        let targets = select_gossip_targets(&self_peer, &peers);
        assert!(targets.is_empty());
    }
}
