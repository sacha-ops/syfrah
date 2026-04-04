//! Volume manager — start/stop ZeroFS processes per volume.
//!
//! `VolumeMgr` is the runtime process supervisor for volumes on a single node.
//! Each volume is backed by one ZeroFS child process that exposes a 9P socket.
//! The host mounts the 9P filesystem at `/var/lib/syfrah/volumes/{volume_id}/`,
//! which Cloud Hypervisor can share via virtio-fs or containers can bind-mount.
//!
//! ## Lifecycle
//!
//! 1. `start_volume` resolves the ZeroFS binary, spawns the process, waits
//!    for the 9P socket to appear, mounts it on the host, and tracks the PID.
//! 2. `stop_volume` unmounts the 9P filesystem, sends SIGTERM, waits up to a
//!    grace period, then SIGKILLs if the process is still running.
//! 3. Background reaping: callers should poll `reap_exited` periodically to
//!    detect crashed processes and update internal state.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::time;
use tracing::info;

use crate::binary;
use crate::cache::{CachePrewarmConfig, CachePrewarmProgress, CachePrewarmer, PrewarmHandle};

// ---------------------------------------------------------------------------
// Volume health (ADR-006 §25 — S3 outage degradation)
// ---------------------------------------------------------------------------

/// Health state of a volume's S3 backend.
///
/// Transitions are driven by S3 reachability probes:
/// - `Healthy` → `Degraded` when S3 is unreachable for longer than
///   `S3HealthConfig::degraded_after`.
/// - `Degraded` → `Error` when S3 is unreachable for longer than
///   `S3HealthConfig::error_after`.
/// - Any state → `Healthy` when S3 becomes reachable again and the
///   dirty buffer has been flushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VolumeHealth {
    /// S3 is reachable and all data is synced.
    Healthy,
    /// S3 has been unreachable for > `degraded_after` but < `error_after`.
    /// Writes are still accepted but buffered locally.
    Degraded,
    /// S3 has been unreachable for > `error_after`.
    /// New writes are rejected to prevent unbounded local buffer growth.
    Error,
}

impl std::fmt::Display for VolumeHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VolumeHealth::Healthy => write!(f, "Healthy"),
            VolumeHealth::Degraded => write!(f, "Degraded"),
            VolumeHealth::Error => write!(f, "Error"),
        }
    }
}

/// Configurable thresholds for S3 health transitions.
#[derive(Debug, Clone)]
pub struct S3HealthConfig {
    /// Duration of S3 unreachability before transitioning to `Degraded`.
    /// Default: 5 minutes.
    pub degraded_after: Duration,
    /// Duration of S3 unreachability before transitioning to `Error`.
    /// Default: 30 minutes.
    pub error_after: Duration,
    /// Maximum dirty bytes before rejecting new writes in `Degraded` state.
    /// Default: 1 GiB.
    pub max_dirty_bytes: u64,
}

impl Default for S3HealthConfig {
    fn default() -> Self {
        Self {
            degraded_after: Duration::from_secs(5 * 60),
            error_after: Duration::from_secs(30 * 60),
            max_dirty_bytes: 1_073_741_824, // 1 GiB
        }
    }
}

/// Per-volume S3 health tracker.
///
/// Tracks when S3 became unreachable for a specific volume and how
/// many dirty bytes are buffered locally. The `VolumeMgr` uses this
/// to compute the current `VolumeHealth` and to decide whether new
/// writes should be accepted.
#[derive(Debug, Clone)]
pub struct VolumeHealthTracker {
    /// Current health state.
    health: VolumeHealth,
    /// When S3 became unreachable (`None` if currently reachable).
    s3_unreachable_since: Option<Instant>,
    /// Bytes written locally but not yet flushed to S3.
    dirty_bytes: u64,
    /// Whether a flush is currently in progress (recovery).
    flush_in_progress: bool,
}

impl VolumeHealthTracker {
    fn new() -> Self {
        Self {
            health: VolumeHealth::Healthy,
            s3_unreachable_since: None,
            dirty_bytes: 0,
            flush_in_progress: false,
        }
    }

    /// Current health state.
    pub fn health(&self) -> VolumeHealth {
        self.health
    }

    /// Current dirty bytes count.
    pub fn dirty_bytes(&self) -> u64 {
        self.dirty_bytes
    }

    /// Whether a recovery flush is in progress.
    pub fn flush_in_progress(&self) -> bool {
        self.flush_in_progress
    }

    /// Record that S3 is unreachable. Call on each failed probe.
    /// The first call records the timestamp; subsequent calls are no-ops
    /// for the timestamp but still re-evaluate the health state.
    fn record_s3_unreachable(&mut self, config: &S3HealthConfig) {
        if self.s3_unreachable_since.is_none() {
            self.s3_unreachable_since = Some(Instant::now());
        }
        self.recompute_health(config);
    }

    /// Record that S3 is reachable. Resets the unreachable timer.
    /// If there are dirty bytes, starts the flush process and downgrades
    /// from `Error` to `Degraded` so that writes can resume while the
    /// backlog drains (ADR-006 §25 recovery).
    fn record_s3_reachable(&mut self) {
        self.s3_unreachable_since = None;
        if self.dirty_bytes > 0 {
            self.flush_in_progress = true;
            // Allow writes to resume while flush drains the backlog.
            // Error → Degraded ensures can_accept_write returns true.
            // Degraded stays Degraded (no change needed).
            if self.health == VolumeHealth::Error {
                self.health = VolumeHealth::Degraded;
            }
        } else {
            self.health = VolumeHealth::Healthy;
            self.flush_in_progress = false;
        }
    }

    /// Record additional dirty bytes (writes buffered locally during outage).
    fn add_dirty_bytes(&mut self, bytes: u64) {
        self.dirty_bytes = self.dirty_bytes.saturating_add(bytes);
    }

    /// Record that `bytes` have been flushed to S3 (recovery).
    fn flush_bytes(&mut self, bytes: u64) {
        self.dirty_bytes = self.dirty_bytes.saturating_sub(bytes);
        if self.dirty_bytes == 0 {
            self.flush_in_progress = false;
            if self.s3_unreachable_since.is_none() {
                self.health = VolumeHealth::Healthy;
            }
        }
    }

    /// Check whether new writes should be accepted.
    ///
    /// Writes are rejected when:
    /// - Health is `Error` (S3 unreachable > `error_after`), OR
    /// - Dirty bytes exceed `max_dirty_bytes` threshold.
    fn can_accept_write(&self, config: &S3HealthConfig) -> bool {
        if self.health == VolumeHealth::Error {
            return false;
        }
        self.dirty_bytes < config.max_dirty_bytes
    }

    /// Recompute health state based on how long S3 has been unreachable.
    fn recompute_health(&mut self, config: &S3HealthConfig) {
        let elapsed = match self.s3_unreachable_since {
            Some(since) => since.elapsed(),
            None => return, // S3 reachable — health managed by record_s3_reachable
        };

        if elapsed >= config.error_after {
            self.health = VolumeHealth::Error;
        } else if elapsed >= config.degraded_after {
            self.health = VolumeHealth::Degraded;
        }
        // Below degraded_after: health stays at whatever it was (Healthy).
    }
}

/// Summary of a volume's health state, suitable for gossip dissemination
/// and storage status reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeHealthReport {
    pub volume_id: String,
    pub health: VolumeHealth,
    pub dirty_bytes: u64,
    pub flush_in_progress: bool,
}

// ---------------------------------------------------------------------------
// Manifest types (snapshot capture)
// ---------------------------------------------------------------------------

/// Point-in-time manifest captured from a running ZeroFS process.
///
/// Contains the SST files and WAL position needed to record a
/// crash-consistent snapshot via the Raft `CreateSnapshot` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeManifest {
    /// SST file keys currently referenced by this volume's LSM tree.
    pub sst_files: Vec<String>,
    /// WAL byte offset at the time the manifest was captured.
    pub wal_position: u64,
}

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// S3 backend configuration for a single volume.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
}

/// Local cache configuration.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Path to SSD cache directory.
    pub disk_path: PathBuf,
    /// Disk cache size limit in bytes.
    pub disk_size_bytes: u64,
    /// In-memory cache limit in bytes.
    pub memory_size_bytes: u64,
}

// ---------------------------------------------------------------------------
// TOML config generation for ZeroFS
// ---------------------------------------------------------------------------

/// Generate a ZeroFS TOML configuration file for a volume.
///
/// Secrets (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `ZEROFS_PASSWORD`)
/// are referenced via `${ENV_VAR}` placeholders and passed as environment
/// variables when spawning the process.
pub fn generate_config(
    volume_id: &str,
    s3: &S3Config,
    cache: &CacheConfig,
    generation: u64,
    size_gb: f64,
) -> String {
    let cache_dir = format!("/tmp/syfrah-cache/{volume_id}");
    let disk_size_gb = cache.disk_size_bytes as f64 / 1_073_741_824.0;
    let memory_size_gb = cache.memory_size_bytes as f64 / 1_073_741_824.0;
    let s3_url = format!("s3://{}/volumes/{volume_id}/gen-{generation}/", s3.bucket);
    let ninep_socket = format!("/tmp/syfrah/{volume_id}/zerofs.9p.sock");
    let endpoint = &s3.endpoint;

    format!(
        r#"[cache]
dir = "{cache_dir}"
disk_size_gb = {disk_size_gb:.1}
memory_size_gb = {memory_size_gb:.1}

[storage]
url = "{s3_url}"
encryption_password = "${{ZEROFS_PASSWORD}}"

[filesystem]
max_size_gb = {size_gb:.1}
compression = "lz4"

[servers.ninep]
unix_socket = "{ninep_socket}"

[lsm]
wal_enabled = true

[aws]
endpoint = "{endpoint}"
access_key_id = "${{AWS_ACCESS_KEY_ID}}"
secret_access_key = "${{AWS_SECRET_ACCESS_KEY}}"
"#
    )
}

/// Tracked state for a running ZeroFS process.
struct VolumeProcess {
    child: Child,
    /// Host mount point for the 9P filesystem.
    mount_point: PathBuf,
    /// Path to the zerofs.toml config file for this volume.
    config_path: PathBuf,
    #[allow(dead_code)]
    generation: u64,
    /// S3 credentials for passing to checkpoint commands.
    s3_env: S3Env,
    /// S3 health tracker for this volume.
    health_tracker: VolumeHealthTracker,
    /// Handle for an in-flight cache pre-warming task (if any).
    prewarm_handle: Option<PrewarmHandle>,
}

/// S3 credential environment variables needed by ZeroFS checkpoint commands.
#[derive(Debug, Clone)]
struct S3Env {
    access_key: String,
    secret_key: String,
    encryption_passphrase: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by `VolumeMgr` operations.
#[derive(Debug, thiserror::Error)]
pub enum VolumeMgrError {
    #[error("volume {0} is already running")]
    AlreadyRunning(String),

    #[error("volume {0} is not running")]
    NotRunning(String),

    #[error("volume {0}: write rejected, S3 backend in Error state")]
    WriteRejected(String),

    #[error("zerofs binary: {0}")]
    Binary(#[from] binary::ZerofsError),

    #[error("spawn failed: {0}")]
    Spawn(String),

    #[error("9p socket did not appear within {0:?}")]
    SocketTimeout(Duration),

    #[error("mount failed: {0}")]
    Mount(String),

    #[error("stop failed: {0}")]
    Stop(String),

    #[error("checkpoint failed: {0}")]
    Checkpoint(String),
}

// ---------------------------------------------------------------------------
// VolumeMgr
// ---------------------------------------------------------------------------

/// Default timeout waiting for the 9P socket to appear after spawning ZeroFS.
const SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval when waiting for the 9P socket.
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Grace period before SIGKILL on stop.
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

/// Base directory for host-side 9P mount points.
const VOLUMES_BASE: &str = "/var/lib/syfrah/volumes";

/// Manages ZeroFS child processes — one per volume.
pub struct VolumeMgr {
    /// Active ZeroFS processes keyed by volume_id.
    processes: HashMap<String, VolumeProcess>,
    /// Optional explicit path to the zerofs binary.
    binary_override: Option<PathBuf>,
    /// Base directory for volume mount points. Defaults to `VOLUMES_BASE`.
    volumes_base: PathBuf,
    /// S3 health transition thresholds (ADR-006 §25).
    s3_health_config: S3HealthConfig,
    /// Cache pre-warming configuration for post-migration warmup.
    prewarm_config: CachePrewarmConfig,
}

impl VolumeMgr {
    /// Create a new `VolumeMgr`.
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            binary_override: None,
            volumes_base: PathBuf::from(VOLUMES_BASE),
            s3_health_config: S3HealthConfig::default(),
            prewarm_config: CachePrewarmConfig::default(),
        }
    }

    /// Set an explicit zerofs binary path (overrides resolution order).
    pub fn with_binary(mut self, path: PathBuf) -> Self {
        self.binary_override = Some(path);
        self
    }

    /// Override S3 health transition thresholds.
    pub fn with_s3_health_config(mut self, config: S3HealthConfig) -> Self {
        self.s3_health_config = config;
        self
    }

    /// Override cache pre-warming configuration.
    pub fn with_prewarm_config(mut self, config: CachePrewarmConfig) -> Self {
        self.prewarm_config = config;
        self
    }

    /// Override the volumes base directory (useful for tests).
    pub fn with_volumes_base(mut self, path: PathBuf) -> Self {
        self.volumes_base = path;
        self
    }

    /// Start a ZeroFS process for `volume_id`.
    ///
    /// Generates a TOML config file, writes it to `/tmp/syfrah/{volume_id}/zerofs.toml`,
    /// then spawns ZeroFS with `zerofs run -c <config_path>`. Secrets are passed via
    /// environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `ZEROFS_PASSWORD`)
    /// to avoid exposure in `/proc/pid/cmdline`.
    ///
    /// After ZeroFS starts and exposes the 9P unix socket, the host mounts it at
    /// `/var/lib/syfrah/volumes/{volume_id}/` via `mount -t 9p`. Cloud Hypervisor
    /// shares this directory with the guest via virtio-fs; containers bind-mount it.
    pub async fn start_volume(
        &mut self,
        volume_id: &str,
        s3: &S3Config,
        cache: &CacheConfig,
        encryption_passphrase: &str,
        generation: u64,
        size_gb: f64,
    ) -> Result<PathBuf, VolumeMgrError> {
        if self.processes.contains_key(volume_id) {
            return Err(VolumeMgrError::AlreadyRunning(volume_id.to_string()));
        }

        let binary_path = binary::resolve_binary(self.binary_override.as_deref())?;

        let ninep_socket = format!("/tmp/syfrah/{volume_id}/zerofs.9p.sock");
        let mount_point = self.volumes_base.join(volume_id);

        // Generate and write the TOML config.
        let config_toml = generate_config(volume_id, s3, cache, generation, size_gb);
        let config_dir = PathBuf::from(format!("/tmp/syfrah/{volume_id}"));
        tokio::fs::create_dir_all(&config_dir)
            .await
            .map_err(|e| VolumeMgrError::Spawn(format!("failed to create config dir: {e}")))?;
        tokio::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| {
                VolumeMgrError::Spawn(format!("failed to set config dir permissions: {e}"))
            })?;
        let config_path = config_dir.join("zerofs.toml");
        tokio::fs::write(&config_path, &config_toml)
            .await
            .map_err(|e| VolumeMgrError::Spawn(format!("failed to write zerofs.toml: {e}")))?;
        tokio::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| {
                VolumeMgrError::Spawn(format!("failed to set config file permissions: {e}"))
            })?;

        // Create the mount point directory.
        tokio::fs::create_dir_all(&mount_point)
            .await
            .map_err(|e| VolumeMgrError::Mount(format!("failed to create mount point: {e}")))?;

        // Spawn ZeroFS with TOML config; secrets via env vars.
        let mut child = Command::new(&binary_path)
            .arg("run")
            .arg("-c")
            .arg(&config_path)
            .env("AWS_ACCESS_KEY_ID", &s3.access_key)
            .env("AWS_SECRET_ACCESS_KEY", &s3.secret_key)
            .env("ZEROFS_PASSWORD", encryption_passphrase)
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| VolumeMgrError::Spawn(e.to_string()))?;

        // Wait for the 9P socket to appear.
        if let Err(e) = self
            .wait_for_path(std::path::Path::new(&ninep_socket))
            .await
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(e);
        }

        // Mount the 9P filesystem on the host.
        let mount_output = tokio::process::Command::new("mount")
            .arg("-t")
            .arg("9p")
            .arg("-o")
            .arg("trans=unix,version=9p2000.L")
            .arg(&ninep_socket)
            .arg(&mount_point)
            .output()
            .await
            .map_err(|e| VolumeMgrError::Mount(format!("mount command failed to execute: {e}")))?;

        if !mount_output.status.success() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let stderr = String::from_utf8_lossy(&mount_output.stderr);
            return Err(VolumeMgrError::Mount(format!(
                "mount -t 9p failed: {stderr}"
            )));
        }

        self.processes.insert(
            volume_id.to_string(),
            VolumeProcess {
                child,
                mount_point: mount_point.clone(),
                config_path,
                generation,
                s3_env: S3Env {
                    access_key: s3.access_key.clone(),
                    secret_key: s3.secret_key.clone(),
                    encryption_passphrase: encryption_passphrase.to_string(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        // Trigger cache pre-warming for migrated volumes (generation > 1).
        if generation > 1 && self.prewarm_config.enabled {
            self.start_prewarm(volume_id);
        }

        Ok(mount_point)
    }

    /// Stop a running ZeroFS process for `volume_id`.
    ///
    /// Unmounts the 9P filesystem, sends SIGTERM, and waits up to
    /// `STOP_GRACE_PERIOD`. If the process does not exit, it is killed
    /// with SIGKILL. The mount point directory is cleaned up afterward.
    pub async fn stop_volume(&mut self, volume_id: &str) -> Result<(), VolumeMgrError> {
        let mut proc = self
            .processes
            .remove(volume_id)
            .ok_or_else(|| VolumeMgrError::NotRunning(volume_id.to_string()))?;

        // Unmount the 9P filesystem before stopping ZeroFS.
        let _ = tokio::process::Command::new("umount")
            .arg(&proc.mount_point)
            .output()
            .await;

        // Send SIGTERM.
        #[cfg(unix)]
        {
            if let Some(pid) = proc.child.id() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
        }

        // Wait for graceful exit.
        let result = match time::timeout(STOP_GRACE_PERIOD, proc.child.wait()).await {
            Ok(Ok(_status)) => Ok(()),
            Ok(Err(e)) => Err(VolumeMgrError::Stop(format!(
                "error waiting for process: {e}"
            ))),
            Err(_elapsed) => {
                // Grace period expired — SIGKILL.
                proc.child
                    .kill()
                    .await
                    .map_err(|e| VolumeMgrError::Stop(format!("SIGKILL failed: {e}")))?;
                let _ = proc.child.wait().await;
                Ok(())
            }
        };

        // Clean up mount point directory.
        let _ = tokio::fs::remove_dir(&proc.mount_point).await;

        result
    }

    /// Stop a running ZeroFS process with explicit flush control.
    ///
    /// When `flush` is true (normal detach), sends SIGTERM and waits for the
    /// process to flush its cache to S3 before exiting. The grace period
    /// allows time for the flush to complete.
    ///
    /// When `flush` is false (force detach), sends SIGKILL immediately,
    /// skipping the cache flush. Data since the last fsync will be lost.
    ///
    /// In both cases the 9P mount is unmounted before stopping ZeroFS.
    pub async fn stop_volume_flush(
        &mut self,
        volume_id: &str,
        flush: bool,
    ) -> Result<(), VolumeMgrError> {
        if !flush {
            // Force detach: remove from tracking and SIGKILL immediately.
            let mut proc = self
                .processes
                .remove(volume_id)
                .ok_or_else(|| VolumeMgrError::NotRunning(volume_id.to_string()))?;
            // Unmount the 9P filesystem.
            let _ = tokio::process::Command::new("umount")
                .arg(&proc.mount_point)
                .output()
                .await;
            proc.child
                .kill()
                .await
                .map_err(|e| VolumeMgrError::Stop(format!("SIGKILL failed: {e}")))?;
            let _ = proc.child.wait().await;
            // Clean up mount point directory.
            let _ = tokio::fs::remove_dir(&proc.mount_point).await;
            return Ok(());
        }

        // Normal detach: delegate to stop_volume which does SIGTERM + grace.
        self.stop_volume(volume_id).await
    }

    /// Get the mount path for a running volume.
    ///
    /// Returns the host-side mount point where the 9P filesystem is mounted.
    /// Cloud Hypervisor uses this path for virtio-fs sharing; containers
    /// bind-mount it directly.
    ///
    /// Returns `None` if the volume is not running.
    pub fn get_mount_path(&self, volume_id: &str) -> Option<PathBuf> {
        self.processes.get(volume_id).map(|p| p.mount_point.clone())
    }

    /// Returns `true` if the volume has a tracked running process.
    pub fn is_running(&self, volume_id: &str) -> bool {
        self.processes.contains_key(volume_id)
    }

    /// Get the tracked generation for a running volume.
    ///
    /// Returns `None` if the volume is not tracked.
    pub fn get_generation(&self, volume_id: &str) -> Option<u64> {
        self.processes.get(volume_id).map(|p| p.generation)
    }

    /// Self-fence a volume whose generation is stale.
    ///
    /// On recovery after a reschedule, the source hypervisor's reconciler
    /// calls this with the current Raft generation. If the local process is
    /// running with a lower generation, we stop ZeroFS (SIGKILL — no flush,
    /// since a new writer at gen-{N+1} is already active) and discard the
    /// local cache.
    ///
    /// Returns `true` if a stale process was stopped, `false` if the volume
    /// was not running or already at the correct generation.
    pub async fn self_fence_stale(
        &mut self,
        volume_id: &str,
        current_generation: u64,
    ) -> Result<bool, VolumeMgrError> {
        let local_gen = match self.get_generation(volume_id) {
            Some(g) => g,
            None => return Ok(false), // not running locally — nothing to fence
        };

        if local_gen >= current_generation {
            return Ok(false); // up-to-date — no fencing needed
        }

        // Stale generation detected — force-kill (no flush, data belongs to new gen).
        self.stop_volume_flush(volume_id, false).await?;
        Ok(true)
    }

    /// List all actively tracked volumes as `(volume_id, generation)` pairs.
    pub fn list_active(&self) -> Vec<(String, u64)> {
        self.processes
            .iter()
            .map(|(id, proc)| (id.clone(), proc.generation))
            .collect()
    }

    // -------------------------------------------------------------------
    // Cache pre-warming (post-migration)
    // -------------------------------------------------------------------

    /// Start cache pre-warming for a volume.
    ///
    /// Called automatically by `start_volume` when `generation > 1` (indicating
    /// the volume migrated to this hypervisor). Can also be called manually.
    ///
    /// If pre-warming is already in progress for this volume, the existing task
    /// is left running.
    pub fn start_prewarm(&mut self, volume_id: &str) {
        let proc = match self.processes.get_mut(volume_id) {
            Some(p) => p,
            None => return,
        };

        // Don't restart if already warming.
        if let Some(ref handle) = proc.prewarm_handle {
            if !handle.progress().done {
                return;
            }
        }

        info!(
            volume = %volume_id,
            generation = proc.generation,
            "starting cache pre-warm for migrated volume"
        );

        let handle = CachePrewarmer::start(volume_id, &proc.mount_point, &self.prewarm_config);
        proc.prewarm_handle = Some(handle);
    }

    /// Cancel cache pre-warming for a volume.
    pub fn cancel_prewarm(&mut self, volume_id: &str) {
        if let Some(proc) = self.processes.get_mut(volume_id) {
            if let Some(ref handle) = proc.prewarm_handle {
                handle.cancel();
            }
            proc.prewarm_handle = None;
        }
    }

    /// Get the pre-warm progress for a specific volume.
    ///
    /// Returns `None` if the volume is not running or has no active pre-warm.
    pub fn prewarm_progress(&self, volume_id: &str) -> Option<CachePrewarmProgress> {
        self.processes
            .get(volume_id)
            .and_then(|p| p.prewarm_handle.as_ref())
            .map(|h| h.progress())
    }

    /// Get pre-warm progress for all volumes with active or recent warming.
    pub fn all_prewarm_progress(&self) -> Vec<CachePrewarmProgress> {
        self.processes
            .values()
            .filter_map(|p| p.prewarm_handle.as_ref().map(|h| h.progress()))
            .collect()
    }

    /// Get the current pre-warm configuration.
    pub fn prewarm_config(&self) -> &CachePrewarmConfig {
        &self.prewarm_config
    }

    // -------------------------------------------------------------------
    // S3 health tracking (ADR-006 §25)
    // -------------------------------------------------------------------

    /// Get the current S3 health config.
    pub fn s3_health_config(&self) -> &S3HealthConfig {
        &self.s3_health_config
    }

    /// Get the health state for a volume. Returns `None` if not running.
    pub fn volume_health(&self, volume_id: &str) -> Option<VolumeHealth> {
        self.processes
            .get(volume_id)
            .map(|p| p.health_tracker.health())
    }

    /// Record an S3 probe result for a specific volume.
    ///
    /// Called by the reconciler after probing S3. When `reachable` is false,
    /// the tracker records the outage start time and recomputes health
    /// based on configured thresholds.
    pub fn record_s3_probe(&mut self, volume_id: &str, reachable: bool) {
        if let Some(proc) = self.processes.get_mut(volume_id) {
            if reachable {
                proc.health_tracker.record_s3_reachable();
            } else {
                proc.health_tracker
                    .record_s3_unreachable(&self.s3_health_config);
            }
        }
    }

    /// Record S3 probe result for ALL running volumes at once.
    ///
    /// Useful when the S3 endpoint is shared across volumes — a single
    /// probe applies to every volume on this hypervisor.
    pub fn record_s3_probe_all(&mut self, reachable: bool) {
        let config = self.s3_health_config.clone();
        for proc in self.processes.values_mut() {
            if reachable {
                proc.health_tracker.record_s3_reachable();
            } else {
                proc.health_tracker.record_s3_unreachable(&config);
            }
        }
    }

    /// Check whether a volume can accept new writes.
    ///
    /// Returns `Ok(())` if the write is allowed, or
    /// `Err(VolumeMgrError::WriteRejected)` if the volume is in Error
    /// state or has exceeded the dirty bytes threshold.
    pub fn check_write_allowed(&self, volume_id: &str) -> Result<(), VolumeMgrError> {
        let proc = self
            .processes
            .get(volume_id)
            .ok_or_else(|| VolumeMgrError::NotRunning(volume_id.to_string()))?;

        if proc.health_tracker.can_accept_write(&self.s3_health_config) {
            Ok(())
        } else {
            Err(VolumeMgrError::WriteRejected(volume_id.to_string()))
        }
    }

    /// Record dirty bytes buffered locally for a volume during an S3 outage.
    pub fn add_dirty_bytes(&mut self, volume_id: &str, bytes: u64) {
        if let Some(proc) = self.processes.get_mut(volume_id) {
            proc.health_tracker.add_dirty_bytes(bytes);
        }
    }

    /// Record that `bytes` have been successfully flushed to S3 (recovery).
    ///
    /// When all dirty bytes are flushed and S3 is reachable, the volume
    /// transitions back to `Healthy`.
    pub fn record_flush(&mut self, volume_id: &str, bytes: u64) {
        if let Some(proc) = self.processes.get_mut(volume_id) {
            proc.health_tracker.flush_bytes(bytes);
        }
    }

    /// Get the total dirty bytes across all running volumes.
    pub fn total_dirty_bytes(&self) -> u64 {
        self.processes
            .values()
            .map(|p| p.health_tracker.dirty_bytes())
            .sum()
    }

    /// Produce health reports for all running volumes.
    ///
    /// These reports are suitable for gossip dissemination and inclusion
    /// in the `StorageStatusReport`.
    pub fn health_reports(&self) -> Vec<VolumeHealthReport> {
        self.processes
            .iter()
            .map(|(id, proc)| VolumeHealthReport {
                volume_id: id.clone(),
                health: proc.health_tracker.health(),
                dirty_bytes: proc.health_tracker.dirty_bytes(),
                flush_in_progress: proc.health_tracker.flush_in_progress(),
            })
            .collect()
    }

    /// Create a snapshot of a running volume using ZeroFS's native checkpoint.
    ///
    /// Runs `zerofs checkpoint create -c {config} {snapshot_name}` with the
    /// volume's S3 credentials passed as environment variables. This creates
    /// a crash-consistent checkpoint that can later be restored with
    /// `restore_from_checkpoint`.
    pub async fn capture_manifest(
        &self,
        volume_id: &str,
        snapshot_name: &str,
    ) -> Result<(), VolumeMgrError> {
        let proc = self
            .processes
            .get(volume_id)
            .ok_or_else(|| VolumeMgrError::NotRunning(volume_id.to_string()))?;

        let binary_path = binary::resolve_binary(self.binary_override.as_deref())?;

        let output = Command::new(&binary_path)
            .arg("checkpoint")
            .arg("create")
            .arg("-c")
            .arg(&proc.config_path)
            .arg(snapshot_name)
            .env("AWS_ACCESS_KEY_ID", &proc.s3_env.access_key)
            .env("AWS_SECRET_ACCESS_KEY", &proc.s3_env.secret_key)
            .env("ZEROFS_PASSWORD", &proc.s3_env.encryption_passphrase)
            .output()
            .await
            .map_err(|e| {
                VolumeMgrError::Checkpoint(format!("failed to run zerofs checkpoint create: {e}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VolumeMgrError::Checkpoint(format!(
                "zerofs checkpoint create failed: {stderr}"
            )));
        }

        Ok(())
    }

    /// List checkpoints for a running volume.
    ///
    /// Runs `zerofs checkpoint list -c {config}` and returns the raw
    /// stdout as a string. The caller is responsible for parsing the
    /// output into a structured format.
    pub async fn list_checkpoints(&self, volume_id: &str) -> Result<String, VolumeMgrError> {
        let proc = self
            .processes
            .get(volume_id)
            .ok_or_else(|| VolumeMgrError::NotRunning(volume_id.to_string()))?;

        let binary_path = binary::resolve_binary(self.binary_override.as_deref())?;

        let output = Command::new(&binary_path)
            .arg("checkpoint")
            .arg("list")
            .arg("-c")
            .arg(&proc.config_path)
            .env("AWS_ACCESS_KEY_ID", &proc.s3_env.access_key)
            .env("AWS_SECRET_ACCESS_KEY", &proc.s3_env.secret_key)
            .env("ZEROFS_PASSWORD", &proc.s3_env.encryption_passphrase)
            .output()
            .await
            .map_err(|e| {
                VolumeMgrError::Checkpoint(format!("failed to run zerofs checkpoint list: {e}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VolumeMgrError::Checkpoint(format!(
                "zerofs checkpoint list failed: {stderr}"
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Restore a volume from a ZeroFS checkpoint.
    ///
    /// Generates a new `zerofs.toml` for the restored volume and starts
    /// a new ZeroFS instance with `zerofs run -c {config} --checkpoint {name} --read-only`.
    /// The restored volume is mounted at `/var/lib/syfrah/volumes/{target_volume_id}/`.
    ///
    /// Returns the host-side mount point for the restored volume.
    #[allow(clippy::too_many_arguments)]
    pub async fn restore_from_checkpoint(
        &mut self,
        target_volume_id: &str,
        checkpoint_name: &str,
        s3: &S3Config,
        cache: &CacheConfig,
        encryption_passphrase: &str,
        generation: u64,
        size_gb: f64,
    ) -> Result<PathBuf, VolumeMgrError> {
        if self.processes.contains_key(target_volume_id) {
            return Err(VolumeMgrError::AlreadyRunning(target_volume_id.to_string()));
        }

        let binary_path = binary::resolve_binary(self.binary_override.as_deref())?;

        let mount_point = self.volumes_base.join(target_volume_id);

        // Generate and write a new TOML config for the restored volume.
        let config_toml = generate_config(target_volume_id, s3, cache, generation, size_gb);
        let config_dir = PathBuf::from(format!("/tmp/syfrah/{target_volume_id}"));
        tokio::fs::create_dir_all(&config_dir)
            .await
            .map_err(|e| VolumeMgrError::Spawn(format!("failed to create config dir: {e}")))?;
        tokio::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| {
                VolumeMgrError::Spawn(format!("failed to set config dir permissions: {e}"))
            })?;
        let config_path = config_dir.join("zerofs.toml");
        tokio::fs::write(&config_path, &config_toml)
            .await
            .map_err(|e| VolumeMgrError::Spawn(format!("failed to write zerofs.toml: {e}")))?;
        tokio::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| {
                VolumeMgrError::Spawn(format!("failed to set config file permissions: {e}"))
            })?;

        // Create the mount point directory.
        tokio::fs::create_dir_all(&mount_point)
            .await
            .map_err(|e| VolumeMgrError::Mount(format!("failed to create mount point: {e}")))?;

        let ninep_socket = format!("/tmp/syfrah/{target_volume_id}/zerofs.9p.sock");

        // Spawn ZeroFS from the checkpoint in read-only mode.
        let mut child = Command::new(&binary_path)
            .arg("run")
            .arg("-c")
            .arg(&config_path)
            .arg("--checkpoint")
            .arg(checkpoint_name)
            .arg("--read-only")
            .env("AWS_ACCESS_KEY_ID", &s3.access_key)
            .env("AWS_SECRET_ACCESS_KEY", &s3.secret_key)
            .env("ZEROFS_PASSWORD", encryption_passphrase)
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| VolumeMgrError::Spawn(e.to_string()))?;

        // Wait for the 9P socket to appear.
        if let Err(e) = self
            .wait_for_path(std::path::Path::new(&ninep_socket))
            .await
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(e);
        }

        // Mount the 9P filesystem on the host.
        let mount_output = tokio::process::Command::new("mount")
            .arg("-t")
            .arg("9p")
            .arg("-o")
            .arg("trans=unix,version=9p2000.L,ro")
            .arg(&ninep_socket)
            .arg(&mount_point)
            .output()
            .await
            .map_err(|e| VolumeMgrError::Mount(format!("mount command failed to execute: {e}")))?;

        if !mount_output.status.success() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let stderr = String::from_utf8_lossy(&mount_output.stderr);
            return Err(VolumeMgrError::Mount(format!(
                "mount -t 9p failed: {stderr}"
            )));
        }

        self.processes.insert(
            target_volume_id.to_string(),
            VolumeProcess {
                child,
                mount_point: mount_point.clone(),
                config_path,
                generation,
                s3_env: S3Env {
                    access_key: s3.access_key.clone(),
                    secret_key: s3.secret_key.clone(),
                    encryption_passphrase: encryption_passphrase.to_string(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        Ok(mount_point)
    }

    /// Reap any child processes that have exited (crashed or terminated
    /// externally). Returns volume IDs whose processes have exited.
    ///
    /// Callers should invoke this periodically (e.g. on a supervision tick)
    /// and update the observed state accordingly.
    pub async fn reap_exited(&mut self) -> Vec<String> {
        let mut exited = Vec::new();
        for (id, proc) in &mut self.processes {
            match proc.child.try_wait() {
                Ok(Some(_status)) => {
                    exited.push(id.clone());
                }
                Ok(None) => {} // still running
                Err(_) => {
                    // Can't inspect — treat as exited.
                    exited.push(id.clone());
                }
            }
        }
        for id in &exited {
            self.processes.remove(id);
        }
        exited
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Wait for a file/socket to appear on disk.
    async fn wait_for_path(&self, path: &Path) -> Result<(), VolumeMgrError> {
        let deadline = time::Instant::now() + SOCKET_WAIT_TIMEOUT;
        while time::Instant::now() < deadline {
            if path.exists() {
                return Ok(());
            }
            time::sleep(SOCKET_POLL_INTERVAL).await;
        }
        Err(VolumeMgrError::SocketTimeout(SOCKET_WAIT_TIMEOUT))
    }
}

impl Drop for VolumeMgr {
    fn drop(&mut self) {
        for (id, proc) in &self.processes {
            if let Some(pid) = proc.child.id() {
                eprintln!(
                    "VolumeMgr::drop: sending SIGTERM to orphaned ZeroFS process \
                     (volume={id}, pid={pid})"
                );
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            } else {
                eprintln!("VolumeMgr::drop: volume {id} has no PID (already exited?)");
            }
        }
        if !self.processes.is_empty() {
            eprintln!(
                "VolumeMgr::drop: {} volume process(es) were still active at drop time",
                self.processes.len()
            );
        }
    }
}

impl Default for VolumeMgr {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Test helpers (cfg(test) or cfg(feature = "test-helpers"))
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-helpers"))]
impl VolumeMgr {
    /// Inject a fake running process for testing.
    ///
    /// Spawns a long-running `sleep` process so that `is_running` and
    /// `list_active` behave as if a real ZeroFS process were tracked.
    pub fn inject_fake_process(&mut self, volume_id: &str, generation: u64) {
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn sleep for test helper");
        let mount_point = self.volumes_base.join(volume_id);
        let config_path = PathBuf::from(format!("/tmp/syfrah/{volume_id}/zerofs.toml"));
        self.processes.insert(
            volume_id.to_string(),
            VolumeProcess {
                child,
                mount_point,
                config_path,
                generation,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_has_no_active_volumes() {
        let mgr = VolumeMgr::new();
        assert!(mgr.list_active().is_empty());
        assert!(!mgr.is_running("vol-1"));
    }

    #[tokio::test]
    async fn list_active_returns_generation() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-gen".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-gen"),
                config_path: PathBuf::from("/tmp/syfrah/vol-gen/zerofs.toml"),
                generation: 42,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        let active = mgr.list_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, "vol-gen");
        assert_eq!(active[0].1, 42);

        mgr.stop_volume("vol-gen").await.ok();
    }

    #[test]
    fn with_binary_sets_override() {
        let mgr = VolumeMgr::new().with_binary(PathBuf::from("/opt/zerofs"));
        assert_eq!(mgr.binary_override, Some(PathBuf::from("/opt/zerofs")));
    }

    #[tokio::test]
    async fn start_volume_rejects_duplicate() {
        let mut mgr = VolumeMgr::new();
        // Manually insert a fake tracked process to simulate a running volume.
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-dup".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-dup"),
                config_path: PathBuf::from("/tmp/syfrah/vol-dup/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        let result = mgr
            .start_volume(
                "vol-dup",
                &S3Config {
                    endpoint: "http://s3:9000".into(),
                    bucket: "test".into(),
                    access_key: "ak".into(),
                    secret_key: "sk".into(),
                },
                &CacheConfig {
                    disk_path: PathBuf::from("/tmp/cache"),
                    disk_size_bytes: 1_073_741_824,
                    memory_size_bytes: 268_435_456,
                },
                "passphrase",
                1,
                50.0,
            )
            .await;

        assert!(matches!(result, Err(VolumeMgrError::AlreadyRunning(_))));

        // Cleanup the sleep process.
        mgr.stop_volume("vol-dup").await.ok();
    }

    #[tokio::test]
    async fn stop_volume_rejects_unknown() {
        let mut mgr = VolumeMgr::new();
        let result = mgr.stop_volume("nonexistent").await;
        assert!(matches!(result, Err(VolumeMgrError::NotRunning(_))));
    }

    #[tokio::test]
    async fn stop_volume_terminates_process() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-stop".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-stop"),
                config_path: PathBuf::from("/tmp/syfrah/vol-stop/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        assert!(mgr.is_running("vol-stop"));
        mgr.stop_volume("vol-stop").await.unwrap();
        assert!(!mgr.is_running("vol-stop"));
    }

    #[tokio::test]
    async fn reap_exited_detects_dead_process() {
        let mut mgr = VolumeMgr::new();
        // Spawn a process that exits immediately.
        let child = Command::new("true").kill_on_drop(false).spawn().unwrap();
        mgr.processes.insert(
            "vol-dead".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-dead"),
                config_path: PathBuf::from("/tmp/syfrah/vol-dead/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        // Give the process a moment to exit.
        time::sleep(Duration::from_millis(100)).await;

        let exited = mgr.reap_exited().await;
        assert!(exited.contains(&"vol-dead".to_string()));
        assert!(!mgr.is_running("vol-dead"));
    }

    #[tokio::test]
    async fn reap_exited_keeps_running_process() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-alive".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-alive"),
                config_path: PathBuf::from("/tmp/syfrah/vol-alive/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        let exited = mgr.reap_exited().await;
        assert!(exited.is_empty());
        assert!(mgr.is_running("vol-alive"));

        // Cleanup.
        mgr.stop_volume("vol-alive").await.ok();
    }

    #[test]
    fn default_impl_matches_new() {
        let mgr = VolumeMgr::default();
        let active: Vec<(String, u64)> = mgr.list_active();
        assert!(active.is_empty());
    }

    // ── stop_volume_flush tests (#1195) ─────────────────────────────

    #[tokio::test]
    async fn stop_volume_flush_graceful_terminates_process() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-flush".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-flush"),
                config_path: PathBuf::from("/tmp/syfrah/vol-flush/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        assert!(mgr.is_running("vol-flush"));
        mgr.stop_volume_flush("vol-flush", true).await.unwrap();
        assert!(!mgr.is_running("vol-flush"));
    }

    #[tokio::test]
    async fn stop_volume_flush_force_kills_immediately() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-force".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/tmp/test-volumes/vol-force"),
                config_path: PathBuf::from("/tmp/syfrah/vol-force/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        assert!(mgr.is_running("vol-force"));
        mgr.stop_volume_flush("vol-force", false).await.unwrap();
        assert!(!mgr.is_running("vol-force"));
    }

    #[tokio::test]
    async fn stop_volume_flush_unknown_returns_not_running() {
        let mut mgr = VolumeMgr::new();
        let result = mgr.stop_volume_flush("nonexistent", true).await;
        assert!(matches!(result, Err(VolumeMgrError::NotRunning(_))));
    }

    // ── get_mount_path tests ──────────────────────────────────────────

    #[test]
    fn get_mount_path_returns_none_for_unknown() {
        let mgr = VolumeMgr::new();
        assert!(mgr.get_mount_path("nonexistent").is_none());
    }

    #[tokio::test]
    async fn get_mount_path_returns_path_for_running_volume() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-9p".to_string(),
            VolumeProcess {
                child,
                mount_point: PathBuf::from("/var/lib/syfrah/volumes/vol-9p"),
                config_path: PathBuf::from("/tmp/syfrah/vol-9p/zerofs.toml"),
                generation: 1,
                s3_env: S3Env {
                    access_key: "test-ak".into(),
                    secret_key: "test-sk".into(),
                    encryption_passphrase: "test-pass".into(),
                },
                health_tracker: VolumeHealthTracker::new(),
                prewarm_handle: None,
            },
        );

        assert_eq!(
            mgr.get_mount_path("vol-9p"),
            Some(PathBuf::from("/var/lib/syfrah/volumes/vol-9p"))
        );

        // Cleanup.
        mgr.stop_volume("vol-9p").await.ok();
    }

    // ── get_generation tests (#1204) ───────────────────────────────

    #[test]
    fn get_generation_returns_none_for_unknown() {
        let mgr = VolumeMgr::new();
        assert_eq!(mgr.get_generation("nonexistent"), None);
    }

    #[tokio::test]
    async fn get_generation_returns_tracked_generation() {
        let mut mgr = VolumeMgr::new();
        mgr.inject_fake_process("vol-gen", 5);
        assert_eq!(mgr.get_generation("vol-gen"), Some(5));
        mgr.stop_volume("vol-gen").await.ok();
    }

    // ── self_fence_stale tests (#1204) ─────────────────────────────

    #[tokio::test]
    async fn self_fence_stale_kills_old_generation() {
        let mut mgr = VolumeMgr::new();
        mgr.inject_fake_process("vol-stale", 3);
        assert!(mgr.is_running("vol-stale"));

        // Current Raft generation is 5, local is 3 — stale.
        let fenced = mgr.self_fence_stale("vol-stale", 5).await.unwrap();
        assert!(fenced, "stale process should have been fenced");
        assert!(!mgr.is_running("vol-stale"), "process should be stopped");
    }

    #[tokio::test]
    async fn self_fence_stale_noop_for_current_generation() {
        let mut mgr = VolumeMgr::new();
        mgr.inject_fake_process("vol-current", 5);

        // Same generation — no fencing needed.
        let fenced = mgr.self_fence_stale("vol-current", 5).await.unwrap();
        assert!(!fenced, "current gen should not be fenced");
        assert!(mgr.is_running("vol-current"), "process should still run");
        mgr.stop_volume("vol-current").await.ok();
    }

    #[tokio::test]
    async fn self_fence_stale_noop_for_unknown_volume() {
        let mut mgr = VolumeMgr::new();
        let fenced = mgr.self_fence_stale("vol-unknown", 5).await.unwrap();
        assert!(!fenced, "unknown volume should not trigger fencing");
    }

    #[tokio::test]
    async fn self_fence_stale_noop_for_newer_generation() {
        let mut mgr = VolumeMgr::new();
        mgr.inject_fake_process("vol-future", 10);

        // Local gen 10, Raft gen 8 — local is ahead (shouldn't happen, but safe).
        let fenced = mgr.self_fence_stale("vol-future", 8).await.unwrap();
        assert!(!fenced, "newer local gen should not be fenced");
        assert!(mgr.is_running("vol-future"));
        mgr.stop_volume("vol-future").await.ok();
    }

    // ── VolumeManifest tests (#1200) ───────────────────────────────

    #[test]
    fn volume_manifest_serde_roundtrip() {
        let manifest = VolumeManifest {
            sst_files: vec!["sst-001.sst".into(), "sst-002.sst".into()],
            wal_position: 42,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: VolumeManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn volume_manifest_empty_sst_files() {
        let manifest = VolumeManifest {
            sst_files: vec![],
            wal_position: 0,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: VolumeManifest = serde_json::from_str(&json).unwrap();
        assert!(deserialized.sst_files.is_empty());
        assert_eq!(deserialized.wal_position, 0);
    }

    #[tokio::test]
    async fn capture_manifest_rejects_unknown_volume() {
        let mgr = VolumeMgr::new();
        let result = mgr.capture_manifest("nonexistent", "snap-1").await;
        assert!(matches!(result, Err(VolumeMgrError::NotRunning(_))));
    }

    #[tokio::test]
    async fn list_checkpoints_rejects_unknown_volume() {
        let mgr = VolumeMgr::new();
        let result = mgr.list_checkpoints("nonexistent").await;
        assert!(matches!(result, Err(VolumeMgrError::NotRunning(_))));
    }

    #[tokio::test]
    async fn restore_from_checkpoint_rejects_duplicate() {
        let mut mgr = VolumeMgr::new();
        mgr.inject_fake_process("vol-dup-restore", 1);

        let result = mgr
            .restore_from_checkpoint(
                "vol-dup-restore",
                "snap-1",
                &S3Config {
                    endpoint: "http://s3:9000".into(),
                    bucket: "test".into(),
                    access_key: "ak".into(),
                    secret_key: "sk".into(),
                },
                &CacheConfig {
                    disk_path: PathBuf::from("/tmp/cache"),
                    disk_size_bytes: 1_073_741_824,
                    memory_size_bytes: 268_435_456,
                },
                "passphrase",
                1,
                50.0,
            )
            .await;

        assert!(matches!(result, Err(VolumeMgrError::AlreadyRunning(_))));
        mgr.stop_volume("vol-dup-restore").await.ok();
    }

    // ── VolumeHealth tests (#1209 — S3 outage degradation) ─────────

    #[test]
    fn volume_health_display() {
        assert_eq!(VolumeHealth::Healthy.to_string(), "Healthy");
        assert_eq!(VolumeHealth::Degraded.to_string(), "Degraded");
        assert_eq!(VolumeHealth::Error.to_string(), "Error");
    }

    #[test]
    fn volume_health_serde_roundtrip() {
        let health = VolumeHealth::Degraded;
        let json = serde_json::to_string(&health).unwrap();
        let deser: VolumeHealth = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, health);
    }

    #[test]
    fn health_tracker_starts_healthy() {
        let tracker = VolumeHealthTracker::new();
        assert_eq!(tracker.health(), VolumeHealth::Healthy);
        assert_eq!(tracker.dirty_bytes(), 0);
        assert!(!tracker.flush_in_progress());
    }

    #[test]
    fn health_tracker_transitions_to_degraded() {
        let mut tracker = VolumeHealthTracker::new();
        // Use very short thresholds for testing.
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_secs(3600),
            max_dirty_bytes: 1_073_741_824,
        };
        tracker.record_s3_unreachable(&config);
        assert_eq!(tracker.health(), VolumeHealth::Degraded);
    }

    #[test]
    fn health_tracker_transitions_to_error() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_millis(0),
            max_dirty_bytes: 1_073_741_824,
        };
        tracker.record_s3_unreachable(&config);
        assert_eq!(tracker.health(), VolumeHealth::Error);
    }

    #[test]
    fn health_tracker_recovers_when_s3_returns() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_secs(3600),
            max_dirty_bytes: 1_073_741_824,
        };
        tracker.record_s3_unreachable(&config);
        assert_eq!(tracker.health(), VolumeHealth::Degraded);

        // S3 returns, no dirty bytes → immediate Healthy.
        tracker.record_s3_reachable();
        assert_eq!(tracker.health(), VolumeHealth::Healthy);
    }

    #[test]
    fn health_tracker_recovery_with_dirty_bytes() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_secs(3600),
            max_dirty_bytes: 1_073_741_824,
        };
        tracker.record_s3_unreachable(&config);
        tracker.add_dirty_bytes(1000);

        // S3 returns but there are dirty bytes → flush needed.
        tracker.record_s3_reachable();
        assert!(tracker.flush_in_progress());
        assert_ne!(tracker.health(), VolumeHealth::Healthy);

        // Flush completes.
        tracker.flush_bytes(1000);
        assert_eq!(tracker.health(), VolumeHealth::Healthy);
        assert!(!tracker.flush_in_progress());
        assert_eq!(tracker.dirty_bytes(), 0);
    }

    #[test]
    fn health_tracker_rejects_writes_at_threshold() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_secs(3600),
            max_dirty_bytes: 100,
        };
        tracker.record_s3_unreachable(&config);
        tracker.add_dirty_bytes(50);
        assert!(tracker.can_accept_write(&config));

        tracker.add_dirty_bytes(60); // now at 110, exceeds 100
        assert!(!tracker.can_accept_write(&config));
    }

    #[test]
    fn health_tracker_rejects_writes_in_error_state() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_millis(0),
            max_dirty_bytes: u64::MAX,
        };
        tracker.record_s3_unreachable(&config);
        assert_eq!(tracker.health(), VolumeHealth::Error);
        assert!(!tracker.can_accept_write(&config));
    }

    #[tokio::test]
    async fn volume_mgr_health_tracking() {
        let mut mgr = VolumeMgr::new().with_s3_health_config(S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_secs(3600),
            max_dirty_bytes: 1_073_741_824,
        });
        mgr.inject_fake_process("vol-h", 1);

        // Initially healthy.
        assert_eq!(mgr.volume_health("vol-h"), Some(VolumeHealth::Healthy));
        assert!(mgr.check_write_allowed("vol-h").is_ok());

        // S3 goes down.
        mgr.record_s3_probe("vol-h", false);
        assert_eq!(mgr.volume_health("vol-h"), Some(VolumeHealth::Degraded));

        // Buffer some dirty bytes.
        mgr.add_dirty_bytes("vol-h", 500);
        assert_eq!(mgr.total_dirty_bytes(), 500);

        // S3 comes back.
        mgr.record_s3_probe("vol-h", true);
        // Flush dirty data.
        mgr.record_flush("vol-h", 500);
        assert_eq!(mgr.volume_health("vol-h"), Some(VolumeHealth::Healthy));
        assert_eq!(mgr.total_dirty_bytes(), 0);

        mgr.stop_volume("vol-h").await.ok();
    }

    #[tokio::test]
    async fn volume_mgr_health_reports() {
        let mut mgr = VolumeMgr::new().with_s3_health_config(S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_secs(3600),
            max_dirty_bytes: 1_073_741_824,
        });
        mgr.inject_fake_process("vol-r1", 1);
        mgr.inject_fake_process("vol-r2", 1);

        mgr.record_s3_probe_all(false);
        let reports = mgr.health_reports();
        assert_eq!(reports.len(), 2);
        for r in &reports {
            assert_eq!(r.health, VolumeHealth::Degraded);
        }

        mgr.stop_volume("vol-r1").await.ok();
        mgr.stop_volume("vol-r2").await.ok();
    }

    #[test]
    fn volume_health_unknown_volume() {
        let mgr = VolumeMgr::new();
        assert_eq!(mgr.volume_health("nonexistent"), None);
    }

    #[test]
    fn health_tracker_error_recovery_with_dirty_bytes() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_millis(0),
            max_dirty_bytes: 1_073_741_824,
        };

        // Drive into Error state.
        tracker.record_s3_unreachable(&config);
        assert_eq!(tracker.health(), VolumeHealth::Error);
        assert!(
            !tracker.can_accept_write(&config),
            "Error state rejects writes"
        );

        // Accumulate dirty bytes while in Error.
        tracker.add_dirty_bytes(5000);

        // S3 returns — should downgrade to Degraded so writes resume.
        tracker.record_s3_reachable();
        assert_eq!(tracker.health(), VolumeHealth::Degraded);
        assert!(tracker.flush_in_progress());
        assert!(
            tracker.can_accept_write(&config),
            "writes should resume during flush"
        );

        // Flush completes → Healthy.
        tracker.flush_bytes(5000);
        assert_eq!(tracker.health(), VolumeHealth::Healthy);
        assert!(!tracker.flush_in_progress());
    }

    #[test]
    fn health_tracker_error_recovery_no_dirty_bytes() {
        let mut tracker = VolumeHealthTracker::new();
        let config = S3HealthConfig {
            degraded_after: Duration::from_millis(0),
            error_after: Duration::from_millis(0),
            max_dirty_bytes: 1_073_741_824,
        };

        // Drive into Error state with no dirty bytes.
        tracker.record_s3_unreachable(&config);
        assert_eq!(tracker.health(), VolumeHealth::Error);

        // S3 returns, no dirty bytes → immediate Healthy.
        tracker.record_s3_reachable();
        assert_eq!(tracker.health(), VolumeHealth::Healthy);
        assert!(!tracker.flush_in_progress());
    }

    #[test]
    fn check_write_rejected_unknown_volume() {
        let mgr = VolumeMgr::new();
        assert!(matches!(
            mgr.check_write_allowed("nonexistent"),
            Err(VolumeMgrError::NotRunning(_))
        ));
    }

    #[test]
    fn s3_health_config_defaults() {
        let config = S3HealthConfig::default();
        assert_eq!(config.degraded_after, Duration::from_secs(300));
        assert_eq!(config.error_after, Duration::from_secs(1800));
        assert_eq!(config.max_dirty_bytes, 1_073_741_824);
    }

    #[test]
    fn volume_health_report_serde() {
        let report = VolumeHealthReport {
            volume_id: "vol-1".into(),
            health: VolumeHealth::Degraded,
            dirty_bytes: 1024,
            flush_in_progress: false,
        };
        let json = serde_json::to_string(&report).unwrap();
        let deser: VolumeHealthReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.health, VolumeHealth::Degraded);
        assert_eq!(deser.dirty_bytes, 1024);
    }

    // ── generate_config tests (#1250) ─────────────────────────────────

    #[test]
    fn generate_config_produces_valid_toml() {
        let s3 = S3Config {
            endpoint: "https://s3.us-east-1.amazonaws.com".into(),
            bucket: "my-bucket".into(),
            access_key: "AKID".into(),
            secret_key: "SECRET".into(),
        };
        let cache = CacheConfig {
            disk_path: PathBuf::from("/tmp/cache"),
            disk_size_bytes: 10 * 1_073_741_824,  // 10 GiB
            memory_size_bytes: 2 * 1_073_741_824, // 2 GiB
        };
        let toml_str = generate_config("vol-abc", &s3, &cache, 1, 50.0);

        // Verify key TOML sections and values are present.
        assert!(
            toml_str.contains("[cache]"),
            "missing [cache] section:\n{toml_str}"
        );
        assert!(
            toml_str.contains("dir = \"/tmp/syfrah-cache/vol-abc\""),
            "wrong cache dir:\n{toml_str}"
        );
        assert!(
            toml_str.contains("disk_size_gb = 10.0"),
            "wrong disk_size_gb:\n{toml_str}"
        );
        assert!(
            toml_str.contains("memory_size_gb = 2.0"),
            "wrong memory_size_gb:\n{toml_str}"
        );
        assert!(
            toml_str.contains("[storage]"),
            "missing [storage] section:\n{toml_str}"
        );
        assert!(
            toml_str.contains("url = \"s3://my-bucket/volumes/vol-abc/gen-1/\""),
            "wrong s3 url:\n{toml_str}"
        );
        assert!(
            toml_str.contains("encryption_password = \"${ZEROFS_PASSWORD}\""),
            "missing encryption_password placeholder:\n{toml_str}"
        );
        assert!(
            toml_str.contains("[filesystem]"),
            "missing [filesystem] section:\n{toml_str}"
        );
        assert!(
            toml_str.contains("max_size_gb = 50.0"),
            "wrong max_size_gb:\n{toml_str}"
        );
        assert!(
            toml_str.contains("compression = \"lz4\""),
            "missing compression:\n{toml_str}"
        );
        assert!(
            toml_str.contains("[servers.ninep]"),
            "missing [servers.ninep] section:\n{toml_str}"
        );
        assert!(
            toml_str.contains("unix_socket = \"/tmp/syfrah/vol-abc/zerofs.9p.sock\""),
            "wrong unix_socket:\n{toml_str}"
        );
        assert!(
            toml_str.contains("[lsm]"),
            "missing [lsm] section:\n{toml_str}"
        );
        assert!(
            toml_str.contains("wal_enabled = true"),
            "missing wal_enabled:\n{toml_str}"
        );
        assert!(
            toml_str.contains("[aws]"),
            "missing [aws] section:\n{toml_str}"
        );
        assert!(
            toml_str.contains("endpoint = \"https://s3.us-east-1.amazonaws.com\""),
            "missing aws endpoint:\n{toml_str}"
        );
        assert!(
            toml_str.contains("access_key_id = \"${AWS_ACCESS_KEY_ID}\""),
            "missing aws access_key_id placeholder:\n{toml_str}"
        );
        assert!(
            toml_str.contains("secret_access_key = \"${AWS_SECRET_ACCESS_KEY}\""),
            "missing aws secret_access_key placeholder:\n{toml_str}"
        );
    }

    #[test]
    fn generate_config_different_volumes_produce_unique_paths() {
        let s3 = S3Config {
            endpoint: "https://s3.example.com".into(),
            bucket: "bucket".into(),
            access_key: "ak".into(),
            secret_key: "sk".into(),
        };
        let cache = CacheConfig {
            disk_path: PathBuf::from("/tmp/cache"),
            disk_size_bytes: 1_073_741_824,
            memory_size_bytes: 268_435_456,
        };

        let config_a = generate_config("vol-aaa", &s3, &cache, 1, 10.0);
        let config_b = generate_config("vol-bbb", &s3, &cache, 3, 20.0);

        // Each volume gets its own cache dir, socket, and S3 prefix.
        assert!(config_a.contains("vol-aaa"));
        assert!(config_b.contains("vol-bbb"));
        assert!(config_a.contains("gen-1"));
        assert!(config_b.contains("gen-3"));
        assert!(config_a.contains("max_size_gb = 10.0"));
        assert!(config_b.contains("max_size_gb = 20.0"));
    }

    #[test]
    fn generate_config_no_cli_secrets() {
        let s3 = S3Config {
            endpoint: "https://s3.example.com".into(),
            bucket: "bucket".into(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        };
        let cache = CacheConfig {
            disk_path: PathBuf::from("/tmp/cache"),
            disk_size_bytes: 1_073_741_824,
            memory_size_bytes: 268_435_456,
        };

        let toml_str = generate_config("vol-sec", &s3, &cache, 1, 50.0);

        // Secrets should NOT appear as literal values in the config.
        assert!(
            !toml_str.contains("AKIAIOSFODNN7EXAMPLE"),
            "access key should not be in config"
        );
        assert!(
            !toml_str.contains("wJalrXUtnFEMI"),
            "secret key should not be in config"
        );
        // Instead, env var placeholders should be used.
        assert!(toml_str.contains("${AWS_ACCESS_KEY_ID}"));
        assert!(toml_str.contains("${AWS_SECRET_ACCESS_KEY}"));
        assert!(toml_str.contains("${ZEROFS_PASSWORD}"));
    }
}
