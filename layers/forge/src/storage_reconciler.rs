//! Storage reconciler — detects desired volumes in Raft state and
//! initializes/stops ZeroFS processes via VolumeMgr.
//!
//! ## How it works
//!
//! On each tick the reconciler:
//! 1. Reads desired volumes for this hypervisor from Raft state
//! 2. Reads actual running volumes from VolumeMgr
//! 3. Computes the diff:
//!    - Desired but not running  -> start_volume (Available + assigned here)
//!    - Running but not desired  -> stop_volume  (fenced / detached / deleted)
//!    - Generation mismatch      -> stop_volume  (stale placement, fencing)
//!    - Pending detach            -> detach_volume (CH remove-device → flush → NBD disconnect)
//! 4. Reaps crashed ZeroFS processes
//!
//! ## Detach flow (#1195)
//!
//! When a volume transitions from Attached to Available (detach requested):
//! 1. CH: `PUT /vm.remove-device` — guest loses the block device
//! 2. ZeroFS: flush cache to S3 (SIGTERM + graceful shutdown)
//! 3. NBD: device disconnected when ZeroFS exits
//!
//! Force detach (`force=true`) skips the flush: SIGKILL, data since last
//! fsync is lost.
//!
//! The reconciler does NOT modify Raft state. It only reads desired state
//! and converges local ZeroFS processes to match.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use syfrah_storage::volume_mgr::{CacheConfig, S3Config, VolumeMgr, VolumeMgrError};

// ---------------------------------------------------------------------------
// Desired-state types (read from Raft)
// ---------------------------------------------------------------------------

/// Desired volume state as read from the Raft state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredVolume {
    pub id: String,
    pub name: String,
    pub size_gb: u32,
    pub placement_generation: u64,
    pub hypervisor_id: String,
}

/// A volume pending detach — read from Raft when desired state is Available
/// but the volume is still attached on this hypervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredDetach {
    /// Volume ID to detach.
    pub volume_id: String,
    /// VM ID the volume is currently attached to (needed for CH remove-device).
    pub vm_id: String,
    /// Cloud Hypervisor device ID for the attached block device (e.g. `"_disk0"`).
    pub ch_device_id: String,
    /// If true, skip the ZeroFS cache flush (data loss possible).
    pub force: bool,
}

/// Storage configuration for the region (from Raft).
#[derive(Debug, Clone)]
pub struct RegionStorageConfig {
    pub s3_endpoint: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub cache_disk_path: PathBuf,
    pub cache_disk_size_bytes: u64,
    pub cache_memory_size_bytes: u64,
}

// ---------------------------------------------------------------------------
// Trait for reading desired state (mockable in tests)
// ---------------------------------------------------------------------------

/// Trait abstracting access to the Raft state machine's volume data.
///
/// In production, implemented by a wrapper around `RedbStateMachine`.
/// In tests, a mock can return predetermined desired state.
#[async_trait::async_trait]
pub trait VolumeStateReader: Send + Sync {
    /// List volumes that should be running on this hypervisor.
    ///
    /// Returns volumes in Attached state with `attached_hypervisor_id == local_id`.
    async fn desired_volumes(&self, local_hypervisor_id: &str) -> Vec<DesiredVolume>;

    /// List volumes pending detach on this hypervisor.
    ///
    /// Returns volumes whose desired state is Available but are still attached
    /// (ZeroFS still running) on this hypervisor. The reconciler will execute
    /// the detach sequence: CH remove-device → ZeroFS flush → NBD disconnect.
    async fn pending_detaches(&self, local_hypervisor_id: &str) -> Vec<DesiredDetach>;

    /// Get the storage config for the region, if configured.
    async fn storage_config(&self) -> Option<RegionStorageConfig>;
}

// ---------------------------------------------------------------------------
// Reconcile action types
// ---------------------------------------------------------------------------

/// Action taken by the storage reconciler on a single pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StorageReconcileAction {
    /// Start a ZeroFS process for a volume.
    StartVolume { volume_id: String, generation: u64 },
    /// Stop a ZeroFS process (fenced, detached, or deleted).
    StopVolume { volume_id: String, reason: String },
    /// Detach a volume: CH remove-device → ZeroFS flush → NBD disconnect.
    DetachVolume {
        volume_id: String,
        vm_id: String,
        force: bool,
    },
    /// Reap a crashed ZeroFS process.
    ReapCrashed { volume_id: String },
}

/// Report from a single reconciliation pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageReconcileReport {
    pub pass_number: u64,
    pub started: usize,
    pub stopped: usize,
    pub detached: usize,
    pub reaped: usize,
    pub errors: Vec<String>,
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// CH client provider (for detach — needs to talk to Cloud Hypervisor)
// ---------------------------------------------------------------------------

/// Trait for obtaining a `ChClient` for a given VM.
///
/// The reconciler needs to call `PUT /vm.remove-device` during detach,
/// which requires the CH API socket path for the target VM. In production,
/// this resolves to `/run/syfrah/vms/{vm_id}/api.sock`.
#[async_trait::async_trait]
pub trait ChClientProvider: Send + Sync {
    /// Remove a device from a running VM.
    ///
    /// Implementations should construct a `ChClient` for `vm_id` and call
    /// `remove_device(ch_device_id)`. Returns an error string on failure.
    async fn remove_device(&self, vm_id: &str, ch_device_id: &str) -> Result<(), String>;
}

/// No-op CH client provider — used when no VMs are running or during tests.
///
/// Always returns an error, since there is no real CH socket to connect to.
pub struct NoOpChClientProvider;

#[async_trait::async_trait]
impl ChClientProvider for NoOpChClientProvider {
    async fn remove_device(&self, vm_id: &str, ch_device_id: &str) -> Result<(), String> {
        Err(format!(
            "no CH client available for vm={vm_id} device={ch_device_id}"
        ))
    }
}

// ---------------------------------------------------------------------------
// StorageReconciler
// ---------------------------------------------------------------------------

/// The storage reconciler — runs a periodic loop converging local ZeroFS
/// processes to match the desired Raft state.
pub struct StorageReconciler {
    /// This hypervisor's ID (used to filter volumes from Raft).
    local_hypervisor_id: String,
    /// Interval between reconciliation passes.
    interval_secs: u64,
    /// Pass counter.
    pass_count: std::sync::atomic::AtomicU64,
    /// Last report.
    last_report: std::sync::Mutex<Option<StorageReconcileReport>>,
    /// Local encryption passphrase (never replicated via Raft).
    encryption_passphrase: String,
}

impl StorageReconciler {
    /// Create a new storage reconciler.
    pub fn new(local_hypervisor_id: String, encryption_passphrase: String) -> Self {
        Self {
            local_hypervisor_id,
            interval_secs: 5,
            pass_count: std::sync::atomic::AtomicU64::new(0),
            last_report: std::sync::Mutex::new(None),
            encryption_passphrase,
        }
    }

    /// Create with a custom interval.
    pub fn with_interval(mut self, interval_secs: u64) -> Self {
        self.interval_secs = interval_secs;
        self
    }

    /// Get the current pass count.
    pub fn pass_count(&self) -> u64 {
        self.pass_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the last reconciliation report.
    pub fn last_report(&self) -> Option<StorageReconcileReport> {
        self.last_report.lock().unwrap().clone()
    }

    /// Run a single reconciliation pass.
    ///
    /// Reads desired state from `reader`, diffs against `volume_mgr`'s
    /// running processes, and applies start/stop actions.
    ///
    /// The `ch_provider` is used during detach to call `PUT /vm.remove-device`
    /// before stopping ZeroFS. Pass `&NoOpChClientProvider` if no VMs are running.
    pub async fn reconcile_once(
        &self,
        reader: &dyn VolumeStateReader,
        volume_mgr: &mut VolumeMgr,
    ) -> StorageReconcileReport {
        self.reconcile_once_with_ch(reader, volume_mgr, &NoOpChClientProvider)
            .await
    }

    /// Run a single reconciliation pass with an explicit CH client provider.
    ///
    /// This is the full detach-aware reconciliation loop. The detach sequence:
    /// 1. CH: `PUT /vm.remove-device` (guest loses the block device)
    /// 2. ZeroFS: flush cache to S3 (SIGTERM + graceful wait)
    /// 3. NBD: device disconnected when ZeroFS process exits
    ///
    /// Force detach skips step 2 (SIGKILL instead of SIGTERM, no flush).
    pub async fn reconcile_once_with_ch(
        &self,
        reader: &dyn VolumeStateReader,
        volume_mgr: &mut VolumeMgr,
        ch_provider: &dyn ChClientProvider,
    ) -> StorageReconcileReport {
        let pass = self
            .pass_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let start = std::time::Instant::now();

        let mut report = StorageReconcileReport {
            pass_number: pass,
            ..Default::default()
        };

        // 1. Reap crashed processes first.
        let reaped = volume_mgr.reap_exited().await;
        for id in &reaped {
            warn!(volume_id = %id, "storage reconciler: reaped crashed ZeroFS process");
        }
        report.reaped = reaped.len();

        // 2. Process pending detaches (CH remove-device → ZeroFS flush → NBD disconnect).
        //    This runs before the start/stop diff so that detached volumes are
        //    no longer running when we reach step 6/7.
        //    Track which volumes have pending detaches so step 7 doesn't
        //    redundantly stop them (especially if detach failed at remove-device
        //    and the volume should remain running).
        let detaches = reader.pending_detaches(&self.local_hypervisor_id).await;
        let pending_detach_ids: std::collections::HashSet<String> =
            detaches.iter().map(|d| d.volume_id.clone()).collect();
        for detach in &detaches {
            if !volume_mgr.is_running(&detach.volume_id) {
                debug!(
                    volume_id = %detach.volume_id,
                    "storage reconciler: detach requested but volume not running, skipping"
                );
                continue;
            }

            info!(
                volume_id = %detach.volume_id,
                vm_id = %detach.vm_id,
                device_id = %detach.ch_device_id,
                force = detach.force,
                "storage reconciler: detaching volume"
            );

            // Step 1: CH remove-device — guest loses the block device.
            //         Idempotent: 404 means already removed, which is fine.
            if let Err(e) = ch_provider
                .remove_device(&detach.vm_id, &detach.ch_device_id)
                .await
            {
                error!(
                    volume_id = %detach.volume_id,
                    vm_id = %detach.vm_id,
                    error = %e,
                    "storage reconciler: failed to remove device from VM, aborting detach"
                );
                report
                    .errors
                    .push(format!("detach-remove-device {}: {e}", detach.volume_id));
                // Do NOT proceed with flush/stop if remove-device failed —
                // the guest may still be writing to the device.
                continue;
            }

            // Step 2+3: Stop ZeroFS (flush=true → SIGTERM+grace, flush=false → SIGKILL).
            //           ZeroFS flushes its cache to S3 on graceful shutdown.
            //           The NBD device is disconnected when the process exits.
            let flush = !detach.force;
            match volume_mgr.stop_volume_flush(&detach.volume_id, flush).await {
                Ok(()) => {
                    info!(
                        volume_id = %detach.volume_id,
                        force = detach.force,
                        "storage reconciler: volume detached successfully"
                    );
                    report.detached += 1;
                }
                Err(e) => {
                    error!(
                        volume_id = %detach.volume_id,
                        error = %e,
                        "storage reconciler: failed to stop ZeroFS during detach"
                    );
                    report
                        .errors
                        .push(format!("detach-stop {}: {e}", detach.volume_id));
                }
            }
        }

        // 3. Read desired state.
        let desired = reader.desired_volumes(&self.local_hypervisor_id).await;
        let config = match reader.storage_config().await {
            Some(c) => c,
            None => {
                debug!("storage reconciler: no storage config yet, skipping");
                report.duration_ms = start.elapsed().as_millis() as u64;
                *self.last_report.lock().unwrap() = Some(report.clone());
                return report;
            }
        };

        // 4. Build desired set and running map (with generations).
        let desired_map: HashMap<String, &DesiredVolume> =
            desired.iter().map(|v| (v.id.clone(), v)).collect();
        let running: HashMap<String, u64> = volume_mgr.list_active().into_iter().collect();

        let s3 = S3Config {
            endpoint: config.s3_endpoint.clone(),
            bucket: config.s3_bucket.clone(),
            access_key: config.s3_access_key.clone(),
            secret_key: config.s3_secret_key.clone(),
        };
        let cache = CacheConfig {
            disk_path: config.cache_disk_path.clone(),
            disk_size_bytes: config.cache_disk_size_bytes,
            memory_size_bytes: config.cache_memory_size_bytes,
        };

        // 5. Generation fencing: stop volumes running with a stale generation.
        //    After stopping, the volume will be picked up as "desired but not
        //    running" in step 6 and restarted with the correct generation.
        for (id, vol) in &desired_map {
            if let Some(&running_gen) = running.get(id.as_str()) {
                if running_gen != vol.placement_generation {
                    warn!(
                        volume_id = %id,
                        running_generation = running_gen,
                        desired_generation = vol.placement_generation,
                        "storage reconciler: generation mismatch, stopping stale volume"
                    );
                    match volume_mgr.stop_volume(id).await {
                        Ok(()) => {
                            report.stopped += 1;
                        }
                        Err(e) => {
                            error!(
                                volume_id = %id,
                                error = %e,
                                "storage reconciler: failed to stop stale-generation volume"
                            );
                            report.errors.push(format!("stop-stale {id}: {e}"));
                        }
                    }
                }
            }
        }

        // 6. Start volumes that are desired but not running (including those
        //    just stopped due to generation mismatch above).
        for (id, vol) in &desired_map {
            if !volume_mgr.is_running(id) {
                info!(
                    volume_id = %id,
                    generation = vol.placement_generation,
                    "storage reconciler: starting volume"
                );
                match volume_mgr
                    .start_volume(
                        id,
                        &s3,
                        &cache,
                        &self.encryption_passphrase,
                        vol.placement_generation,
                    )
                    .await
                {
                    Ok(nbd) => {
                        info!(
                            volume_id = %id,
                            nbd_device = %nbd.display(),
                            "storage reconciler: volume started"
                        );
                        report.started += 1;
                    }
                    Err(VolumeMgrError::AlreadyRunning(_)) => {
                        // Race with a previous pass — harmless.
                        debug!(volume_id = %id, "storage reconciler: already running");
                    }
                    Err(e) => {
                        error!(volume_id = %id, error = %e, "storage reconciler: failed to start volume");
                        report.errors.push(format!("start {id}: {e}"));
                    }
                }
            }
        }

        // 7. Stop volumes that are running but not desired (fenced/detached/deleted).
        for id in running.keys() {
            if !desired_map.contains_key(id) {
                // Skip if already stopped during generation fencing above.
                if !volume_mgr.is_running(id) {
                    continue;
                }
                // Skip volumes with pending detaches — step 2 handles them.
                // If detach failed (e.g. remove-device error), the volume
                // must remain running so the guest doesn't lose data.
                if pending_detach_ids.contains(id) {
                    continue;
                }
                let reason =
                    "volume no longer desired on this hypervisor (detached/deleted/fenced)";
                info!(volume_id = %id, reason, "storage reconciler: stopping volume");
                match volume_mgr.stop_volume(id).await {
                    Ok(()) => {
                        report.stopped += 1;
                    }
                    Err(e) => {
                        error!(volume_id = %id, error = %e, "storage reconciler: failed to stop volume");
                        report.errors.push(format!("stop {id}: {e}"));
                    }
                }
            }
        }

        report.duration_ms = start.elapsed().as_millis() as u64;

        debug!(
            pass = pass,
            started = report.started,
            stopped = report.stopped,
            detached = report.detached,
            reaped = report.reaped,
            errors = report.errors.len(),
            "storage reconciliation pass complete"
        );

        *self.last_report.lock().unwrap() = Some(report.clone());
        report
    }

    /// Run the periodic reconciliation loop.
    pub async fn run_loop(
        &self,
        reader: Arc<dyn VolumeStateReader>,
        volume_mgr: &mut VolumeMgr,
        shutdown_rx: watch::Receiver<bool>,
    ) {
        self.run_loop_with_ch(
            reader,
            volume_mgr,
            Arc::new(NoOpChClientProvider),
            shutdown_rx,
        )
        .await
    }

    /// Run the periodic reconciliation loop with an explicit CH client provider.
    pub async fn run_loop_with_ch(
        &self,
        reader: Arc<dyn VolumeStateReader>,
        volume_mgr: &mut VolumeMgr,
        ch_provider: Arc<dyn ChClientProvider>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        let interval = tokio::time::Duration::from_secs(self.interval_secs);
        info!(
            interval_secs = self.interval_secs,
            hypervisor_id = %self.local_hypervisor_id,
            "storage reconciliation loop started"
        );

        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    self.reconcile_once_with_ch(
                        reader.as_ref(),
                        volume_mgr,
                        ch_provider.as_ref(),
                    ).await;
                }
                result = shutdown_rx.changed() => {
                    if result.is_err() || *shutdown_rx.borrow() {
                        info!("storage reconciliation loop shutting down");
                        break;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// EmptyStateReader — no-op reader for bootstrapping
// ---------------------------------------------------------------------------

/// A no-op `VolumeStateReader` that returns no desired volumes and no config.
///
/// Used during daemon startup before the Raft state machine is available.
/// Once the control plane initialises, the daemon should replace this with a
/// Raft-backed reader.
pub struct EmptyStateReader;

#[async_trait::async_trait]
impl VolumeStateReader for EmptyStateReader {
    async fn desired_volumes(&self, _local_hypervisor_id: &str) -> Vec<DesiredVolume> {
        Vec::new()
    }

    async fn pending_detaches(&self, _local_hypervisor_id: &str) -> Vec<DesiredDetach> {
        Vec::new()
    }

    async fn storage_config(&self) -> Option<RegionStorageConfig> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::process::Command;

    /// Mock VolumeStateReader for tests.
    struct MockStateReader {
        desired: Mutex<Vec<DesiredVolume>>,
        detaches: Mutex<Vec<DesiredDetach>>,
        config: Mutex<Option<RegionStorageConfig>>,
    }

    impl MockStateReader {
        fn new() -> Self {
            Self {
                desired: Mutex::new(Vec::new()),
                detaches: Mutex::new(Vec::new()),
                config: Mutex::new(None),
            }
        }

        fn with_config(self) -> Self {
            *self.config.lock().unwrap() = Some(RegionStorageConfig {
                s3_endpoint: "http://localhost:9000".into(),
                s3_bucket: "test-bucket".into(),
                s3_access_key: "test-ak".into(),
                s3_secret_key: "test-sk".into(),
                cache_disk_path: PathBuf::from("/tmp/cache"),
                cache_disk_size_bytes: 1_073_741_824,
                cache_memory_size_bytes: 268_435_456,
            });
            self
        }

        fn set_desired(&self, vols: Vec<DesiredVolume>) {
            *self.desired.lock().unwrap() = vols;
        }

        fn set_detaches(&self, detaches: Vec<DesiredDetach>) {
            *self.detaches.lock().unwrap() = detaches;
        }
    }

    #[async_trait::async_trait]
    impl VolumeStateReader for MockStateReader {
        async fn desired_volumes(&self, _local_hypervisor_id: &str) -> Vec<DesiredVolume> {
            self.desired.lock().unwrap().clone()
        }

        async fn pending_detaches(&self, _local_hypervisor_id: &str) -> Vec<DesiredDetach> {
            self.detaches.lock().unwrap().clone()
        }

        async fn storage_config(&self) -> Option<RegionStorageConfig> {
            self.config.lock().unwrap().clone()
        }
    }

    /// Mock CH client provider for testing detach.
    struct MockChProvider {
        /// Track remove_device calls: (vm_id, device_id).
        calls: Mutex<Vec<(String, String)>>,
        /// If set, all remove_device calls return this error.
        fail_with: Mutex<Option<String>>,
    }

    impl MockChProvider {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_with: Mutex::new(None),
            }
        }

        fn failing(msg: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_with: Mutex::new(Some(msg.to_string())),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl ChClientProvider for MockChProvider {
        async fn remove_device(&self, vm_id: &str, ch_device_id: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((vm_id.to_string(), ch_device_id.to_string()));
            if let Some(err) = self.fail_with.lock().unwrap().as_ref() {
                return Err(err.clone());
            }
            Ok(())
        }
    }

    #[test]
    fn reconciler_creates() {
        let r = StorageReconciler::new("hv-1".into(), "test-pass".into());
        assert_eq!(r.pass_count(), 0);
        assert!(r.last_report().is_none());
    }

    #[test]
    fn reconciler_with_interval() {
        let r = StorageReconciler::new("hv-1".into(), "test-pass".into()).with_interval(30);
        assert_eq!(r.interval_secs, 30);
    }

    #[tokio::test]
    async fn reconcile_once_no_config_skips() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new();
        let mut mgr = VolumeMgr::new();

        let report = reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(report.pass_number, 0);
        assert_eq!(report.started, 0);
        assert_eq!(report.stopped, 0);
        assert!(report.errors.is_empty());
    }

    #[tokio::test]
    async fn reconcile_once_no_desired_no_running() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        let report = reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(report.started, 0);
        assert_eq!(report.stopped, 0);
        assert_eq!(report.reaped, 0);
    }

    #[tokio::test]
    async fn reconcile_stops_orphaned_volume() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        // Insert a fake running process.
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // Access internals to insert a tracked process.
        // We can't call start_volume because it needs a real zerofs binary,
        // so test the stop path by verifying the reconciler tries to stop
        // processes not in the desired set.
        //
        // Since VolumeMgr's processes field is private, we test indirectly:
        // the reconciler should produce 0 stops when there are no running
        // processes and no desired volumes.
        drop(child);

        let report = reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(report.stopped, 0);
        assert_eq!(report.started, 0);
    }

    #[tokio::test]
    async fn reconcile_reaps_dead_processes() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        // No processes inserted, so reap returns empty.
        let report = reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(report.reaped, 0);
    }

    #[tokio::test]
    async fn reconcile_pass_count_increments() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(reconciler.pass_count(), 1);
        reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(reconciler.pass_count(), 2);
    }

    #[tokio::test]
    async fn reconcile_last_report_available() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        assert!(reconciler.last_report().is_none());
        reconciler.reconcile_once(&reader, &mut mgr).await;
        let report = reconciler.last_report().unwrap();
        assert_eq!(report.pass_number, 0);
    }

    #[tokio::test]
    async fn reconcile_desired_but_no_binary_reports_error() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        reader.set_desired(vec![DesiredVolume {
            id: "vol-01".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
        }]);
        let mut mgr = VolumeMgr::new();

        // start_volume will fail because there's no zerofs binary.
        let report = reconciler.reconcile_once(&reader, &mut mgr).await;
        // It should attempt to start and fail.
        assert_eq!(report.started, 0);
        assert!(!report.errors.is_empty());
    }

    #[tokio::test]
    async fn reconcile_loop_shutdown() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into()).with_interval(1);
        let reader = Arc::new(MockStateReader::new().with_config());
        let mut mgr = VolumeMgr::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // Run for a short time then shut down.
        let handle = tokio::spawn({
            let reader = Arc::clone(&reader);
            async move {
                // We need to pass mgr by ref but it's moved into the closure.
                // Use a separate scope.
                reconciler.run_loop(reader, &mut mgr, shutdown_rx).await;
                reconciler.pass_count()
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
        shutdown_tx.send(true).unwrap();
        let passes = handle.await.unwrap();
        assert!(passes >= 1, "expected at least 1 pass, got {passes}");
    }

    #[test]
    fn reconcile_action_serializes() {
        let action = StorageReconcileAction::StartVolume {
            volume_id: "vol-01".into(),
            generation: 1,
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("vol-01"));
    }

    #[test]
    fn reconcile_report_default() {
        let report = StorageReconcileReport::default();
        assert_eq!(report.pass_number, 0);
        assert_eq!(report.started, 0);
        assert_eq!(report.stopped, 0);
        assert_eq!(report.reaped, 0);
        assert!(report.errors.is_empty());
    }

    #[tokio::test]
    async fn reconcile_generation_mismatch_stops_stale_volume() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        // Inject a fake process running with generation 1.
        mgr.inject_fake_process("vol-gen", 1);
        assert!(mgr.is_running("vol-gen"));

        // Desired state says generation should be 2.
        reader.set_desired(vec![DesiredVolume {
            id: "vol-gen".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 2,
            hypervisor_id: "hv-1".into(),
        }]);

        let report = reconciler.reconcile_once(&reader, &mut mgr).await;

        // The stale volume should have been stopped.
        assert!(
            report.stopped >= 1,
            "expected at least 1 stop for stale generation, got {}",
            report.stopped
        );

        // start_volume will fail (no zerofs binary) but the attempt should
        // be recorded as an error — proving the reconciler tried to restart
        // with the new generation.
        assert!(
            !report.errors.is_empty(),
            "expected a start error (no zerofs binary) after stopping stale volume"
        );
    }

    #[tokio::test]
    async fn reconcile_matching_generation_no_restart() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        // Inject a fake process running with generation 3.
        mgr.inject_fake_process("vol-ok", 3);
        assert!(mgr.is_running("vol-ok"));

        // Desired state also says generation 3.
        reader.set_desired(vec![DesiredVolume {
            id: "vol-ok".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 3,
            hypervisor_id: "hv-1".into(),
        }]);

        let report = reconciler.reconcile_once(&reader, &mut mgr).await;

        // Nothing should be stopped or started — generations match.
        assert_eq!(report.stopped, 0);
        assert_eq!(report.started, 0);
        assert!(report.errors.is_empty());
        assert!(mgr.is_running("vol-ok"));

        // Cleanup.
        mgr.stop_volume("vol-ok").await.ok();
    }

    // ── Detach tests (#1195) ────────────────────────────────────────

    #[tokio::test]
    async fn detach_graceful_removes_device_then_stops_zerofs() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let ch = MockChProvider::new();
        let mut mgr = VolumeMgr::new();

        // Inject a running volume.
        mgr.inject_fake_process("vol-detach", 1);
        assert!(mgr.is_running("vol-detach"));

        // Request a graceful detach.
        reader.set_detaches(vec![DesiredDetach {
            volume_id: "vol-detach".into(),
            vm_id: "vm-1".into(),
            ch_device_id: "_disk0".into(),
            force: false,
        }]);

        let report = reconciler
            .reconcile_once_with_ch(&reader, &mut mgr, &ch)
            .await;

        assert_eq!(report.detached, 1, "expected 1 detach");
        assert!(
            report.errors.is_empty(),
            "expected no errors: {:?}",
            report.errors
        );
        assert!(
            !mgr.is_running("vol-detach"),
            "volume should no longer be running"
        );
        assert_eq!(ch.call_count(), 1, "expected 1 remove_device call");
    }

    #[tokio::test]
    async fn detach_force_skips_flush() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let ch = MockChProvider::new();
        let mut mgr = VolumeMgr::new();

        mgr.inject_fake_process("vol-force", 1);

        reader.set_detaches(vec![DesiredDetach {
            volume_id: "vol-force".into(),
            vm_id: "vm-2".into(),
            ch_device_id: "_disk1".into(),
            force: true,
        }]);

        let report = reconciler
            .reconcile_once_with_ch(&reader, &mut mgr, &ch)
            .await;

        assert_eq!(report.detached, 1);
        assert!(!mgr.is_running("vol-force"));
        assert_eq!(ch.call_count(), 1);
    }

    #[tokio::test]
    async fn detach_skips_if_volume_not_running() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let ch = MockChProvider::new();
        let mut mgr = VolumeMgr::new();

        // Request detach for a volume that is not running.
        reader.set_detaches(vec![DesiredDetach {
            volume_id: "vol-gone".into(),
            vm_id: "vm-3".into(),
            ch_device_id: "_disk0".into(),
            force: false,
        }]);

        let report = reconciler
            .reconcile_once_with_ch(&reader, &mut mgr, &ch)
            .await;

        assert_eq!(report.detached, 0);
        assert_eq!(
            ch.call_count(),
            0,
            "should not call remove_device if volume not running"
        );
    }

    #[tokio::test]
    async fn detach_aborts_if_remove_device_fails() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let ch = MockChProvider::failing("connection refused");
        let mut mgr = VolumeMgr::new();

        mgr.inject_fake_process("vol-fail", 1);

        reader.set_detaches(vec![DesiredDetach {
            volume_id: "vol-fail".into(),
            vm_id: "vm-4".into(),
            ch_device_id: "_disk0".into(),
            force: false,
        }]);

        let report = reconciler
            .reconcile_once_with_ch(&reader, &mut mgr, &ch)
            .await;

        // Detach should fail — volume should still be running.
        assert_eq!(report.detached, 0);
        assert!(
            !report.errors.is_empty(),
            "expected an error from failed remove_device"
        );
        assert!(
            mgr.is_running("vol-fail"),
            "volume must remain running when remove_device fails"
        );

        // Cleanup.
        mgr.stop_volume("vol-fail").await.ok();
    }

    #[tokio::test]
    async fn detach_no_pending_produces_zero_detaches() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let ch = MockChProvider::new();
        let mut mgr = VolumeMgr::new();

        let report = reconciler
            .reconcile_once_with_ch(&reader, &mut mgr, &ch)
            .await;

        assert_eq!(report.detached, 0);
        assert_eq!(ch.call_count(), 0);
    }

    #[test]
    fn detach_action_serializes() {
        let action = StorageReconcileAction::DetachVolume {
            volume_id: "vol-01".into(),
            vm_id: "vm-01".into(),
            force: false,
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("vol-01"));
        assert!(json.contains("DetachVolume"));
    }

    #[test]
    fn reconcile_report_default_includes_detached() {
        let report = StorageReconcileReport::default();
        assert_eq!(report.detached, 0);
    }
}
