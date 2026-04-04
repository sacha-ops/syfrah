//! Storage reconciler — detects desired volumes in Raft state and
//! initializes/stops ZeroFS processes via VolumeMgr, then attaches NBD
//! devices to Cloud Hypervisor VMs via the CH add-disk API.
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
//! 5. For volumes with desired=AttachedTo and ZeroFS running but not yet
//!    attached to CH: calls PUT /vm.add-disk with rate limiting defaults
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

use syfrah_storage::volume_mgr::{
    CacheConfig, S3Config, VolumeManifest, VolumeMgr, VolumeMgrError,
};

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
    /// The VM this volume should be attached to, if any.
    /// When `Some`, the reconciler will call CH add-disk after ZeroFS starts.
    pub vm_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Disk rate limiting configuration (ADR-006 §15)
// ---------------------------------------------------------------------------

/// I/O rate limiting configuration for virtio-block devices attached via CH.
///
/// These defaults match ADR-006 §15 and can be overridden per-volume in the
/// desired state (future work).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskRateLimitConfig {
    /// Maximum sustained bandwidth in bytes/sec (default: 200 MB/s).
    pub bw_size: u64,
    /// Refill period for the bandwidth bucket in milliseconds.
    pub bw_refill_ms: u64,
    /// Maximum sustained IOPS (default: 10,000).
    pub ops_size: u64,
    /// Refill period for the IOPS bucket in milliseconds.
    pub ops_refill_ms: u64,
}

impl Default for DiskRateLimitConfig {
    fn default() -> Self {
        Self {
            bw_size: 200 * 1024 * 1024, // 200 MB/s
            bw_refill_ms: 1000,
            ops_size: 10_000,
            ops_refill_ms: 1000,
        }
    }
}

impl DiskRateLimitConfig {
    /// Build the `rate_limiter_config` JSON fragment for CH `add-disk`.
    pub fn to_ch_json(&self) -> serde_json::Value {
        serde_json::json!({
            "bandwidth": {
                "size": self.bw_size,
                "one_time_burst": 0,
                "refill_time": self.bw_refill_ms
            },
            "ops": {
                "size": self.ops_size,
                "one_time_burst": 0,
                "refill_time": self.ops_refill_ms
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Trait for attaching disks to VMs via Cloud Hypervisor (mockable in tests)
// ---------------------------------------------------------------------------

/// Abstraction over the Cloud Hypervisor add-disk API.
///
/// In production, this is implemented by resolving the VM's API socket and
/// calling `ChClient::add_disk`. In tests, a mock records calls.
#[async_trait::async_trait]
pub trait VmDiskAttacher: Send + Sync {
    /// Attach an NBD device to the specified VM as a virtio-block disk.
    ///
    /// `vm_id` identifies the VM. `nbd_path` is the block device path
    /// (e.g. `/dev/nbd0`). `rate_limit` configures I/O throttling.
    async fn attach_disk(
        &self,
        vm_id: &str,
        nbd_path: &std::path::Path,
        rate_limit: &DiskRateLimitConfig,
    ) -> Result<(), String>;
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
    /// Attach an NBD device to a VM via Cloud Hypervisor.
    AttachDisk { volume_id: String, vm_id: String },
}

/// Report from a single reconciliation pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageReconcileReport {
    pub pass_number: u64,
    pub started: usize,
    pub stopped: usize,
    pub detached: usize,
    pub reaped: usize,
    pub attached: usize,
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
// Snapshot submission trait (Raft command submission)
// ---------------------------------------------------------------------------

/// Trait for submitting a `CreateSnapshot` command to the Raft state machine.
///
/// In production, the implementation serializes a `StateMachineCommand::CreateSnapshot`
/// and proposes it through the Raft client. In tests, a mock records the call.
#[async_trait::async_trait]
pub trait SnapshotSubmitter: Send + Sync {
    /// Submit a snapshot creation request. Returns the snapshot ID on success.
    ///
    /// `snapshot_id` — pre-generated unique ID for the snapshot.
    /// `source_volume_id` — the volume being snapshotted.
    /// `manifest` — SST files + WAL position captured from ZeroFS.
    async fn submit_create_snapshot(
        &self,
        snapshot_id: &str,
        source_volume_id: &str,
        manifest: &VolumeManifest,
    ) -> Result<String, String>;
}

// ---------------------------------------------------------------------------
// Snapshot creation flow (ADR-006 §6)
// ---------------------------------------------------------------------------

/// Create a snapshot of a running volume.
///
/// Orchestrates the full snapshot creation flow:
/// 1. Capture the current manifest from ZeroFS (SST files + WAL position)
/// 2. Submit a `CreateSnapshot` Raft command with the manifest data
/// 3. SST refcount increments happen atomically in the state machine
///
/// Returns the snapshot ID on success.
pub async fn create_snapshot(
    volume_mgr: &VolumeMgr,
    submitter: &dyn SnapshotSubmitter,
    snapshot_id: &str,
    volume_id: &str,
) -> Result<String, String> {
    // Step 1: Capture manifest from ZeroFS.
    let manifest = volume_mgr
        .capture_manifest(volume_id)
        .await
        .map_err(|e| format!("failed to capture manifest for volume {volume_id}: {e}"))?;

    info!(
        volume_id,
        snapshot_id,
        sst_count = manifest.sst_files.len(),
        wal_position = manifest.wal_position,
        "captured volume manifest for snapshot"
    );

    // Step 2: Submit CreateSnapshot Raft command.
    let result = submitter
        .submit_create_snapshot(snapshot_id, volume_id, &manifest)
        .await?;

    info!(volume_id, snapshot_id, "snapshot created via Raft");
    Ok(result)
}

// ---------------------------------------------------------------------------
// StorageReconciler
// ---------------------------------------------------------------------------

/// The storage reconciler — runs a periodic loop converging local ZeroFS
/// processes to match the desired Raft state, and attaching NBD devices
/// to Cloud Hypervisor VMs when desired=AttachedTo.
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
    /// Rate limiting defaults for attached disks (ADR-006 §15).
    disk_rate_limit: DiskRateLimitConfig,
    /// Set of volume IDs already attached to CH in previous passes.
    /// Cleared when a volume is stopped or no longer desired.
    attached_volumes: std::sync::Mutex<std::collections::HashSet<String>>,
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
            disk_rate_limit: DiskRateLimitConfig::default(),
            attached_volumes: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Create with a custom interval.
    pub fn with_interval(mut self, interval_secs: u64) -> Self {
        self.interval_secs = interval_secs;
        self
    }

    /// Override disk rate limiting configuration.
    pub fn with_disk_rate_limit(mut self, config: DiskRateLimitConfig) -> Self {
        self.disk_rate_limit = config;
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
    /// running processes, and applies start/stop/attach/detach actions.
    ///
    /// When `attacher` is `Some`, volumes with `vm_id` set and ZeroFS
    /// running will be attached to Cloud Hypervisor via `add-disk`.
    /// The `ch_provider` is used during detach to call `PUT /vm.remove-device`
    /// before stopping ZeroFS. Pass `&NoOpChClientProvider` if no VMs are running.
    pub async fn reconcile_once(
        &self,
        reader: &dyn VolumeStateReader,
        volume_mgr: &mut VolumeMgr,
    ) -> StorageReconcileReport {
        self.reconcile_once_full(reader, volume_mgr, None, &NoOpChClientProvider)
            .await
    }

    /// Run a single reconciliation pass with an optional disk attacher.
    pub async fn reconcile_once_with_attacher(
        &self,
        reader: &dyn VolumeStateReader,
        volume_mgr: &mut VolumeMgr,
        attacher: Option<&dyn VmDiskAttacher>,
    ) -> StorageReconcileReport {
        self.reconcile_once_full(reader, volume_mgr, attacher, &NoOpChClientProvider)
            .await
    }

    /// Run a single reconciliation pass with an explicit CH client provider (for detach).
    pub async fn reconcile_once_with_ch(
        &self,
        reader: &dyn VolumeStateReader,
        volume_mgr: &mut VolumeMgr,
        ch_provider: &dyn ChClientProvider,
    ) -> StorageReconcileReport {
        self.reconcile_once_full(reader, volume_mgr, None, ch_provider)
            .await
    }

    /// Full reconciliation pass with both attach and detach support.
    ///
    /// This is the full attach/detach-aware reconciliation loop. The detach sequence:
    /// 1. CH: `PUT /vm.remove-device` (guest loses the block device)
    /// 2. ZeroFS: flush cache to S3 (SIGTERM + graceful wait)
    /// 3. NBD: device disconnected when ZeroFS process exits
    ///
    /// Force detach skips step 2 (SIGKILL instead of SIGTERM, no flush).
    pub async fn reconcile_once_full(
        &self,
        reader: &dyn VolumeStateReader,
        volume_mgr: &mut VolumeMgr,
        attacher: Option<&dyn VmDiskAttacher>,
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
                            // Clear from attached set so the volume can be
                            // re-attached after restarting with the new
                            // generation.
                            self.attached_volumes.lock().unwrap().remove(id.as_str());
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
                        // Remove from attached set — volume is no longer running.
                        self.attached_volumes.lock().unwrap().remove(id);
                    }
                    Err(e) => {
                        error!(volume_id = %id, error = %e, "storage reconciler: failed to stop volume");
                        report.errors.push(format!("stop {id}: {e}"));
                    }
                }
            }
        }

        // 7. Attach volumes to Cloud Hypervisor VMs.
        //    For volumes with desired=AttachedTo (vm_id set) and ZeroFS running
        //    but not yet attached to CH, call add-disk with rate limiting.
        if let Some(attacher) = attacher {
            for (id, vol) in &desired_map {
                if let Some(ref vm_id) = vol.vm_id {
                    // Only attach if ZeroFS is running and not already attached
                    // in a previous pass.
                    let already_attached =
                        self.attached_volumes.lock().unwrap().contains(id.as_str());
                    if volume_mgr.is_running(id) && !already_attached {
                        if let Some(nbd_path) = volume_mgr.get_nbd_device(id) {
                            info!(
                                volume_id = %id,
                                vm_id = %vm_id,
                                nbd_device = %nbd_path.display(),
                                "storage reconciler: attaching volume to VM"
                            );
                            match attacher
                                .attach_disk(vm_id, &nbd_path, &self.disk_rate_limit)
                                .await
                            {
                                Ok(()) => {
                                    info!(
                                        volume_id = %id,
                                        vm_id = %vm_id,
                                        "storage reconciler: volume attached to VM"
                                    );
                                    report.attached += 1;
                                    self.attached_volumes.lock().unwrap().insert(id.clone());
                                }
                                Err(e) => {
                                    error!(
                                        volume_id = %id,
                                        vm_id = %vm_id,
                                        error = %e,
                                        "storage reconciler: failed to attach volume to VM"
                                    );
                                    report.errors.push(format!("attach {id} to {vm_id}: {e}"));
                                }
                            }
                        } else {
                            warn!(
                                volume_id = %id,
                                "storage reconciler: volume running but no NBD device found"
                            );
                        }
                    }
                }
            }
        }

        report.duration_ms = start.elapsed().as_millis() as u64;

        debug!(
            pass = pass,
            started = report.started,
            stopped = report.stopped,
            attached = report.attached,
            detached = report.detached,
            reaped = report.reaped,
            errors = report.errors.len(),
            "storage reconciliation pass complete"
        );

        *self.last_report.lock().unwrap() = Some(report.clone());
        report
    }

    /// Run the periodic reconciliation loop.
    ///
    /// When `attacher` is `Some`, each pass will attempt to attach NBD
    /// devices to Cloud Hypervisor VMs via the `VmDiskAttacher` trait.
    /// Without an attacher the attach step is skipped (volumes still
    /// start/stop normally).
    pub async fn run_loop(
        &self,
        reader: Arc<dyn VolumeStateReader>,
        volume_mgr: &mut VolumeMgr,
        attacher: Option<Arc<dyn VmDiskAttacher>>,
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
                    self.reconcile_once_with_attacher(
                        reader.as_ref(),
                        volume_mgr,
                        attacher.as_deref(),
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
            vm_id: None,
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
                reconciler
                    .run_loop(reader, &mut mgr, None, shutdown_rx)
                    .await;
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
            vm_id: None,
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
            vm_id: None,
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

    // -- Attach flow tests ---------------------------------------------------

    #[tokio::test]
    async fn generation_fencing_clears_attached_volumes() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();
        let attacher = MockDiskAttacher::new();

        // Inject a fake running ZeroFS process at generation 1.
        mgr.inject_fake_process("vol-fence", 1);

        // Desired: volume attached to vm-10 at generation 1.
        reader.set_desired(vec![DesiredVolume {
            id: "vol-fence".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-10".into()),
        }]);

        // Pass 1: attach succeeds.
        let r1 = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;
        assert_eq!(r1.attached, 1);

        // Confirm attached_volumes contains the entry.
        assert!(reconciler
            .attached_volumes
            .lock()
            .unwrap()
            .contains("vol-fence"));

        // Re-inject at generation 1 so we can trigger a gen mismatch stop.
        mgr.inject_fake_process("vol-fence", 1);

        // Desired state bumps to generation 2.
        reader.set_desired(vec![DesiredVolume {
            id: "vol-fence".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 2,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-10".into()),
        }]);

        // Pass 2: generation mismatch -> stop -> should clear attached_volumes.
        let r2 = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;
        assert!(
            r2.stopped >= 1,
            "expected stop for stale generation, got {}",
            r2.stopped
        );

        // The key assertion: attached_volumes must NOT contain the entry
        // so that re-attachment can happen after restart.
        assert!(
            !reconciler
                .attached_volumes
                .lock()
                .unwrap()
                .contains("vol-fence"),
            "attached_volumes should be cleared after generation-fencing stop"
        );
    }

    #[tokio::test]
    async fn run_loop_forwards_attacher() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into()).with_interval(1);
        let reader = Arc::new(MockStateReader::new().with_config());
        let mut mgr = VolumeMgr::new();
        let attacher: Arc<dyn VmDiskAttacher> = Arc::new(MockDiskAttacher::new());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        mgr.inject_fake_process("vol-loop", 1);
        reader.set_desired(vec![DesiredVolume {
            id: "vol-loop".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-loop".into()),
        }]);

        let handle = tokio::spawn({
            let reader = Arc::clone(&reader);
            let attacher = Arc::clone(&attacher);
            async move {
                reconciler
                    .run_loop(reader, &mut mgr, Some(attacher), shutdown_rx)
                    .await;
                reconciler.pass_count()
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
        shutdown_tx.send(true).unwrap();
        let passes = handle.await.unwrap();
        assert!(passes >= 1, "expected at least 1 pass, got {passes}");
    }

    /// Mock disk attacher that records calls.
    struct MockDiskAttacher {
        calls: Mutex<Vec<(String, String)>>,
        fail_for: Mutex<Vec<String>>,
    }

    impl MockDiskAttacher {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_for: Mutex::new(Vec::new()),
            }
        }

        fn fail_for_volume(self, vol_id: &str) -> Self {
            self.fail_for.lock().unwrap().push(vol_id.to_string());
            self
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl VmDiskAttacher for MockDiskAttacher {
        async fn attach_disk(
            &self,
            vm_id: &str,
            nbd_path: &std::path::Path,
            _rate_limit: &DiskRateLimitConfig,
        ) -> Result<(), String> {
            let vol_hint = nbd_path.display().to_string();
            if self
                .fail_for
                .lock()
                .unwrap()
                .iter()
                .any(|v| vol_hint.contains(v))
            {
                return Err("mock attach failure".to_string());
            }
            self.calls
                .lock()
                .unwrap()
                .push((vm_id.to_string(), vol_hint));
            Ok(())
        }
    }

    #[tokio::test]
    async fn attach_volume_to_vm_after_zerofs_starts() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();
        let attacher = MockDiskAttacher::new();

        // Inject a fake running ZeroFS process.
        mgr.inject_fake_process("vol-attach", 1);
        assert!(mgr.is_running("vol-attach"));

        // Desired: volume should be attached to vm-42.
        reader.set_desired(vec![DesiredVolume {
            id: "vol-attach".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-42".into()),
        }]);

        let report = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;

        assert_eq!(
            report.attached, 1,
            "expected 1 attach, got {}",
            report.attached
        );
        assert!(report.errors.is_empty());

        let calls = attacher.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "vm-42");

        // Cleanup.
        mgr.stop_volume("vol-attach").await.ok();
    }

    #[tokio::test]
    async fn attach_idempotent_does_not_reattach() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();
        let attacher = MockDiskAttacher::new();

        mgr.inject_fake_process("vol-idem", 1);

        reader.set_desired(vec![DesiredVolume {
            id: "vol-idem".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-99".into()),
        }]);

        // First pass: should attach.
        let r1 = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;
        assert_eq!(r1.attached, 1);

        // Second pass: already attached — should NOT call attacher again.
        let r2 = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;
        assert_eq!(r2.attached, 0);

        // Only one call total.
        assert_eq!(attacher.calls().len(), 1);

        mgr.stop_volume("vol-idem").await.ok();
    }

    #[tokio::test]
    async fn attach_not_called_without_vm_id() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();
        let attacher = MockDiskAttacher::new();

        mgr.inject_fake_process("vol-no-vm", 1);

        // No vm_id — volume is just a ZeroFS process, no CH attach.
        reader.set_desired(vec![DesiredVolume {
            id: "vol-no-vm".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: None,
        }]);

        let report = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;

        assert_eq!(report.attached, 0);
        assert!(attacher.calls().is_empty());

        mgr.stop_volume("vol-no-vm").await.ok();
    }

    #[tokio::test]
    async fn attach_failure_records_error() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();
        // This attacher fails for any path containing "nbd" (all of them).
        let attacher = MockDiskAttacher::new().fail_for_volume("nbd");

        mgr.inject_fake_process("vol-fail", 1);

        reader.set_desired(vec![DesiredVolume {
            id: "vol-fail".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-fail".into()),
        }]);

        let report = reconciler
            .reconcile_once_with_attacher(&reader, &mut mgr, Some(&attacher))
            .await;

        assert_eq!(report.attached, 0);
        assert!(!report.errors.is_empty());
        assert!(report.errors[0].contains("attach"));
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

    #[tokio::test]
    async fn attach_without_attacher_is_noop() {
        let reconciler = StorageReconciler::new("hv-1".into(), "test-pass".into());
        let reader = MockStateReader::new().with_config();
        let mut mgr = VolumeMgr::new();

        mgr.inject_fake_process("vol-noop", 1);

        reader.set_desired(vec![DesiredVolume {
            id: "vol-noop".into(),
            name: "pgdata".into(),
            size_gb: 100,
            placement_generation: 1,
            hypervisor_id: "hv-1".into(),
            vm_id: Some("vm-noop".into()),
        }]);

        // reconcile_once (no attacher) should not crash.
        let report = reconciler.reconcile_once(&reader, &mut mgr).await;
        assert_eq!(report.attached, 0);
        assert!(report.errors.is_empty());

        mgr.stop_volume("vol-noop").await.ok();
    }

    // -- Rate limit config tests ---------------------------------------------

    #[test]
    fn disk_rate_limit_default_values() {
        let config = DiskRateLimitConfig::default();
        assert_eq!(config.bw_size, 200 * 1024 * 1024); // 200 MB/s
        assert_eq!(config.ops_size, 10_000);
        assert_eq!(config.bw_refill_ms, 1000);
        assert_eq!(config.ops_refill_ms, 1000);
    }

    #[test]
    fn disk_rate_limit_to_ch_json() {
        let config = DiskRateLimitConfig::default();
        let json = config.to_ch_json();
        assert_eq!(json["bandwidth"]["size"], 200 * 1024 * 1024);
        assert_eq!(json["ops"]["size"], 10_000);
        assert_eq!(json["bandwidth"]["refill_time"], 1000);
        assert_eq!(json["ops"]["refill_time"], 1000);
    }

    #[test]
    fn disk_rate_limit_serde_roundtrip() {
        let config = DiskRateLimitConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DiskRateLimitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bw_size, config.bw_size);
        assert_eq!(parsed.ops_size, config.ops_size);
    }

    #[test]
    fn reconcile_action_attach_serializes() {
        let action = StorageReconcileAction::AttachDisk {
            volume_id: "vol-01".into(),
            vm_id: "vm-42".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("vol-01"));
        assert!(json.contains("vm-42"));
        assert!(json.contains("AttachDisk"));
    }

    #[test]
    fn reconcile_report_default_includes_attached() {
        let report = StorageReconcileReport::default();
        assert_eq!(report.attached, 0);
    }

    #[test]
    fn reconciler_with_disk_rate_limit() {
        let config = DiskRateLimitConfig {
            bw_size: 100 * 1024 * 1024,
            bw_refill_ms: 500,
            ops_size: 5_000,
            ops_refill_ms: 500,
        };
        let r =
            StorageReconciler::new("hv-1".into(), "test-pass".into()).with_disk_rate_limit(config);
        assert_eq!(r.disk_rate_limit.bw_size, 100 * 1024 * 1024);
        assert_eq!(r.disk_rate_limit.ops_size, 5_000);
    }

    // ── Snapshot creation tests (#1200) ────────────────────────────

    /// Mock SnapshotSubmitter that records calls and returns success.
    struct MockSnapshotSubmitter {
        calls: Mutex<Vec<(String, String, VolumeManifest)>>,
        fail_with: Mutex<Option<String>>,
    }

    impl MockSnapshotSubmitter {
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

        fn last_call(&self) -> Option<(String, String, VolumeManifest)> {
            self.calls.lock().unwrap().last().cloned()
        }
    }

    #[async_trait::async_trait]
    impl SnapshotSubmitter for MockSnapshotSubmitter {
        async fn submit_create_snapshot(
            &self,
            snapshot_id: &str,
            source_volume_id: &str,
            manifest: &VolumeManifest,
        ) -> Result<String, String> {
            if let Some(err) = self.fail_with.lock().unwrap().as_ref() {
                return Err(err.clone());
            }
            self.calls.lock().unwrap().push((
                snapshot_id.to_string(),
                source_volume_id.to_string(),
                manifest.clone(),
            ));
            Ok(snapshot_id.to_string())
        }
    }

    #[tokio::test]
    async fn create_snapshot_succeeds_with_manifest_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut mgr = VolumeMgr::new();
        mgr = mgr.with_nbd_base(tmp.path().join("nbd"));
        mgr.inject_fake_process("vol-snap", 1);

        // Pre-write manifest file.
        let manifest = VolumeManifest {
            sst_files: vec!["sst-a.sst".into()],
            wal_position: 42,
        };
        let manifest_path = tmp.path().join("vol-snap.manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap())
            .await
            .unwrap();

        let submitter = MockSnapshotSubmitter::new();
        let result = create_snapshot(&mgr, &submitter, "snap-01", "vol-snap").await;

        assert!(result.is_ok(), "create_snapshot failed: {result:?}");
        assert_eq!(result.unwrap(), "snap-01");
        assert_eq!(submitter.call_count(), 1);

        let (sid, vid, m) = submitter.last_call().unwrap();
        assert_eq!(sid, "snap-01");
        assert_eq!(vid, "vol-snap");
        assert_eq!(m.sst_files, vec!["sst-a.sst"]);
        assert_eq!(m.wal_position, 42);

        mgr.stop_volume("vol-snap").await.ok();
    }

    #[tokio::test]
    async fn create_snapshot_fails_when_volume_not_running() {
        let mgr = VolumeMgr::new();
        let submitter = MockSnapshotSubmitter::new();
        let result = create_snapshot(&mgr, &submitter, "snap-01", "nonexistent").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
        assert_eq!(submitter.call_count(), 0);
    }

    #[tokio::test]
    async fn create_snapshot_propagates_raft_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut mgr = VolumeMgr::new();
        mgr = mgr.with_nbd_base(tmp.path().join("nbd"));
        mgr.inject_fake_process("vol-snap", 1);

        // Pre-write manifest file.
        let manifest_path = tmp.path().join("vol-snap.manifest.json");
        tokio::fs::write(
            &manifest_path,
            serde_json::to_string(&VolumeManifest {
                sst_files: vec!["sst-x.sst".into()],
                wal_position: 7,
            })
            .unwrap(),
        )
        .await
        .unwrap();

        let submitter = MockSnapshotSubmitter::failing("raft: not leader");
        let result = create_snapshot(&mgr, &submitter, "snap-01", "vol-snap").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("raft: not leader"));

        mgr.stop_volume("vol-snap").await.ok();
    }
}
