//! GC worker — two-phase garbage collection for unreachable SSTs, WAL
//! segments, and orphaned generation prefixes.
//!
//! ## Design
//!
//! The GC worker runs only on the Raft leader (single-writer guarantee).
//! It periodically scans `pending_gc_ssts` from the state machine and
//! deletes the corresponding S3 objects in two phases:
//!
//! 1. **Mark**: SSTs are added to `pending_gc_ssts` when their refcount
//!    reaches zero (done by the state machine in `DeleteSnapshot` /
//!    `DeleteVolume`). This is monotonic — once marked unreachable, an
//!    SST stays unreachable.
//!
//! 2. **Wait + Delete**: The GC worker waits for a configurable grace
//!    period (default 1 hour) before deleting from S3. This ensures
//!    that any in-flight operations (reads, restores) have time to
//!    complete. After successful S3 deletion, a `GcCompleteSsts`
//!    command is submitted through Raft to remove the keys from the
//!    pending list.
//!
//! Additionally, the worker handles:
//! - **WAL segment cleanup**: Deletes WAL segments below `min_wal_position`.
//! - **Orphaned generation cleanup**: Removes objects under stale `gen-N/`
//!   prefixes that no longer match any volume's `placement_generation`.
//!
//! ## Safety invariants
//!
//! - GC runs only on the leader. If leadership is lost mid-cycle, the
//!   incomplete pass is harmless — the next leader will retry.
//! - Never deletes during in-flight operations: the grace period and
//!   `restores_in_progress` guard protect against this.
//! - GC is monotonic: once an SST is in `pending_gc_ssts`, it can never
//!   become reachable again (refcounts only increment on snapshot create,
//!   and the SST was already removed from `sst_refcounts`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::storage_cleanup::{s3_auth_header, S3CleanupConfig, S3CleanupError};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// GC worker configuration.
#[derive(Debug, Clone)]
pub struct GcConfig {
    /// How often the GC worker wakes up to scan for work.
    pub scan_interval: Duration,
    /// Grace period after an SST is marked unreachable before it is
    /// deleted from S3. Default: 1 hour.
    pub grace_period: Duration,
    /// Maximum number of SST keys to delete per GC cycle. Prevents
    /// overwhelming S3 with deletes during large cleanups.
    pub max_deletes_per_cycle: usize,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            scan_interval: Duration::from_secs(300),
            grace_period: Duration::from_secs(3600),
            max_deletes_per_cycle: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// State reader trait (abstracts Raft state access)
// ---------------------------------------------------------------------------

/// Provides read access to the state needed by the GC worker.
///
/// Implemented by the controlplane integration to read from the Raft
/// state machine without the GC worker needing a direct dependency on
/// the Raft node.
pub trait GcStateReader: Send + Sync {
    /// Returns true if this node is the current Raft leader.
    fn is_leader(&self) -> bool;

    /// Returns the list of SST keys pending garbage collection.
    fn pending_gc_ssts(&self) -> Vec<String>;

    /// Returns the minimum WAL position across all non-deleted snapshots.
    /// `None` means there are no snapshots, so all WAL segments can be
    /// cleaned up.
    fn min_wal_position(&self) -> Option<u64>;

    /// Returns snapshot IDs with in-progress restores. GC must not delete
    /// SSTs that might be read by an ongoing restore.
    fn restores_in_progress(&self) -> Vec<String>;

    /// Returns the current generation for each volume, keyed by volume ID.
    /// Used to identify orphaned generation prefixes.
    fn volume_generations(&self) -> HashMap<String, u64>;
}

/// Submits GC completion commands through Raft.
///
/// The GC worker calls this after successfully deleting objects from S3
/// to update the replicated state.
#[async_trait::async_trait]
pub trait GcCommandSubmitter: Send + Sync {
    /// Submit a `GcCompleteSsts` command through Raft.
    async fn gc_complete_ssts(&self, sst_keys: Vec<String>) -> Result<(), String>;

    /// Submit a `GcCompleteWalSegments` command through Raft.
    async fn gc_complete_wal_segments(&self, below_position: u64) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// GC worker
// ---------------------------------------------------------------------------

/// Tracks when each SST and orphaned generation was first seen, so we
/// can enforce grace periods before deletion.
struct GcWorkerState {
    /// Maps SST key -> instant when it was first observed in pending_gc_ssts.
    first_seen: HashMap<String, Instant>,
    /// Maps "volume_id/gen-N" -> instant when the orphaned generation was
    /// first observed. Used to enforce a grace period before deleting
    /// orphaned generation objects.
    orphaned_gen_first_seen: HashMap<String, Instant>,
}

impl GcWorkerState {
    fn new() -> Self {
        Self {
            first_seen: HashMap::new(),
            orphaned_gen_first_seen: HashMap::new(),
        }
    }

    /// Update tracking: add newly seen keys, remove keys no longer pending.
    fn update_tracking(&mut self, pending: &[String]) {
        let now = Instant::now();

        // Add new keys.
        for key in pending {
            self.first_seen.entry(key.clone()).or_insert(now);
        }

        // Remove keys that are no longer in the pending list (already GC'd
        // by another leader, or removed for some other reason).
        self.first_seen.retain(|k, _| pending.contains(k));
    }

    /// Returns SST keys that have been pending longer than the grace period.
    fn keys_past_grace_period(&self, grace_period: Duration) -> Vec<String> {
        let now = Instant::now();
        self.first_seen
            .iter()
            .filter(|(_, first_seen)| now.duration_since(**first_seen) >= grace_period)
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// Delete a single S3 object by key.
///
/// Reuses the auth infrastructure from `storage_cleanup`.
async fn delete_s3_object(config: &S3CleanupConfig, key: &str) -> Result<(), S3CleanupError> {
    let client = reqwest::Client::new();
    let delete_url = format!("{}/{}/{}", config.endpoint, config.bucket, key);

    let resp = client
        .delete(&delete_url)
        .header(
            "Authorization",
            s3_auth_header(config, "DELETE", &config.bucket, key),
        )
        .send()
        .await
        .map_err(|e| S3CleanupError::DeleteFailed {
            key: key.to_string(),
            source: e,
        })?;

    if resp.status().is_success() || resp.status().as_u16() == 204 {
        Ok(())
    } else {
        Err(S3CleanupError::S3Error {
            status: resp.status().as_u16(),
            operation: "DELETE".into(),
            key: key.to_string(),
        })
    }
}

/// List S3 objects under a prefix.
///
/// TODO: Handle pagination via continuation tokens. Currently limited to
/// 1000 keys per request — objects beyond that are silently skipped and
/// will be picked up on subsequent GC cycles.
async fn list_s3_objects(
    config: &S3CleanupConfig,
    prefix: &str,
) -> Result<Vec<String>, S3CleanupError> {
    let client = reqwest::Client::new();
    let list_url = format!(
        "{}/{}?list-type=2&prefix={}&max-keys=1000",
        config.endpoint, config.bucket, prefix,
    );

    let resp = client
        .get(&list_url)
        .header(
            "Authorization",
            s3_auth_header(config, "GET", &config.bucket, ""),
        )
        .send()
        .await
        .map_err(|e| S3CleanupError::ListFailed {
            prefix: prefix.to_string(),
            source: e,
        })?;

    if !resp.status().is_success() {
        return Err(S3CleanupError::S3Error {
            status: resp.status().as_u16(),
            operation: "ListObjectsV2".into(),
            key: prefix.to_string(),
        });
    }

    let body = resp.text().await.map_err(|e| S3CleanupError::ListFailed {
        prefix: prefix.to_string(),
        source: e,
    })?;

    Ok(crate::storage_cleanup::parse_s3_list_keys(&body))
}

/// Run a single GC cycle for SST files.
///
/// Returns the list of SST keys that were successfully deleted.
async fn gc_ssts_cycle(
    config: &S3CleanupConfig,
    state: &GcWorkerState,
    gc_config: &GcConfig,
    restores_in_progress: &[String],
) -> Vec<String> {
    // Never delete SSTs while restores are in progress — the restore
    // process may be reading SSTs that are marked for GC.
    if !restores_in_progress.is_empty() {
        debug!(
            restores = restores_in_progress.len(),
            "GC: skipping SST deletion — restores in progress"
        );
        return Vec::new();
    }

    let mut eligible = state.keys_past_grace_period(gc_config.grace_period);
    eligible.truncate(gc_config.max_deletes_per_cycle);

    if eligible.is_empty() {
        return Vec::new();
    }

    info!(count = eligible.len(), "GC: deleting SST objects from S3");

    let mut deleted = Vec::new();
    for key in &eligible {
        match delete_s3_object(config, key).await {
            Ok(()) => {
                debug!(key, "GC: SST deleted from S3");
                deleted.push(key.clone());
            }
            Err(e) => {
                warn!(key, error = %e, "GC: failed to delete SST from S3");
                // Continue with remaining keys — partial progress is fine.
            }
        }
    }

    deleted
}

/// Run a single GC cycle for WAL segments below `min_wal_position`.
///
/// WAL segments are stored under `wal/` prefix with names like
/// `wal/segment-{position}`. We list objects under the prefix and
/// delete any with a position strictly below `min_wal_position`.
async fn gc_wal_cycle(config: &S3CleanupConfig, min_wal_position: Option<u64>) -> Option<u64> {
    let min_pos = match min_wal_position {
        Some(pos) if pos > 0 => pos,
        _ => return None,
    };

    let keys = match list_s3_objects(config, "wal/segment-").await {
        Ok(keys) => keys,
        Err(e) => {
            warn!(error = %e, "GC: failed to list WAL segments");
            return None;
        }
    };

    if keys.is_empty() {
        return None;
    }

    let mut highest_deleted: Option<u64> = None;

    for key in &keys {
        // Parse position from key name: "wal/segment-{position}"
        let position = match key.strip_prefix("wal/segment-") {
            Some(pos_str) => match pos_str.parse::<u64>() {
                Ok(p) => p,
                Err(_) => continue,
            },
            None => continue,
        };

        if position >= min_pos {
            continue;
        }

        match delete_s3_object(config, key).await {
            Ok(()) => {
                debug!(key, position, "GC: WAL segment deleted");
                highest_deleted = Some(highest_deleted.map_or(position, |h: u64| h.max(position)));
            }
            Err(e) => {
                warn!(key, error = %e, "GC: failed to delete WAL segment");
            }
        }
    }

    highest_deleted
}

/// Run a single GC cycle for orphaned generation prefixes.
///
/// Volumes store data under `volumes/{id}/gen-{N}/`. When a volume's
/// placement_generation advances (e.g., after migration), old `gen-N/`
/// prefixes become orphaned and their objects can be deleted.
///
/// Orphaned generations are subject to the same grace period as SSTs to
/// protect in-flight reads or compactions targeting the old generation.
/// Volumes with active restores are skipped entirely.
async fn gc_orphaned_generations(
    config: &S3CleanupConfig,
    volume_generations: &HashMap<String, u64>,
    restores_in_progress: &[String],
    grace_period: Duration,
    worker_state: &mut GcWorkerState,
) {
    let now = Instant::now();

    for (volume_id, current_gen) in volume_generations {
        // Skip volumes with in-progress restores — the restore may be
        // reading from the old generation.
        if restores_in_progress
            .iter()
            .any(|r| r.contains(volume_id.as_str()))
        {
            debug!(
                volume_id,
                "GC: skipping orphaned generation cleanup — restore in progress"
            );
            continue;
        }

        // List all objects under the volume prefix.
        let prefix = format!("volumes/{volume_id}/gen-");
        let keys = match list_s3_objects(config, &prefix).await {
            Ok(keys) => keys,
            Err(e) => {
                warn!(
                    volume_id,
                    error = %e,
                    "GC: failed to list generation prefixes"
                );
                continue;
            }
        };

        for key in &keys {
            // Parse generation from key: "volumes/{id}/gen-{N}/..."
            let after_prefix = match key.strip_prefix(&format!("volumes/{volume_id}/gen-")) {
                Some(rest) => rest,
                None => continue,
            };
            let gen_str = match after_prefix.split('/').next() {
                Some(g) => g,
                None => continue,
            };
            let gen: u64 = match gen_str.parse() {
                Ok(g) => g,
                Err(_) => continue,
            };

            if gen >= *current_gen {
                continue; // Current or future generation — keep.
            }

            // Enforce grace period: track when we first saw this orphaned
            // generation and only delete after the grace period has elapsed.
            let tracking_key = format!("{volume_id}/gen-{gen}");
            let first_seen = worker_state
                .orphaned_gen_first_seen
                .entry(tracking_key.clone())
                .or_insert(now);
            if now.duration_since(*first_seen) < grace_period {
                debug!(
                    volume_id,
                    gen, tracking_key, "GC: orphaned generation within grace period, skipping"
                );
                continue;
            }

            match delete_s3_object(config, key).await {
                Ok(()) => {
                    debug!(
                        volume_id,
                        key, gen, "GC: orphaned generation object deleted"
                    );
                }
                Err(e) => {
                    warn!(
                        volume_id,
                        key,
                        error = %e,
                        "GC: failed to delete orphaned generation object"
                    );
                }
            }
        }
    }

    // Prune tracking entries for generations that are no longer orphaned
    // (e.g., volume was deleted or generation rolled back).
    worker_state.orphaned_gen_first_seen.retain(|key, _| {
        // key format: "{volume_id}/gen-{N}"
        if let Some((vid, gen_part)) = key.split_once("/gen-") {
            if let Ok(gen) = gen_part.parse::<u64>() {
                if let Some(current) = volume_generations.get(vid) {
                    return gen < *current;
                }
            }
        }
        false
    });
}

/// Run the GC worker loop.
///
/// This should be spawned as a background task. It runs continuously
/// until the shutdown signal is received.
pub async fn run_gc_loop(
    gc_config: GcConfig,
    s3_config: S3CleanupConfig,
    state_reader: Arc<dyn GcStateReader>,
    submitter: Arc<dyn GcCommandSubmitter>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    info!(
        scan_interval_secs = gc_config.scan_interval.as_secs(),
        grace_period_secs = gc_config.grace_period.as_secs(),
        max_deletes = gc_config.max_deletes_per_cycle,
        "GC worker started"
    );

    let mut worker_state = GcWorkerState::new();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(gc_config.scan_interval) => {
                // Only the leader runs GC.
                if !state_reader.is_leader() {
                    debug!("GC: not leader, skipping cycle");
                    // Reset tracking when not leader — grace period restarts
                    // when we become leader again.
                    worker_state = GcWorkerState::new();
                    continue;
                }

                debug!("GC: starting cycle");

                // Phase 1: SST garbage collection.
                let pending = state_reader.pending_gc_ssts();
                worker_state.update_tracking(&pending);

                if !pending.is_empty() {
                    let restores = state_reader.restores_in_progress();
                    let deleted = gc_ssts_cycle(
                        &s3_config,
                        &worker_state,
                        &gc_config,
                        &restores,
                    )
                    .await;

                    if !deleted.is_empty() {
                        info!(count = deleted.len(), "GC: submitting SST completion to Raft");
                        if let Err(e) = submitter.gc_complete_ssts(deleted).await {
                            error!(error = %e, "GC: failed to submit GcCompleteSsts");
                        }
                    }
                }

                // Phase 2: WAL segment cleanup.
                let min_wal = state_reader.min_wal_position();
                if let Some(highest_deleted) = gc_wal_cycle(&s3_config, min_wal).await {
                    info!(
                        below = highest_deleted + 1,
                        "GC: submitting WAL segment completion to Raft"
                    );
                    if let Err(e) = submitter
                        .gc_complete_wal_segments(highest_deleted + 1)
                        .await
                    {
                        error!(error = %e, "GC: failed to submit GcCompleteWalSegments");
                    }
                }

                // Phase 3: Orphaned generation cleanup.
                let volume_gens = state_reader.volume_generations();
                if !volume_gens.is_empty() {
                    let restores = state_reader.restores_in_progress();
                    gc_orphaned_generations(
                        &s3_config,
                        &volume_gens,
                        &restores,
                        gc_config.grace_period,
                        &mut worker_state,
                    )
                    .await;
                }

                debug!("GC: cycle complete");
            }
            result = shutdown_rx.changed() => {
                if result.is_err() || *shutdown_rx.borrow() {
                    info!("GC worker shutting down");
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// No-op implementations for testing
// ---------------------------------------------------------------------------

/// A state reader that always reports not-leader. Useful for tests and
/// nodes that should never run GC.
pub struct NoOpGcStateReader;

impl GcStateReader for NoOpGcStateReader {
    fn is_leader(&self) -> bool {
        false
    }
    fn pending_gc_ssts(&self) -> Vec<String> {
        Vec::new()
    }
    fn min_wal_position(&self) -> Option<u64> {
        None
    }
    fn restores_in_progress(&self) -> Vec<String> {
        Vec::new()
    }
    fn volume_generations(&self) -> HashMap<String, u64> {
        HashMap::new()
    }
}

/// A command submitter that does nothing. Used when GC state changes
/// do not need to be replicated (testing only).
pub struct NoOpGcSubmitter;

#[async_trait::async_trait]
impl GcCommandSubmitter for NoOpGcSubmitter {
    async fn gc_complete_ssts(&self, _sst_keys: Vec<String>) -> Result<(), String> {
        Ok(())
    }
    async fn gc_complete_wal_segments(&self, _below_position: u64) -> Result<(), String> {
        Ok(())
    }
}

// Make parse_s3_list_keys accessible from storage_cleanup (it's pub(crate)).
// We re-use it via `crate::storage_cleanup::parse_s3_list_keys`.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test state reader with configurable behavior.
    struct TestStateReader {
        leader: bool,
        pending: Vec<String>,
        min_wal: Option<u64>,
        restores: Vec<String>,
        generations: HashMap<String, u64>,
    }

    impl GcStateReader for TestStateReader {
        fn is_leader(&self) -> bool {
            self.leader
        }
        fn pending_gc_ssts(&self) -> Vec<String> {
            self.pending.clone()
        }
        fn min_wal_position(&self) -> Option<u64> {
            self.min_wal
        }
        fn restores_in_progress(&self) -> Vec<String> {
            self.restores.clone()
        }
        fn volume_generations(&self) -> HashMap<String, u64> {
            self.generations.clone()
        }
    }

    /// Test submitter that records calls.
    struct TestSubmitter {
        sst_calls: Mutex<Vec<Vec<String>>>,
        wal_calls: Mutex<Vec<u64>>,
    }

    impl TestSubmitter {
        fn new() -> Self {
            Self {
                sst_calls: Mutex::new(Vec::new()),
                wal_calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl GcCommandSubmitter for TestSubmitter {
        async fn gc_complete_ssts(&self, sst_keys: Vec<String>) -> Result<(), String> {
            self.sst_calls.lock().unwrap().push(sst_keys);
            Ok(())
        }
        async fn gc_complete_wal_segments(&self, below_position: u64) -> Result<(), String> {
            self.wal_calls.lock().unwrap().push(below_position);
            Ok(())
        }
    }

    #[test]
    fn gc_worker_state_tracking() {
        let mut state = GcWorkerState::new();

        // First observation.
        state.update_tracking(&["sst-1".into(), "sst-2".into()]);
        assert_eq!(state.first_seen.len(), 2);

        // Same keys — timestamps should not change.
        let t1 = *state.first_seen.get("sst-1").unwrap();
        state.update_tracking(&["sst-1".into(), "sst-2".into()]);
        assert_eq!(*state.first_seen.get("sst-1").unwrap(), t1);

        // Remove sst-2 from pending — should be pruned.
        state.update_tracking(&["sst-1".into()]);
        assert_eq!(state.first_seen.len(), 1);
        assert!(state.first_seen.contains_key("sst-1"));
    }

    #[test]
    fn grace_period_filtering() {
        let mut state = GcWorkerState::new();

        // Insert with a timestamp in the past.
        state
            .first_seen
            .insert("old-sst".into(), Instant::now() - Duration::from_secs(7200));
        state.first_seen.insert("new-sst".into(), Instant::now());

        let eligible = state.keys_past_grace_period(Duration::from_secs(3600));
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0], "old-sst");
    }

    #[test]
    fn gc_config_defaults() {
        let config = GcConfig::default();
        assert_eq!(config.scan_interval, Duration::from_secs(300));
        assert_eq!(config.grace_period, Duration::from_secs(3600));
        assert_eq!(config.max_deletes_per_cycle, 100);
    }

    #[tokio::test]
    async fn gc_loop_shuts_down_immediately() {
        let (tx, rx) = watch::channel(false);
        let config = GcConfig {
            scan_interval: Duration::from_secs(3600),
            ..Default::default()
        };
        let s3 = S3CleanupConfig {
            endpoint: "http://localhost:9000".into(),
            bucket: "test".into(),
            access_key: "key".into(),
            secret_key: "secret".into(),
        };

        let handle = tokio::spawn(async move {
            run_gc_loop(
                config,
                s3,
                Arc::new(NoOpGcStateReader),
                Arc::new(NoOpGcSubmitter),
                rx,
            )
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn gc_skips_when_not_leader() {
        let reader = Arc::new(TestStateReader {
            leader: false,
            pending: vec!["sst-1".into()],
            min_wal: None,
            restores: vec![],
            generations: HashMap::new(),
        });
        let submitter = Arc::new(TestSubmitter::new());

        let (tx, rx) = watch::channel(false);
        let config = GcConfig {
            scan_interval: Duration::from_millis(10),
            grace_period: Duration::ZERO,
            max_deletes_per_cycle: 100,
        };
        let s3 = S3CleanupConfig {
            endpoint: "http://localhost:9000".into(),
            bucket: "test".into(),
            access_key: "key".into(),
            secret_key: "secret".into(),
        };

        let sub = Arc::clone(&submitter);
        let handle = tokio::spawn(async move {
            run_gc_loop(config, s3, reader, sub, rx).await;
        });

        // Let a few cycles run.
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        handle.await.unwrap();

        // No SSTs should have been submitted (not leader).
        assert!(submitter.sst_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn no_op_state_reader_returns_defaults() {
        let reader = NoOpGcStateReader;
        assert!(!reader.is_leader());
        assert!(reader.pending_gc_ssts().is_empty());
        assert_eq!(reader.min_wal_position(), None);
        assert!(reader.restores_in_progress().is_empty());
        assert!(reader.volume_generations().is_empty());
    }

    #[tokio::test]
    async fn no_op_submitter_succeeds() {
        let sub = NoOpGcSubmitter;
        assert!(sub.gc_complete_ssts(vec!["sst-1".into()]).await.is_ok());
        assert!(sub.gc_complete_wal_segments(42).await.is_ok());
    }

    #[tokio::test]
    async fn gc_ssts_cycle_skips_when_restores_in_progress() {
        let mut state = GcWorkerState::new();
        state
            .first_seen
            .insert("sst-1".into(), Instant::now() - Duration::from_secs(7200));

        let config = GcConfig {
            grace_period: Duration::from_secs(3600),
            ..Default::default()
        };
        let s3 = S3CleanupConfig {
            endpoint: "http://localhost:9000".into(),
            bucket: "test".into(),
            access_key: "key".into(),
            secret_key: "secret".into(),
        };

        // With restores in progress, no deletions should happen.
        let deleted = gc_ssts_cycle(&s3, &state, &config, &["snap-restoring".to_string()]).await;
        assert!(deleted.is_empty());
    }
}
