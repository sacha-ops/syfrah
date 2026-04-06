//! Peer announcement — broadcasts new peers to all existing mesh members.
//!
//! When a new node joins, the accepting node sends a PeerAnnounce message
//! to every existing peer so they can add the new node to their WireGuard config.
//!
//! Protocol: TCP connect → send PeerAnnounce → close.
//! Same length-prefixed JSON as the peering protocol.
//!
//! Announcements are best-effort: if a peer is unreachable, we skip it
//! and rely on the next health check or manual sync.

use std::time::Duration;

use syfrah_core::error::SyfrahError;

use super::peer::Peer;
use super::peering::{PeerAnnounce, PeerInfo};
use super::peering_server::{read_json, write_json};
use super::service;

/// Default announce port offset from WireGuard port.
pub const ANNOUNCE_PORT_OFFSET: u16 = 2;

/// Default timeout for each announce connection.
const ANNOUNCE_TIMEOUT_SECS: u64 = 5;

/// Broadcast a new peer to all existing peers.
///
/// Connects to each peer's announce port (mesh_ipv6:wg_port+2) and sends
/// a PeerAnnounce message. Best-effort: failures are logged but don't
/// stop the broadcast.
///
/// Returns (successes, failures).
pub async fn broadcast_new_peer(
    new_peer: &PeerInfo,
    announced_by: &str,
    existing_peers: &[Peer],
    wg_port: u16,
) -> (usize, usize) {
    let announce = PeerAnnounce {
        peer: new_peer.clone(),
        announced_by: announced_by.to_string(),
    };

    let mut successes = 0;
    let mut failures = 0;

    for peer in existing_peers {
        let addr = format!(
            "[{}]:{}",
            peer.mesh_ipv6,
            wg_port + ANNOUNCE_PORT_OFFSET
        );

        match send_announce(&addr, &announce).await {
            Ok(()) => {
                tracing::info!(
                    peer = %peer.name,
                    new_peer = %new_peer.name,
                    "announced new peer"
                );
                successes += 1;
            }
            Err(e) => {
                tracing::warn!(
                    peer = %peer.name,
                    error = %e,
                    "failed to announce new peer"
                );
                failures += 1;
            }
        }
    }

    (successes, failures)
}

/// Send a single PeerAnnounce to one peer.
async fn send_announce(addr: &str, announce: &PeerAnnounce) -> Result<(), SyfrahError> {
    let timeout = Duration::from_secs(ANNOUNCE_TIMEOUT_SECS);

    let mut stream = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| SyfrahError::timeout("announce connection", ANNOUNCE_TIMEOUT_SECS))?
        .map_err(|e| SyfrahError::network(format!("announce connect failed: {e}")))?;

    write_json(&mut stream, announce).await?;

    Ok(())
}

/// Listen for peer announcements and apply them.
///
/// Runs on wg_port + 2. When an announcement arrives:
/// 1. Add the new peer to our PeerList
/// 2. Update WireGuard config
/// 3. Persist state
///
/// Opens DB per-request to avoid lock contention.
pub async fn listen(
    db_opener: impl Fn() -> Result<syfrah_state::LayerDb, SyfrahError>,
    bind_addr: std::net::SocketAddr,
) -> Result<(), SyfrahError> {
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| SyfrahError::internal(format!("failed to bind announce port: {e}")))?;

    tracing::info!(addr = %bind_addr, "announce listener started");

    loop {
        match listener.accept().await {
            Ok((mut stream, peer_addr)) => {
                let db = match db_opener() {
                    Ok(db) => db,
                    Err(e) => {
                        tracing::warn!(error = %e, "announce: failed to open DB");
                        continue;
                    }
                };

                match handle_announce(&mut stream, &db).await {
                    Ok(name) => {
                        tracing::info!(
                            from = %peer_addr,
                            new_peer = %name,
                            "applied peer announcement"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(from = %peer_addr, error = %e, "announce failed");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "announce accept error");
            }
        }
    }
}

/// Handle a single peer announcement.
async fn handle_announce(
    stream: &mut tokio::net::TcpStream,
    db: &syfrah_state::LayerDb,
) -> Result<String, SyfrahError> {
    let announce: PeerAnnounce = read_json(stream).await?;

    // Validate peer data from untrusted source
    super::peering_server::validate_peer_field(&announce.peer.name, "name")?;
    super::peering_server::validate_peer_field(&announce.peer.region, "region")?;
    super::peering_server::validate_peer_field(&announce.peer.zone, "zone")?;
    super::peering_server::validate_peer_field(&announce.peer.wg_public_key, "wg_public_key")?;

    let mut state = super::state::FabricState::load(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?
        .ok_or_else(|| SyfrahError::precondition("not initialized"))?;

    let peer_name = announce.peer.name.clone();

    // Skip if we already know this peer
    if state.peers.find_by_key(&announce.peer.wg_public_key).is_some() {
        tracing::debug!(peer = %peer_name, "already known, skipping");
        return Ok(peer_name);
    }

    // Add the new peer
    let new_peer = Peer::new(
        announce.peer.name,
        announce.peer.region,
        announce.peer.zone,
        announce.peer.wg_public_key,
        announce.peer.wg_port,
        announce.peer.endpoint,
        announce.peer.mesh_ipv6,
    );
    if let Err(e) = state.peers.add(new_peer) {
        tracing::debug!(error = %e, "announce: peer already known");
        return Ok(peer_name);
    }

    // Persist immediately
    state
        .save(db)
        .map_err(|e| SyfrahError::internal(e.to_string()))?;

    // Update WireGuard config
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

    Ok(peer_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_port_offset() {
        assert_eq!(ANNOUNCE_PORT_OFFSET, 2);
        // WG port 51820 → announce port 51822
        let wg_port: u16 = 51820;
        assert_eq!(wg_port + ANNOUNCE_PORT_OFFSET, 51822);
    }

    #[tokio::test]
    async fn broadcast_to_empty_list() {
        let peer_info = PeerInfo {
            name: "new-node".into(),
            region: "eu".into(),
            zone: "fsn1".into(),
            wg_public_key: "key".into(),
            wg_port: 51820,
            endpoint: None,
            mesh_ipv6: "fd01::99".parse().unwrap(),
        };
        let (ok, fail) = broadcast_new_peer(&peer_info, "node-1", &[], 51820).await;
        assert_eq!(ok, 0);
        assert_eq!(fail, 0);
    }

    #[tokio::test]
    async fn announce_roundtrip() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let announce: PeerAnnounce = read_json(&mut stream).await.unwrap();
            assert_eq!(announce.peer.name, "new-node");
            assert_eq!(announce.announced_by, "node-1");
        });

        let announce = PeerAnnounce {
            peer: PeerInfo {
                name: "new-node".into(),
                region: "eu".into(),
                zone: "fsn1".into(),
                wg_public_key: "key".into(),
                wg_port: 51820,
                endpoint: None,
                mesh_ipv6: "fd01::99".parse().unwrap(),
            },
            announced_by: "node-1".into(),
        };

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        write_json(&mut stream, &announce).await.unwrap();
        drop(stream);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_announce_unreachable() {
        let announce = PeerAnnounce {
            peer: PeerInfo {
                name: "n".into(),
                region: "eu".into(),
                zone: "fsn1".into(),
                wg_public_key: "k".into(),
                wg_port: 51820,
                endpoint: None,
                mesh_ipv6: "fd01::1".parse().unwrap(),
            },
            announced_by: "test".into(),
        };
        // Connect to a port nothing is listening on
        let result = send_announce("127.0.0.1:1", &announce).await;
        assert!(result.is_err());
    }
}
