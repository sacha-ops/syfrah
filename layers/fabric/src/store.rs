use std::collections::HashMap;
use std::fs;
use std::net::Ipv6Addr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use syfrah_core::mesh::PeerRecord;
use syfrah_state::LayerDb;

const LAYER_NAME: &str = "fabric";

/// Minimum interval between JSON state exports. Peer updates that arrive
/// faster than this are persisted in redb (the source of truth) but the
/// legacy `state.json` file is only regenerated once the cooldown expires.
const JSON_DEBOUNCE_SECS: u64 = 2;

/// Tracks the last time `state.json` was written so we can debounce.
static LAST_JSON_WRITE: Mutex<Option<Instant>> = Mutex::new(None);

/// In-memory index for O(1) peer lookups by WG public key.
/// Lazily populated on first access; kept in sync by upsert/remove operations.
static PEER_INDEX: Mutex<Option<HashMap<String, PeerRecord>>> = Mutex::new(None);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("state error: {0}")]
    State(#[from] syfrah_state::StateError),
    #[error("no state found at {0}")]
    NotFound(PathBuf),
}

/// Persisted state for a mesh node.
/// This struct is used for backward-compatible load/save operations.
/// Internally, data is stored in redb tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    pub mesh_name: String,
    /// The mesh secret (syf_sk_...)
    pub mesh_secret: String,
    pub wg_private_key: String,
    pub wg_public_key: String,
    pub mesh_ipv6: Ipv6Addr,
    pub mesh_prefix: Ipv6Addr,
    pub wg_listen_port: u16,
    pub node_name: String,
    #[serde(default)]
    pub public_endpoint: Option<std::net::SocketAddr>,
    #[serde(default = "default_peering_port")]
    pub peering_port: u16,
    pub peers: Vec<PeerRecord>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
    #[serde(default)]
    pub metrics: Metrics,
}

fn default_peering_port() -> u16 {
    51821
}

/// Path to the Unix domain socket for CLI-daemon control.
pub fn control_socket_path() -> PathBuf {
    state_dir().join("control.sock")
}

/// Simple counters for observability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub peers_discovered: u64,
    pub wg_reconciliations: u64,
    pub peers_marked_unreachable: u64,
    pub announcements_failed: u64,
    pub daemon_started_at: u64,
    #[serde(default)]
    pub announces_dropped: u64,
    #[serde(default)]
    pub announces_queued: u64,
    #[serde(default)]
    pub announces_queue_full: u64,
    #[serde(default)]
    pub peer_limit_reached: u64,
}

fn state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
}

/// Create the state directory if it doesn't exist and set permissions to 0o700.
fn ensure_state_dir() -> Result<(), StoreError> {
    let dir = state_dir();
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn state_file() -> PathBuf {
    state_dir().join("state.json")
}

/// Check if a mesh state exists.
/// Checks both legacy JSON and new redb.
pub fn exists() -> bool {
    state_file().exists() || LayerDb::layer_exists(LAYER_NAME)
}

/// Save node state.
/// Writes to both redb (primary) and JSON (backward compat for E2E tests).
pub fn save(state: &NodeState) -> Result<(), StoreError> {
    ensure_state_dir()?;
    let dir = state_dir();

    // Write to redb
    let db = open_db()?;
    db.batch(|w| {
        w.set("config", "mesh_name", &state.mesh_name)?;
        w.set("config", "mesh_secret", &state.mesh_secret)?;
        w.set("config", "wg_private_key", &state.wg_private_key)?;
        w.set("config", "wg_public_key", &state.wg_public_key)?;
        w.set("config", "mesh_ipv6", &state.mesh_ipv6)?;
        w.set("config", "mesh_prefix", &state.mesh_prefix)?;
        w.set("config", "wg_listen_port", &state.wg_listen_port)?;
        w.set("config", "node_name", &state.node_name)?;
        w.set("config", "public_endpoint", &state.public_endpoint)?;
        w.set("config", "peering_port", &state.peering_port)?;
        w.set("config", "region", &state.region)?;
        w.set("config", "zone", &state.zone)?;
        w.set_metric("peers_discovered", state.metrics.peers_discovered)?;
        w.set_metric("wg_reconciliations", state.metrics.wg_reconciliations)?;
        w.set_metric(
            "peers_marked_unreachable",
            state.metrics.peers_marked_unreachable,
        )?;
        w.set_metric("announcements_failed", state.metrics.announcements_failed)?;
        w.set_metric("daemon_started_at", state.metrics.daemon_started_at)?;
        w.set_metric("announces_dropped", state.metrics.announces_dropped)?;
        w.set_metric("announces_queued", state.metrics.announces_queued)?;
        w.set_metric("announces_queue_full", state.metrics.announces_queue_full)?;
        w.set_metric("peer_limit_reached", state.metrics.peer_limit_reached)?;
        Ok(())
    })?;

    // Sync peers: clear existing and re-add all
    // (for full save compat — atomic peer ops should be preferred)
    let existing: Vec<(String, PeerRecord)> = db.list("peers")?;
    for (key, _) in &existing {
        db.delete("peers", key)?;
    }
    for peer in &state.peers {
        db.set("peers", &peer.wg_public_key, peer)?;
    }

    // Invalidate the in-memory peer index so it rebuilds from redb
    invalidate_peer_index();

    // Also write legacy JSON for backward compat with E2E tests
    // that inspect state.json directly
    let file = state_file();
    let tmp = dir.join("state.json.tmp");
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&tmp, &json)?;
    fs::rename(&tmp, &file)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// Load node state.
/// Tries redb first, falls back to legacy JSON.
pub fn load() -> Result<NodeState, StoreError> {
    // Try redb first
    if LayerDb::layer_exists(LAYER_NAME) {
        if let Ok(state) = load_from_redb() {
            return Ok(state);
        }
    }

    // Fallback to legacy JSON
    let file = state_file();
    if !file.exists() {
        return Err(StoreError::NotFound(file));
    }
    let json = fs::read_to_string(&file)?;
    let state: NodeState = serde_json::from_str(&json)?;
    Ok(state)
}

/// Load state from redb.
fn load_from_redb() -> Result<NodeState, StoreError> {
    let db = open_db()?;
    load_from_redb_with(&db)
}

/// Load state from an existing redb connection.
fn load_from_redb_with(db: &LayerDb) -> Result<NodeState, StoreError> {
    let mesh_name: String = db
        .get("config", "mesh_name")?
        .ok_or_else(|| StoreError::NotFound(syfrah_state::db_path(LAYER_NAME)))?;
    let mesh_secret: String = db
        .get("config", "mesh_secret")?
        .ok_or_else(|| StoreError::NotFound(syfrah_state::db_path(LAYER_NAME)))?;
    let wg_private_key: String = db
        .get("config", "wg_private_key")?
        .ok_or_else(|| StoreError::NotFound(syfrah_state::db_path(LAYER_NAME)))?;
    let wg_public_key: String = db
        .get("config", "wg_public_key")?
        .ok_or_else(|| StoreError::NotFound(syfrah_state::db_path(LAYER_NAME)))?;
    let mesh_ipv6: Ipv6Addr = db
        .get("config", "mesh_ipv6")?
        .ok_or_else(|| StoreError::NotFound(syfrah_state::db_path(LAYER_NAME)))?;
    let mesh_prefix: Ipv6Addr = db
        .get("config", "mesh_prefix")?
        .ok_or_else(|| StoreError::NotFound(syfrah_state::db_path(LAYER_NAME)))?;
    let wg_listen_port: u16 = db.get("config", "wg_listen_port")?.unwrap_or(51820);
    let node_name: String = db.get("config", "node_name")?.unwrap_or_default();
    let public_endpoint: Option<std::net::SocketAddr> =
        db.get("config", "public_endpoint")?.unwrap_or(None);
    let peering_port: u16 = db.get("config", "peering_port")?.unwrap_or(51821);
    let region: Option<String> = db.get("config", "region")?.unwrap_or(None);
    let zone: Option<String> = db.get("config", "zone")?.unwrap_or(None);

    let peer_entries: Vec<(String, PeerRecord)> = db.list("peers")?;
    let peers: Vec<PeerRecord> = peer_entries
        .into_iter()
        .map(|(_, mut p)| {
            p.ensure_topology();
            p
        })
        .collect();

    let metrics = Metrics {
        peers_discovered: db.get_metric("peers_discovered")?,
        wg_reconciliations: db.get_metric("wg_reconciliations")?,
        peers_marked_unreachable: db.get_metric("peers_marked_unreachable")?,
        announcements_failed: db.get_metric("announcements_failed")?,
        daemon_started_at: db.get_metric("daemon_started_at")?,
        announces_dropped: db.get_metric("announces_dropped")?,
        announces_queued: db.get_metric("announces_queued")?,
        announces_queue_full: db.get_metric("announces_queue_full")?,
        peer_limit_reached: db.get_metric("peer_limit_reached")?,
    };

    Ok(NodeState {
        mesh_name,
        mesh_secret,
        wg_private_key,
        wg_public_key,
        mesh_ipv6,
        mesh_prefix,
        wg_listen_port,
        node_name,
        public_endpoint,
        peering_port,
        peers,
        region,
        zone,
        metrics,
    })
}

/// Generate a zone name based on region and existing peers.
/// Format: zone-{next_index}
///
/// When existing peers already carry zone labels, `next_index` is one past
/// the highest zone index found. When no zone labels exist yet, the peer
/// count is used as the starting index so that each peer can be assigned a
/// unique zone.
///
/// For backward compatibility, both `zone-{N}` and legacy `{region}-zone-{N}`
/// formats are recognized when scanning existing peers.
pub fn generate_zone(region: &str, existing_peers: &[PeerRecord]) -> String {
    let new_prefix = "zone-";
    let legacy_prefix = format!("{region}-zone-");
    let max_zone_index = existing_peers
        .iter()
        .filter_map(|p| {
            p.zone.as_ref().and_then(|z| {
                z.strip_prefix(new_prefix)
                    .or_else(|| z.strip_prefix(&legacy_prefix))
                    .and_then(|suffix| suffix.parse::<u32>().ok())
            })
        })
        .max();
    let next_index = match max_zone_index {
        Some(idx) => idx + 1,
        None => {
            // No peers have zone labels yet; use peer count so each peer
            // can still receive a distinct zone.
            let peer_count = existing_peers.len() as u32;
            peer_count + 1
        }
    };
    format!("zone-{next_index}")
}

/// Delete all state (redb + JSON + entire directory).
pub fn clear() -> Result<(), StoreError> {
    let dir = state_dir();
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    invalidate_peer_index();
    Ok(())
}

// ── Peer index helpers ──────────────────────────────────────

/// Ensure the in-memory peer index is populated. Call with the lock held.
fn ensure_peer_index(index: &mut Option<HashMap<String, PeerRecord>>) {
    if index.is_none() {
        let map = if LayerDb::layer_exists(LAYER_NAME) {
            if let Ok(db) = open_db() {
                let entries: Vec<(String, PeerRecord)> = db.list("peers").unwrap_or_default();
                entries.into_iter().collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };
        *index = Some(map);
    }
}

/// Look up a peer by WG public key in O(1) via the in-memory index.
pub fn peer_by_key(wg_public_key: &str) -> Option<PeerRecord> {
    let mut guard = PEER_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_peer_index(&mut guard);
    guard.as_ref().unwrap().get(wg_public_key).cloned()
}

/// Invalidate the peer index so it is rebuilt on next access.
/// Used after bulk operations like `save()` or `clear()`.
fn invalidate_peer_index() {
    let mut guard = PEER_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

/// Insert or update a single entry in the peer index.
fn index_upsert(peer: &PeerRecord) {
    let mut guard = PEER_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_peer_index(&mut guard);
    guard
        .as_mut()
        .unwrap()
        .insert(peer.wg_public_key.clone(), peer.clone());
}

/// Update a peer in the index by key (used after status changes like Remove).
fn index_update(key: &str, peer: &PeerRecord) {
    let mut guard = PEER_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_peer_index(&mut guard);
    guard
        .as_mut()
        .unwrap()
        .insert(key.to_string(), peer.clone());
}

// ── Atomic peer operations (new, fixes race condition) ──────

/// Add a peer atomically. If the peer already exists (by WG key), it's updated.
///
/// Writes to redb (atomic, single source of truth), then regenerates the
/// legacy JSON export from redb. This eliminates the race condition where
/// concurrent upserts could overwrite each other's JSON changes.
pub fn upsert_peer(peer: &PeerRecord) -> Result<(), StoreError> {
    let db = open_db()?;
    db.set("peers", &peer.wg_public_key, peer)?;

    // Update the in-memory index
    index_upsert(peer);

    // Regenerate JSON from redb only if the debounce window has elapsed.
    // redb is the source of truth; JSON is a best-effort export for
    // backward compat, so skipping a write here is safe.
    maybe_write_json(&db);
    Ok(())
}

/// Add a peer atomically, but only if the peer count is below `max_peers`.
/// If the peer already exists (by WG key), the update always succeeds
/// (it doesn't increase the count). Returns `true` if stored, `false` if
/// the limit was reached and the peer is new.
pub fn upsert_peer_bounded(peer: &PeerRecord, max_peers: usize) -> Result<bool, StoreError> {
    let db = open_db()?;

    // Check if this peer already exists (updates are always allowed)
    if !db.exists("peers", &peer.wg_public_key)? {
        let count = db.count("peers")? as usize;
        if count >= max_peers {
            return Ok(false);
        }
    }

    db.set("peers", &peer.wg_public_key, peer)?;
    index_upsert(peer);
    maybe_write_json(&db);
    Ok(true)
}

/// Return the number of stored peers.
pub fn peer_count() -> Result<usize, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(0);
    }
    let db = open_db()?;
    Ok(db.count("peers")? as usize)
}

/// Return the peer count and whether a specific peer exists in a single read transaction.
pub fn peer_count_and_exists(wg_public_key: &str) -> Result<(usize, bool), StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok((0, false));
    }
    let db = open_db()?;
    let (count, exists) = db.count_and_exists("peers", wg_public_key)?;
    Ok((count as usize, exists))
}

/// Mark all peers with the given node name as Removed, except the one with
/// `except_wg_key`. Returns the number of peers marked as Removed.
///
/// This is used during join/init to purge phantom peers left by previous
/// init/join cycles of the same node name with different WG keys.
pub fn purge_stale_peers_by_name(name: &str, except_wg_key: &str) -> Result<usize, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(0);
    }
    let db = open_db()?;
    let entries: Vec<(String, PeerRecord)> = db.list("peers")?;
    let mut purged = 0;
    for (key, mut peer) in entries {
        if peer.name == name
            && peer.wg_public_key != except_wg_key
            && peer.status != syfrah_core::mesh::PeerStatus::Removed
        {
            peer.status = syfrah_core::mesh::PeerStatus::Removed;
            db.set("peers", &key, &peer)?;
            index_update(&key, &peer);
            purged += 1;
        }
    }
    if purged > 0 {
        maybe_write_json(&db);
    }
    Ok(purged)
}

/// Mark a peer as Removed by name or WG public key.
/// Returns the updated PeerRecord if found, or None if no matching peer exists.
pub fn remove_peer(name_or_key: &str) -> Result<Option<PeerRecord>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(None);
    }
    let db = open_db()?;
    let entries: Vec<(String, PeerRecord)> = db.list("peers")?;
    let found = entries
        .into_iter()
        .find(|(_, p)| p.name == name_or_key || p.wg_public_key == name_or_key);
    match found {
        Some((key, mut peer)) => {
            peer.status = syfrah_core::mesh::PeerStatus::Removed;
            db.set("peers", &key, &peer)?;
            index_update(&key, &peer);
            maybe_write_json(&db);
            Ok(Some(peer))
        }
        None => Ok(None),
    }
}

/// Update a peer's endpoint by name or WG public key.
/// Returns `Ok(Some((old_endpoint, updated_peer)))` if found, or `Ok(None)` if
/// no matching peer exists.
pub fn update_peer_endpoint(
    name_or_key: &str,
    new_endpoint: std::net::SocketAddr,
) -> Result<Option<(std::net::SocketAddr, PeerRecord)>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(None);
    }
    let db = open_db()?;
    let entries: Vec<(String, PeerRecord)> = db.list("peers")?;
    let found = entries
        .into_iter()
        .find(|(_, p)| p.name == name_or_key || p.wg_public_key == name_or_key);
    match found {
        Some((key, mut peer)) => {
            let old_endpoint = peer.endpoint;
            peer.endpoint = new_endpoint;
            db.set("peers", &key, &peer)?;
            index_update(&key, &peer);
            maybe_write_json(&db);
            Ok(Some((old_endpoint, peer)))
        }
        None => Ok(None),
    }
}

/// Get all peers from redb.
pub fn get_peers() -> Result<Vec<PeerRecord>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(vec![]);
    }
    let db = open_db()?;
    let entries: Vec<(String, PeerRecord)> = db.list("peers")?;
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

/// Garbage-collect peers that have been in `Removed` status for longer than
/// `threshold_secs`. Returns the number of peers deleted.
pub fn gc_removed_peers(threshold_secs: u64) -> Result<usize, StoreError> {
    if threshold_secs == 0 {
        return Ok(0);
    }
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(0);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let db = open_db()?;
    let entries: Vec<(String, PeerRecord)> = db.list("peers")?;
    let mut removed = 0usize;
    for (key, peer) in &entries {
        if peer.status == syfrah_core::mesh::PeerStatus::Removed
            && now.saturating_sub(peer.last_seen) >= threshold_secs
        {
            db.delete("peers", key)?;
            removed += 1;
        }
    }
    if removed > 0 {
        maybe_write_json(&db);
    }
    Ok(removed)
}

// ── Zone health persistence ─────────────────────────────────

/// Persist the health status for a zone.
pub fn set_zone_health(
    zone_name: &str,
    status: crate::events::ZoneHealthStatus,
) -> Result<(), StoreError> {
    let db = open_db()?;
    db.set("zone_health", zone_name, &status)?;
    Ok(())
}

/// Load the health status for a specific zone.
pub fn get_zone_health(
    zone_name: &str,
) -> Result<Option<crate::events::ZoneHealthStatus>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(None);
    }
    let db = open_db()?;
    Ok(db.get("zone_health", zone_name)?)
}

/// Load all persisted zone health statuses.
pub fn list_zone_health() -> Result<Vec<(String, crate::events::ZoneHealthStatus)>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(vec![]);
    }
    let db = open_db()?;
    Ok(db.list("zone_health")?)
}

// ── Zone drain state ────────────────────────────────────────

/// Set the drain state for a zone. `true` = draining, `false` = active.
pub fn set_zone_drain(zone_name: &str, draining: bool) -> Result<(), StoreError> {
    let db = open_db()?;
    db.set("zone_drain", zone_name, &draining)?;
    Ok(())
}

/// Get the drain state for a specific zone.
pub fn get_zone_drain(zone_name: &str) -> Result<Option<bool>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(None);
    }
    let db = open_db()?;
    Ok(db.get("zone_drain", zone_name)?)
}

/// List all zone drain states.
pub fn list_zone_drain() -> Result<Vec<(String, bool)>, StoreError> {
    if !LayerDb::layer_exists(LAYER_NAME) {
        return Ok(vec![]);
    }
    let db = open_db()?;
    Ok(db.list("zone_drain")?)
}

// ── Metrics (atomic) ────────────────────────────────────────

/// Increment a metric atomically.
pub fn inc_metric(name: &str, delta: u64) -> Result<u64, StoreError> {
    let db = open_db()?;
    Ok(db.inc_metric(name, delta)?)
}

/// Set a metric atomically.
pub fn set_metric(name: &str, value: u64) -> Result<(), StoreError> {
    let db = open_db()?;
    Ok(db.set_metric(name, value)?)
}

// ── Internal helpers ────────────────────────────────────────

fn open_db() -> Result<LayerDb, StoreError> {
    Ok(LayerDb::open(LAYER_NAME)?)
}

/// Write JSON from redb only if the debounce window has elapsed.
/// Called after every peer upsert to avoid rewriting `state.json` on
/// every single peer update.
fn maybe_write_json(db: &LayerDb) {
    let should_write = {
        let guard = LAST_JSON_WRITE.lock().unwrap_or_else(|e| e.into_inner());
        match *guard {
            Some(last) => last.elapsed().as_secs() >= JSON_DEBOUNCE_SECS,
            None => true, // never written yet
        }
    };
    if should_write {
        if let Ok(state) = load_from_redb_with(db) {
            if save_json_only(&state).is_ok() {
                let mut guard = LAST_JSON_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(Instant::now());
            }
        }
    }
}

/// Force-flush the legacy `state.json` from the current redb state.
/// Call this on daemon shutdown or when an up-to-date JSON export is needed
/// immediately (e.g., before an E2E test reads the file).
pub fn flush_json() -> Result<(), StoreError> {
    let db = open_db()?;
    let state = load_from_redb_with(&db)?;
    save_json_only(&state)?;
    let mut guard = LAST_JSON_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(Instant::now());
    Ok(())
}

/// Write JSON only (no redb) for backward compat.
fn save_json_only(state: &NodeState) -> Result<(), StoreError> {
    ensure_state_dir()?;
    let dir = state_dir();
    let file = state_file();
    let tmp = dir.join("state.json.tmp");
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&tmp, &json)?;
    fs::rename(&tmp, &file)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

// ── PID management (filesystem-based, not redb) ─────────────

fn pid_file() -> PathBuf {
    state_dir().join("daemon.pid")
}

/// Public accessor for the PID file path.
pub fn pid_path() -> PathBuf {
    pid_file()
}

/// Write the current process PID to the PID file with exclusive flock.
///
/// Uses flock(LOCK_EX | LOCK_NB) to prevent two daemons from running
/// simultaneously. The PID is written atomically via a temp file + rename.
/// The lock file is returned and must be kept alive for the daemon's lifetime.
#[cfg(unix)]
pub fn write_pid() -> Result<fs::File, StoreError> {
    use std::io::Write;

    ensure_state_dir()?;
    let dir = state_dir();

    let path = pid_file();

    // Open (or create) the PID file and acquire an exclusive lock.
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let existing = fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let msg = match existing {
            Some(pid) => format!("another daemon is already running (pid {pid})"),
            None => "another daemon is already running (pid unknown)".to_string(),
        };
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            msg,
        )));
    }

    // Write PID atomically: write to temp file, then rename over the lock file.
    // After rename the fd still holds the flock on the same inode.
    let tmp = dir.join("daemon.pid.tmp");
    fs::write(&tmp, std::process::id().to_string())?;
    fs::rename(&tmp, &path)?;

    // Re-acquire lock on the new inode after rename (rename replaces the file).
    // The original fd still points to the old inode, so re-open + re-lock.
    let file = fs::OpenOptions::new()
        .create(false)
        .read(true)
        .write(true)
        .open(&path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "failed to re-acquire PID file lock after atomic write",
        )));
    }

    // Restrict permissions
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
    }

    // Ensure PID content is correct on the locked fd
    file.set_len(0)?;
    let mut f = &file;
    write!(f, "{}", std::process::id())?;

    Ok(file)
}

/// Non-unix fallback (no flock).
#[cfg(not(unix))]
pub fn write_pid() -> Result<(), StoreError> {
    ensure_state_dir()?;
    fs::write(pid_file(), std::process::id().to_string())?;
    Ok(())
}

/// Read the daemon PID from the PID file. Returns None if not found.
pub fn read_pid() -> Option<u32> {
    fs::read_to_string(pid_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Remove the PID file.
pub fn remove_pid() {
    let _ = fs::remove_file(pid_file());
}

/// Check if a daemon is currently running (PID file exists and process alive, not zombie).
/// Also cleans up stale PID files when the process is dead.
pub fn daemon_running() -> Option<u32> {
    let pid = read_pid()?;
    #[cfg(unix)]
    {
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if !alive {
            // Stale PID file — process is dead. Clean up automatically.
            remove_pid();
            return None;
        }
        // Check for zombie: kill(pid,0) succeeds for zombies too
        if is_zombie(pid) {
            remove_pid();
            return None;
        }
        Some(pid)
    }
    #[cfg(not(unix))]
    {
        Some(pid)
    }
}

/// Check if a PID belongs to a syfrah process.
/// Returns true if the process cmdline/name contains "syfrah".
#[cfg(target_os = "linux")]
pub fn is_syfrah_process(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .map(|c| c.contains("syfrah"))
        .unwrap_or(false)
}

/// On macOS, use `ps` to check the process name.
#[cfg(target_os = "macos")]
pub fn is_syfrah_process(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("syfrah"))
        .unwrap_or(false)
}

/// Non-unix fallback — cannot verify process name.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn is_syfrah_process(_pid: u32) -> bool {
    true
}

/// Check if a process is a zombie by reading /proc/PID/status on Linux.
#[cfg(target_os = "linux")]
fn is_zombie(pid: u32) -> bool {
    let status_path = format!("/proc/{pid}/status");
    if let Ok(contents) = fs::read_to_string(status_path) {
        for line in contents.lines() {
            if let Some(state) = line.strip_prefix("State:") {
                return state.trim().starts_with('Z');
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn is_zombie(_pid: u32) -> bool {
    false
}

/// Read the single-letter process state from `/proc/{pid}/status`.
/// Returns `Some('S')`, `Some('D')`, `Some('Z')`, etc., or `None` if unreadable.
#[cfg(target_os = "linux")]
pub fn process_state(pid: u32) -> Option<char> {
    let contents = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in contents.lines() {
        if let Some(state) = line.strip_prefix("State:") {
            return state.trim().chars().next();
        }
    }
    None
}

/// Non-Linux stub — always returns `None`.
#[cfg(not(target_os = "linux"))]
pub fn process_state(_pid: u32) -> Option<char> {
    None
}

/// Attempt to reap a zombie process via `waitpid`.
#[cfg(unix)]
pub fn try_reap(pid: u32) {
    unsafe {
        libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG);
    }
}

#[cfg(not(unix))]
pub fn try_reap(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;
    use syfrah_core::mesh::PeerStatus;

    fn make_peer(key: &str) -> PeerRecord {
        PeerRecord {
            name: "test".into(),
            wg_public_key: key.into(),
            endpoint: "127.0.0.1:51820".parse().unwrap(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1),
            last_seen: 0,
            status: PeerStatus::Active,
            region: None,
            zone: None,
            topology: None,
            hypervisor_report: None,
        }
    }

    fn temp_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn count_and_exists_empty_db() {
        let (_dir, db) = temp_db();
        let (count, exists) = db.count_and_exists("peers", "no-such-key").unwrap();
        assert_eq!(count, 0);
        assert!(!exists);
    }

    #[test]
    fn count_and_exists_after_inserts() {
        let (_dir, db) = temp_db();
        let peer = make_peer("key-1");
        db.set("peers", &peer.wg_public_key, &peer).unwrap();

        let (count, exists) = db.count_and_exists("peers", "key-1").unwrap();
        assert_eq!(count, 1);
        assert!(exists);

        let (count, exists) = db.count_and_exists("peers", "key-2").unwrap();
        assert_eq!(count, 1);
        assert!(!exists);
    }

    #[test]
    fn peer_count_empty_db() {
        let (_dir, db) = temp_db();
        assert_eq!(db.count("peers").unwrap(), 0);
    }

    #[test]
    fn peer_count_matches_list_len() {
        let (_dir, db) = temp_db();
        for i in 0..5 {
            let peer = make_peer(&format!("key-{i}"));
            db.set("peers", &peer.wg_public_key, &peer).unwrap();
        }
        let count = db.count("peers").unwrap() as usize;
        let list: Vec<(String, PeerRecord)> = db.list("peers").unwrap();
        assert_eq!(count, list.len());
        assert_eq!(count, 5);
    }

    #[test]
    fn upsert_peer_bounded_rejects_new_at_limit() {
        let (_dir, db) = temp_db();
        // Fill to capacity (3 peers)
        for i in 0..3 {
            let peer = make_peer(&format!("key-{i}"));
            db.set("peers", &peer.wg_public_key, &peer).unwrap();
        }
        // New peer should be rejected
        let new_peer = make_peer("key-new");
        let exists = db.exists("peers", &new_peer.wg_public_key).unwrap();
        let count = db.count("peers").unwrap() as usize;
        assert!(!exists);
        assert_eq!(count, 3);
        // Simulates upsert_peer_bounded logic: new + at limit → reject
        assert!(!exists && count >= 3);
    }

    #[test]
    fn upsert_peer_bounded_allows_existing_at_limit() {
        let (_dir, db) = temp_db();
        for i in 0..3 {
            let peer = make_peer(&format!("key-{i}"));
            db.set("peers", &peer.wg_public_key, &peer).unwrap();
        }
        // Existing peer update should be allowed even at limit
        let exists = db.exists("peers", "key-1").unwrap();
        let count = db.count("peers").unwrap() as usize;
        assert!(exists);
        assert_eq!(count, 3);
        // Simulates upsert_peer_bounded logic: existing → always allowed
        assert!(exists); // would skip the count check
    }

    #[test]
    fn upsert_peer_bounded_allows_new_under_limit() {
        let (_dir, db) = temp_db();
        db.set("peers", "key-0", &make_peer("key-0")).unwrap();
        let new_peer = make_peer("key-new");
        let exists = db.exists("peers", &new_peer.wg_public_key).unwrap();
        let count = db.count("peers").unwrap() as usize;
        assert!(!exists);
        assert_eq!(count, 1);
        // Under limit of 3 → allowed
        assert!(count < 3);
    }

    #[test]
    fn save_and_load_roundtrip() {
        // Use a temp dir to avoid polluting ~/.syfrah
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("state.json");
        let state = NodeState {
            mesh_name: "test".into(),
            mesh_secret: "syf_sk_test".into(),
            wg_private_key: "priv".into(),
            wg_public_key: "pub".into(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1),
            mesh_prefix: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 0),
            wg_listen_port: 51820,
            node_name: "node-1".into(),
            public_endpoint: None,
            peering_port: 51821,
            peers: vec![],
            region: Some("us-east".into()),
            zone: Some("zone-1".into()),
            metrics: Default::default(),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&file, &json).unwrap();
        let loaded: NodeState =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(loaded.mesh_name, "test");
        assert_eq!(loaded.node_name, "node-1");
        assert_eq!(loaded.mesh_ipv6, state.mesh_ipv6);
    }

    // ── purge_stale_peers_by_name tests ──

    #[test]
    fn purge_stale_peers_marks_old_keys_as_removed() {
        let (_dir, db) = temp_db();

        // Simulate 3 peer records with the same node name but different keys
        // (from repeated init/join cycles).
        let mut p1 = make_peer("old-key-1");
        p1.name = "node-a".into();
        let mut p2 = make_peer("old-key-2");
        p2.name = "node-a".into();
        let mut p3 = make_peer("current-key");
        p3.name = "node-a".into();
        // A different node that should not be purged
        let mut p4 = make_peer("other-node-key");
        p4.name = "node-b".into();

        db.set("peers", &p1.wg_public_key, &p1).unwrap();
        db.set("peers", &p2.wg_public_key, &p2).unwrap();
        db.set("peers", &p3.wg_public_key, &p3).unwrap();
        db.set("peers", &p4.wg_public_key, &p4).unwrap();

        // Purge stale peers for node-a, keeping current-key
        let entries: Vec<(String, PeerRecord)> = db.list("peers").unwrap();
        let mut purged = 0;
        for (key, mut peer) in entries {
            if peer.name == "node-a"
                && peer.wg_public_key != "current-key"
                && peer.status != PeerStatus::Removed
            {
                peer.status = PeerStatus::Removed;
                db.set("peers", &key, &peer).unwrap();
                purged += 1;
            }
        }

        assert_eq!(purged, 2);

        // Verify: old keys are Removed, current key is Active, other node untouched
        let p1_loaded: PeerRecord = db.get("peers", "old-key-1").unwrap().unwrap();
        assert_eq!(p1_loaded.status, PeerStatus::Removed);
        let p2_loaded: PeerRecord = db.get("peers", "old-key-2").unwrap().unwrap();
        assert_eq!(p2_loaded.status, PeerStatus::Removed);
        let p3_loaded: PeerRecord = db.get("peers", "current-key").unwrap().unwrap();
        assert_eq!(p3_loaded.status, PeerStatus::Active);
        let p4_loaded: PeerRecord = db.get("peers", "other-node-key").unwrap().unwrap();
        assert_eq!(p4_loaded.status, PeerStatus::Active);
    }

    #[test]
    fn purge_stale_peers_no_stale_entries() {
        let (_dir, db) = temp_db();

        let mut p1 = make_peer("key-1");
        p1.name = "node-a".into();
        db.set("peers", &p1.wg_public_key, &p1).unwrap();

        // Purge with the only key — nothing should be purged
        let entries: Vec<(String, PeerRecord)> = db.list("peers").unwrap();
        let purged = entries
            .into_iter()
            .filter(|(_, p)| {
                p.name == "node-a" && p.wg_public_key != "key-1" && p.status != PeerStatus::Removed
            })
            .count();
        assert_eq!(purged, 0);
    }

    #[test]
    fn peer_index_upsert_and_lookup() {
        // Test the in-memory index helpers directly
        let peer = make_peer("idx-key-1");
        index_upsert(&peer);
        let found = peer_by_key("idx-key-1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().wg_public_key, "idx-key-1");

        // Missing key returns None
        assert!(peer_by_key("no-such-key").is_none());

        // Update via index_update changes the record
        let mut updated = make_peer("idx-key-1");
        updated.status = PeerStatus::Removed;
        index_update("idx-key-1", &updated);
        let found = peer_by_key("idx-key-1").unwrap();
        assert_eq!(found.status, PeerStatus::Removed);

        // Invalidate clears the index
        invalidate_peer_index();
    }

    #[test]
    fn purge_stale_peers_skips_already_removed() {
        let (_dir, db) = temp_db();

        let mut p1 = make_peer("old-key");
        p1.name = "node-a".into();
        p1.status = PeerStatus::Removed; // already removed
        let mut p2 = make_peer("current-key");
        p2.name = "node-a".into();

        db.set("peers", &p1.wg_public_key, &p1).unwrap();
        db.set("peers", &p2.wg_public_key, &p2).unwrap();

        let entries: Vec<(String, PeerRecord)> = db.list("peers").unwrap();
        let purged = entries
            .into_iter()
            .filter(|(_, p)| {
                p.name == "node-a"
                    && p.wg_public_key != "current-key"
                    && p.status != PeerStatus::Removed
            })
            .count();
        assert_eq!(purged, 0, "already-removed peers should not be counted");
    }
}
