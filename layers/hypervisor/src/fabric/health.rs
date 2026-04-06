//! Health check — monitors peer reachability via WireGuard handshakes.
//!
//! Polls `wg show dump` at a configurable interval and updates peer status:
//! - Handshake within threshold → Active
//! - Handshake older than threshold → Unreachable
//!
//! Designed to run as a background tokio task during `peering` or future daemon mode.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use syfrah_core::error::SyfrahError;
use syfrah_state::LayerDb;

use super::state::FabricState;
use super::wg;

/// Default health check interval.
pub const DEFAULT_INTERVAL_SECS: u64 = 30;

/// Default handshake staleness threshold (5 minutes).
/// WireGuard sends a handshake every 2 minutes when there's traffic,
/// so 5 minutes without one means the peer is likely down.
pub const DEFAULT_STALE_THRESHOLD_SECS: u64 = 300;

/// Result of a single health check sweep.
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    /// Total peers checked.
    pub total: usize,
    /// Peers marked active after this check.
    pub active: usize,
    /// Peers marked unreachable after this check.
    pub unreachable: usize,
    /// Peers whose status changed.
    pub changed: usize,
}

/// Run a single health check sweep.
///
/// Reads WireGuard handshake timestamps, compares against threshold,
/// updates peer status in state, and persists if anything changed.
pub fn check_once(
    db: &LayerDb,
    stale_threshold_secs: u64,
) -> Result<HealthCheckResult, SyfrahError> {
    let mut state = match FabricState::load(db).map_err(|e| SyfrahError::internal(e.to_string()))? {
        Some(s) => s,
        None => return Err(SyfrahError::precondition("not initialized")),
    };

    if state.peers.is_empty() {
        return Ok(HealthCheckResult {
            total: 0,
            active: 0,
            unreachable: 0,
            changed: 0,
        });
    }

    // Get handshake info from WireGuard
    let handshakes = match wg::get_peer_handshakes() {
        Ok(h) => h,
        Err(_) => {
            // WG interface not up — can't check
            return Ok(HealthCheckResult {
                total: state.peers.len(),
                active: 0,
                unreachable: state.peers.len(),
                changed: 0,
            });
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut changed = 0;

    for peer in &mut state.peers.peers {
        let was_active = peer.is_active();

        if let Some(wg_peer) = handshakes.iter().find(|h| h.public_key == peer.wg_public_key) {
            peer.last_handshake = wg_peer.latest_handshake;

            if wg_peer.latest_handshake > 0
                && now.saturating_sub(wg_peer.latest_handshake) <= stale_threshold_secs
            {
                peer.status = super::peer::PeerStatus::Active;
            } else {
                peer.status = super::peer::PeerStatus::Unreachable;
            }
        } else {
            // Peer not in WireGuard at all — unreachable
            peer.status = super::peer::PeerStatus::Unreachable;
        }

        if was_active != peer.is_active() {
            changed += 1;
            if peer.is_active() {
                tracing::info!(peer = %peer.name, "peer now active");
            } else {
                tracing::warn!(peer = %peer.name, "peer now unreachable");
            }
        }
    }

    let active = state.peers.active_count();
    let unreachable = state.peers.unreachable_count();
    let total = state.peers.len();

    // Only persist if something changed
    if changed > 0 {
        state
            .save(db)
            .map_err(|e| SyfrahError::internal(e.to_string()))?;
    }

    Ok(HealthCheckResult {
        total,
        active,
        unreachable,
        changed,
    })
}

/// Run the health check loop. Blocks until cancelled.
///
/// Opens a fresh DB connection each sweep to avoid holding the lock.
pub async fn run_loop(
    db_opener: impl Fn() -> Result<LayerDb, SyfrahError>,
    interval_secs: u64,
    stale_threshold_secs: u64,
) {
    let interval = Duration::from_secs(interval_secs);

    loop {
        tokio::time::sleep(interval).await;

        let db = match db_opener() {
            Ok(db) => db,
            Err(e) => {
                tracing::warn!(error = %e, "health check: failed to open DB");
                continue;
            }
        };

        match check_once(&db, stale_threshold_secs) {
            Ok(result) => {
                if result.changed > 0 {
                    tracing::info!(
                        total = result.total,
                        active = result.active,
                        unreachable = result.unreachable,
                        changed = result.changed,
                        "health check complete"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "health check failed");
            }
        }

        // DB dropped here — lock released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_check_result_defaults() {
        let r = HealthCheckResult {
            total: 3,
            active: 2,
            unreachable: 1,
            changed: 1,
        };
        assert_eq!(r.total, 3);
        assert_eq!(r.active, 2);
        assert_eq!(r.changed, 1);
    }

    #[test]
    fn constants_sensible() {
        assert_eq!(DEFAULT_INTERVAL_SECS, 30);
        assert_eq!(DEFAULT_STALE_THRESHOLD_SECS, 300);
        assert!(DEFAULT_STALE_THRESHOLD_SECS > DEFAULT_INTERVAL_SECS);
    }

    #[test]
    fn check_once_no_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = LayerDb::open_at(&dir.path().join("test.redb")).unwrap();
        let result = check_once(&db, 300);
        assert!(result.is_err()); // not initialized
    }

    #[test]
    fn check_once_no_peers() {
        let dir = tempfile::tempdir().unwrap();
        let db = LayerDb::open_at(&dir.path().join("test.redb")).unwrap();

        // Create state with no peers
        let (mesh, secret) = super::super::mesh::create_mesh();
        let hv = super::super::mesh::create_hypervisor(
            "node-1", "eu", "fsn1", 51820, None, "", &mesh.prefix,
        )
        .unwrap();
        let state = FabricState {
            mesh,
            hypervisor: hv,
            secret: secret.to_string(),
            peers: super::super::peer::PeerList::new(),
            network_mode: super::super::backend::NetworkMode::default(),
        };
        state.save(&db).unwrap();

        let result = check_once(&db, 300).unwrap();
        assert_eq!(result.total, 0);
        assert_eq!(result.active, 0);
        assert_eq!(result.changed, 0);
    }

    #[test]
    fn check_once_with_peers_no_wg() {
        let dir = tempfile::tempdir().unwrap();
        let db = LayerDb::open_at(&dir.path().join("test.redb")).unwrap();

        let (mesh, secret) = super::super::mesh::create_mesh();
        let hv = super::super::mesh::create_hypervisor(
            "node-1", "eu", "fsn1", 51820, None, "", &mesh.prefix,
        )
        .unwrap();
        let mut peers = super::super::peer::PeerList::new();
        peers
            .add(super::super::peer::Peer::new(
                "node-2".into(),
                "eu".into(),
                "nbg1".into(),
                "key-2".into(),
                51820,
                None,
                "fd01::2".parse().unwrap(),
            ))
            .unwrap();

        let state = FabricState {
            mesh,
            hypervisor: hv,
            secret: secret.to_string(),
            peers,
            network_mode: super::super::backend::NetworkMode::default(),
        };
        state.save(&db).unwrap();

        // WG not running → get_peer_handshakes fails → all unreachable, changed=0
        let result = check_once(&db, 300).unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.unreachable, 1);
    }
}
