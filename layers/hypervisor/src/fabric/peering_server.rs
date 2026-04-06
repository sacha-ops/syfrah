//! Peering TCP server — accepts join requests from new nodes.
//!
//! Opens the DB per-request (no long-lived lock).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use syfrah_core::error::SyfrahError;
use syfrah_state::LayerDb;

/// Max PIN failures per IP before blocking.
const MAX_PIN_FAILURES: u32 = 5;
/// Duration to block an IP after too many failures.
const PIN_BLOCK_DURATION: Duration = Duration::from_secs(600); // 10 minutes

/// Simple per-IP rate limiter for PIN brute-force protection.
struct PinRateLimiter {
    failures: HashMap<IpAddr, (u32, Instant)>,
}

impl PinRateLimiter {
    fn new() -> Self {
        Self {
            failures: HashMap::new(),
        }
    }

    fn is_blocked(&self, ip: &IpAddr) -> bool {
        if let Some((count, since)) = self.failures.get(ip) {
            if *count >= MAX_PIN_FAILURES && since.elapsed() < PIN_BLOCK_DURATION {
                return true;
            }
        }
        false
    }

    fn record_failure(&mut self, ip: IpAddr) {
        let entry = self.failures.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= PIN_BLOCK_DURATION {
            // Reset after block period
            *entry = (1, Instant::now());
        } else {
            entry.0 += 1;
        }
    }

    fn record_success(&mut self, ip: &IpAddr) {
        self.failures.remove(ip);
    }
}

use super::peer::Peer;
use super::peering::{JoinRequest, JoinResponse, PeerInfo};
use super::service;
use super::state::FabricState;

/// Start a peering listener. Opens the DB per-request to avoid holding the lock.
pub async fn listen(
    db_opener: impl Fn() -> Result<LayerDb, SyfrahError>,
    pin: &str,
    bind_addr: SocketAddr,
    timeout: Duration,
    max_joins: usize,
) -> Result<usize, SyfrahError> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|e| SyfrahError::internal(format!("failed to bind peering port: {e}")))?;

    // TLS setup
    let tls_config = super::tls::server_config()?;
    let tls_acceptor = TlsAcceptor::from(tls_config);

    tracing::info!(addr = %bind_addr, "peering listener started (TLS)");

    let mut accepted = 0;
    let rate_limiter = Arc::new(Mutex::new(PinRateLimiter::new()));

    loop {
        let accept = tokio::time::timeout(timeout, listener.accept()).await;

        match accept {
            Ok(Ok((tcp_stream, peer_addr))) => {
                // Check rate limit before processing
                {
                    let rl = rate_limiter.lock().unwrap();
                    if rl.is_blocked(&peer_addr.ip()) {
                        tracing::warn!(peer = %peer_addr, "blocked (too many failed PIN attempts)");
                        continue;
                    }
                }

                // TLS handshake
                let mut stream = match tls_acceptor.accept(tcp_stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(peer = %peer_addr, error = %e, "TLS handshake failed");
                        continue;
                    }
                };

                tracing::info!(peer = %peer_addr, "incoming join request (TLS)");

                let db = match db_opener() {
                    Ok(db) => db,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to open DB for join");
                        continue;
                    }
                };

                match handle_join(&mut stream, &db, pin, peer_addr).await {
                    Ok(peer_name) => {
                        rate_limiter.lock().unwrap().record_success(&peer_addr.ip());
                        tracing::info!(peer = %peer_addr, name = %peer_name, "join accepted");
                        accepted += 1;
                        drop(db);
                        if max_joins > 0 && accepted >= max_joins {
                            break;
                        }
                    }
                    Err(e) => {
                        rate_limiter.lock().unwrap().record_failure(peer_addr.ip());
                        tracing::warn!(peer = %peer_addr, error = %e, "join rejected");
                        drop(db);
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "accept error");
            }
            Err(_) => {
                tracing::info!("peering listener timeout");
                break;
            }
        }
    }

    Ok(accepted)
}

/// Handle a single join request on a stream.
async fn handle_join(
    stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
    db: &LayerDb,
    expected_pin: &str,
    peer_addr: SocketAddr,
) -> Result<String, SyfrahError> {
    let mut req = read_json::<JoinRequest>(stream).await?;

    // Validate peer-provided fields against injection (newlines, control chars)
    validate_peer_field(&req.name, "name")?;
    validate_peer_field(&req.region, "region")?;
    validate_peer_field(&req.zone, "zone")?;
    validate_peer_field(&req.wg_public_key, "wg_public_key")?;
    if let Some(ref ep) = req.endpoint {
        validate_peer_field(ep, "endpoint")?;
    }

    // If the joiner didn't specify an endpoint, use their TCP source IP + WG port
    if req.endpoint.is_none() {
        req.endpoint = Some(format!("{}:{}", peer_addr.ip(), req.wg_port));
    }

    if !super::peering::validate_pin(expected_pin, req.pin.as_deref()) {
        let resp = JoinResponse::rejected("invalid PIN");
        write_json(stream, &resp).await?;
        return Err(SyfrahError::permission_denied("invalid PIN"));
    }

    let mut state = FabricState::load(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?
        .ok_or_else(|| SyfrahError::precondition("not initialized"))?;

    let self_info = PeerInfo {
        name: state.hypervisor.name.clone(),
        region: state.hypervisor.region.clone(),
        zone: state.hypervisor.zone.clone(),
        wg_public_key: state.hypervisor.wg_public_key.clone(),
        wg_port: state.hypervisor.wg_port,
        endpoint: state.hypervisor.endpoint.clone(),
        mesh_ipv6: state.hypervisor.mesh_ipv6,
    };

    let existing_peers: Vec<PeerInfo> = state
        .peers
        .peers
        .iter()
        .map(|p| PeerInfo {
            name: p.name.clone(),
            region: p.region.clone(),
            zone: p.zone.clone(),
            wg_public_key: p.wg_public_key.clone(),
            wg_port: p.wg_port,
            endpoint: p.endpoint.clone(),
            mesh_ipv6: p.mesh_ipv6,
        })
        .collect();

    let resp = JoinResponse::accepted(&state.secret, state.mesh.prefix, state.mesh.id.as_str(), existing_peers, self_info);
    write_json(stream, &resp).await?;

    // Derive the new peer's mesh IPv6
    let pub_bytes = base64::engine::general_purpose::STANDARD
        .decode(&req.wg_public_key)
        .unwrap_or_default();
    let peer_ipv6 = syfrah_core::addressing::derive_node_address(&state.mesh.prefix, &pub_bytes);

    // Build announce info before consuming req fields
    let new_peer_info = PeerInfo {
        name: req.name.clone(),
        region: req.region.clone(),
        zone: req.zone.clone(),
        wg_public_key: req.wg_public_key.clone(),
        wg_port: req.wg_port,
        endpoint: req.endpoint.clone(),
        mesh_ipv6: peer_ipv6,
    };

    // Skip if we already have this peer (duplicate join)
    if state.peers.find_by_key(&req.wg_public_key).is_some()
        || state.peers.find_by_name(&req.name).is_some()
    {
        tracing::info!(peer = %req.name, "duplicate join, already known");
        return Ok(req.name);
    }

    // Add peer + save + update WG
    let new_peer = Peer::new(
        req.name.clone(),
        req.region,
        req.zone,
        req.wg_public_key,
        req.wg_port,
        req.endpoint,
        peer_ipv6,
    );
    let _ = state.peers.add(new_peer);

    // Bug 3 fix: save immediately, don't defer
    state
        .save(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?;

    // Update WireGuard config with new peer
    let peers_for_wg: Vec<_> = state
        .peers
        .peers
        .iter()
        .map(|p| {
            (
                p.wg_public_key.clone(),
                "25".to_string(),
                p.mesh_ipv6,
                p.endpoint.clone(),
            )
        })
        .collect();

    service::update_config(
        &state.hypervisor.wg_private_key,
        state.hypervisor.wg_port,
        &state.hypervisor.mesh_ipv6,
        &peers_for_wg,
    )?;
    let announcer_name = state.hypervisor.name.clone();
    let wg_port = state.hypervisor.wg_port;

    // Collect peers to announce to (all except the new one)
    let new_key = new_peer_info.wg_public_key.clone();
    let announce_targets: Vec<_> = state
        .peers
        .peers
        .iter()
        .filter(|p| p.wg_public_key != new_key)
        .cloned()
        .collect();

    if !announce_targets.is_empty() {
        tokio::spawn(async move {
            let (ok, fail) = super::announce::broadcast_new_peer(
                &new_peer_info,
                &announcer_name,
                &announce_targets,
                wg_port,
            )
            .await;
            if ok > 0 || fail > 0 {
                tracing::info!(
                    successes = ok,
                    failures = fail,
                    "peer announcement broadcast complete"
                );
            }
        });
    }

    Ok(req.name)
}

/// Read a length-prefixed JSON message from a stream.
pub async fn read_json<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncReadExt + Unpin),
) -> Result<T, SyfrahError> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| SyfrahError::network(format!("read length failed: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > 1_048_576 {
        return Err(SyfrahError::validation("message too large"));
    }

    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| SyfrahError::network(format!("read body failed: {e}")))?;

    serde_json::from_slice(&buf).map_err(|e| SyfrahError::validation(format!("invalid JSON: {e}")))
}

/// Write a length-prefixed JSON message to a stream.
pub async fn write_json<T: serde::Serialize>(
    stream: &mut (impl AsyncWriteExt + Unpin),
    msg: &T,
) -> Result<(), SyfrahError> {
    let data = serde_json::to_vec(msg)
        .map_err(|e| SyfrahError::internal(format!("serialize failed: {e}")))?;
    let len = (data.len() as u32).to_be_bytes();

    stream
        .write_all(&len)
        .await
        .map_err(|e| SyfrahError::network(format!("write failed: {e}")))?;
    stream
        .write_all(&data)
        .await
        .map_err(|e| SyfrahError::network(format!("write failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| SyfrahError::network(format!("flush failed: {e}")))?;

    Ok(())
}

/// Validate a peer-provided field against injection attacks.
/// Rejects newlines, control characters, and excessive length.
fn validate_peer_field(value: &str, field_name: &str) -> Result<(), SyfrahError> {
    if value.len() > 256 {
        return Err(SyfrahError::validation(format!(
            "{field_name} too long (max 256 chars)"
        )));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(SyfrahError::validation(format!(
            "{field_name} contains control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn json_roundtrip_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let req: JoinRequest = read_json(&mut stream).await.unwrap();
            assert_eq!(req.name, "test-node");

            let resp = JoinResponse::rejected("test rejection");
            write_json(&mut stream, &resp).await.unwrap();
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = JoinRequest {
            name: "test-node".into(),
            region: "eu".into(),
            zone: "fsn1".into(),
            wg_public_key: "key".into(),
            wg_port: 51820,
            endpoint: None,
            pin: Some("1234".into()),
        };
        write_json(&mut client, &req).await.unwrap();

        let resp: JoinResponse = read_json(&mut client).await.unwrap();
        assert!(!resp.accepted);
        assert_eq!(resp.reason.as_deref(), Some("test rejection"));

        server.await.unwrap();
    }
}
