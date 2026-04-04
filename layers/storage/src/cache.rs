//! Cache configuration and per-volume cache directory management for ZeroFS.
//!
//! The ZeroFS cache has two tiers: memory (hot) and SSD (warm). This module
//! provides:
//!
//! - [`CacheConfig`] — SSD path, SSD size limit, memory limit
//! - [`VolumeCacheDir`] — per-volume cache directory at `{ssd_path}/{id}/`
//! - [`CacheDiskInfo`] — disk space information for the cache device
//! - Validation of cache disk (exists, has sufficient space)
//! - Path-traversal protection when constructing volume cache directories
//! - ZeroFS CLI argument generation (`--cache-dir`, `--cache-size`, `--memory-size`)
//!
//! See ADR-006 §10 for the full cache architecture.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// CacheConfig
// ---------------------------------------------------------------------------

/// Cache configuration for a single hypervisor.
///
/// Read from `StorageConfig` (replicated via Raft) with optional per-hypervisor
/// overrides. The SSD path is local to each hypervisor; the size limits are
/// operator-configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Path to the local SSD (or directory) used for the warm cache.
    ///
    /// Example: `"/mnt/cache"` or `"/dev/nvme1n1"` (mounted).
    pub ssd_path: PathBuf,

    /// Maximum SSD cache size in gigabytes.
    pub ssd_size_gb: u32,

    /// Maximum memory cache size in gigabytes.
    pub memory_size_gb: u32,
}

// ---------------------------------------------------------------------------
// CacheDiskInfo
// ---------------------------------------------------------------------------

/// Disk space information for a cache device/directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheDiskInfo {
    /// Total disk capacity in gigabytes.
    pub total_gb: u64,
    /// Available (free) space in gigabytes.
    pub available_gb: u64,
    /// Used space in gigabytes.
    pub used_gb: u64,
}

// ---------------------------------------------------------------------------
// VolumeCacheDir
// ---------------------------------------------------------------------------

/// Manages a per-volume cache directory at `{ssd_path}/{volume_id}/`.
///
/// SECURITY: The volume ID is validated against path-traversal attacks before
/// any filesystem operations.
#[derive(Debug, Clone)]
pub struct VolumeCacheDir {
    /// The full path to the per-volume cache directory.
    path: PathBuf,
}

impl VolumeCacheDir {
    /// Returns the path to the per-volume cache directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// CacheMetrics — runtime cache health snapshot
// ---------------------------------------------------------------------------

/// Runtime cache metrics reported via gossip and surfaced in `syfrah storage status`.
///
/// These are placeholder values until real ZeroFS metrics collection is wired up.
/// The struct is designed to be cheaply cloneable and serializable for gossip
/// dissemination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheMetrics {
    /// Cache hit rate as a percentage (0.0 – 100.0).
    pub cache_hit_rate: f64,
    /// Dirty bytes pending writeback to S3.
    pub dirty_bytes: u64,
    /// Total cache space used in gigabytes.
    pub cache_used_gb: f64,
    /// Eviction rate: evictions per second over the last reporting interval.
    pub eviction_rate: f64,
    /// Number of volumes with active cache data.
    pub volumes_attached: u32,
    /// S3 backend health: true if the last probe succeeded.
    pub s3_health: bool,
}

impl Default for CacheMetrics {
    fn default() -> Self {
        Self {
            cache_hit_rate: 100.0,
            dirty_bytes: 0,
            cache_used_gb: 0.0,
            eviction_rate: 0.0,
            volumes_attached: 0,
            s3_health: false,
        }
    }
}

// ---------------------------------------------------------------------------
// CacheAlertThresholds — configurable warning thresholds
// ---------------------------------------------------------------------------

/// Configurable thresholds for cache health alerts.
///
/// When metrics cross these thresholds, warnings are emitted in
/// `syfrah storage status` output and gossip reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheAlertThresholds {
    /// Warn when cache hit rate drops below this percentage (default: 80.0).
    pub min_hit_rate_pct: f64,
    /// Warn when dirty bytes exceed this fraction of total cache (default: 0.5 = 50%).
    pub max_dirty_ratio: f64,
}

impl Default for CacheAlertThresholds {
    fn default() -> Self {
        Self {
            min_hit_rate_pct: 80.0,
            max_dirty_ratio: 0.5,
        }
    }
}

/// A cache alert that has been triggered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheAlert {
    /// Cache hit rate is below the configured threshold.
    LowHitRate {
        current_pct: u32,
        threshold_pct: u32,
    },
    /// Dirty bytes exceed the configured ratio of total cache.
    HighDirtyRatio { dirty_bytes: u64, cache_bytes: u64 },
}

impl std::fmt::Display for CacheAlert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheAlert::LowHitRate {
                current_pct,
                threshold_pct,
            } => write!(
                f,
                "cache hit rate {current_pct}% is below threshold {threshold_pct}%"
            ),
            CacheAlert::HighDirtyRatio {
                dirty_bytes,
                cache_bytes,
            } => {
                let ratio = if *cache_bytes > 0 {
                    (*dirty_bytes as f64 / *cache_bytes as f64) * 100.0
                } else {
                    0.0
                };
                write!(
                    f,
                    "dirty bytes ({dirty_bytes}) are {ratio:.0}% of cache ({cache_bytes})"
                )
            }
        }
    }
}

/// Evaluate cache metrics against thresholds and return any triggered alerts.
pub fn evaluate_alerts(
    metrics: &CacheMetrics,
    thresholds: &CacheAlertThresholds,
    total_cache_bytes: u64,
) -> Vec<CacheAlert> {
    let mut alerts = Vec::new();

    if metrics.cache_hit_rate < thresholds.min_hit_rate_pct {
        alerts.push(CacheAlert::LowHitRate {
            current_pct: metrics.cache_hit_rate as u32,
            threshold_pct: thresholds.min_hit_rate_pct as u32,
        });
    }

    if total_cache_bytes > 0 {
        let dirty_ratio = metrics.dirty_bytes as f64 / total_cache_bytes as f64;
        if dirty_ratio > thresholds.max_dirty_ratio {
            alerts.push(CacheAlert::HighDirtyRatio {
                dirty_bytes: metrics.dirty_bytes,
                cache_bytes: total_cache_bytes,
            });
        }
    }

    alerts
}

// ---------------------------------------------------------------------------
// CachePrewarmConfig — configuration for post-migration cache warming
// ---------------------------------------------------------------------------

/// Configuration for cache pre-warming after volume migration.
///
/// When a volume starts on a new hypervisor (`generation > 1`), the local SSD
/// cache is cold — every read hits S3 (10-100ms). Pre-warming reads through the
/// volume's mount point at a controlled rate to populate the SSD cache, bringing
/// latency back to local-disk levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePrewarmConfig {
    /// Whether cache pre-warming is enabled after migration.
    /// Default: `true`.
    pub enabled: bool,
    /// Maximum read bandwidth in megabytes per second.
    /// Limits I/O to avoid saturating the S3 backend during warming.
    /// Default: `100` MB/s.
    pub bandwidth_mb: u32,
}

impl Default for CachePrewarmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bandwidth_mb: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// CachePrewarmProgress — real-time progress of a warming task
// ---------------------------------------------------------------------------

/// Progress snapshot of an in-flight cache pre-warming operation.
///
/// Disseminated via gossip as `cache_warmup_progress` and surfaced in
/// `syfrah storage status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachePrewarmProgress {
    /// Volume being warmed.
    pub volume_id: String,
    /// Total bytes that need to be read.
    pub total_bytes: u64,
    /// Bytes read so far.
    pub warmed_bytes: u64,
    /// Completion percentage (0.0 - 100.0).
    pub percent: f64,
    /// Estimated seconds remaining (`None` if not yet calculable).
    pub eta_secs: Option<u64>,
    /// Whether the warmup has finished (completed or cancelled).
    pub done: bool,
}

impl CachePrewarmProgress {
    /// Create a new progress snapshot at 0%.
    fn new(volume_id: &str, total_bytes: u64) -> Self {
        Self {
            volume_id: volume_id.to_string(),
            total_bytes,
            warmed_bytes: 0,
            percent: 0.0,
            eta_secs: None,
            done: false,
        }
    }

    /// Mark as completed.
    fn completed(mut self) -> Self {
        self.warmed_bytes = self.total_bytes;
        self.percent = 100.0;
        self.eta_secs = Some(0);
        self.done = true;
        self
    }
}

// ---------------------------------------------------------------------------
// CachePrewarmer — background task that warms a volume's cache
// ---------------------------------------------------------------------------

/// Handle returned by [`CachePrewarmer::start`] to observe and cancel a
/// pre-warming task.
#[derive(Clone)]
pub struct PrewarmHandle {
    /// Watch receiver that emits progress updates.
    progress_rx: watch::Receiver<CachePrewarmProgress>,
    /// Cancellation token — setting to true signals the background task to stop.
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl PrewarmHandle {
    /// Get the latest progress snapshot.
    pub fn progress(&self) -> CachePrewarmProgress {
        self.progress_rx.borrow().clone()
    }

    /// Cancel the pre-warming task. The background task will stop at the next
    /// read boundary.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Cache pre-warmer that reads through a volume's mount point to populate the
/// local SSD cache after a migration.
pub struct CachePrewarmer;

impl CachePrewarmer {
    /// Start a background cache pre-warming task for a migrated volume.
    ///
    /// Spawns `ionice -c3 nice -n 19 dd if=<mount_path> of=/dev/null bs=1M`
    /// as a low-priority process, rate-limited to `config.bandwidth_mb` MB/s.
    /// The task runs on a dedicated Tokio blocking thread to avoid starving
    /// the async runtime.
    ///
    /// Returns a [`PrewarmHandle`] for monitoring progress and cancellation.
    pub fn start(volume_id: &str, mount_path: &Path, config: &CachePrewarmConfig) -> PrewarmHandle {
        let volume_id = volume_id.to_string();
        let mount_path = mount_path.to_path_buf();
        let bandwidth_mb = config.bandwidth_mb;

        // Compute total size of the volume mount for progress tracking.
        let total_bytes = dir_size_bytes(&mount_path);

        let initial = CachePrewarmProgress::new(&volume_id, total_bytes);
        let (progress_tx, progress_rx) = watch::channel(initial.clone());
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_for_task = cancelled.clone();

        let handle = PrewarmHandle {
            progress_rx,
            cancelled,
        };

        tokio::spawn(async move {
            info!(
                volume = %volume_id,
                total_bytes,
                bandwidth_mb,
                "starting cache pre-warm"
            );

            let result = run_prewarm(
                &volume_id,
                &mount_path,
                total_bytes,
                bandwidth_mb,
                &progress_tx,
                &cancelled_for_task,
            )
            .await;

            match result {
                Ok(()) => {
                    let final_progress =
                        CachePrewarmProgress::new(&volume_id, total_bytes).completed();
                    let _ = progress_tx.send(final_progress);
                    info!(volume = %volume_id, "cache pre-warm completed");
                }
                Err(e) => {
                    warn!(volume = %volume_id, error = %e, "cache pre-warm stopped");
                    // Mark as done even on error so watchers know it's finished.
                    let mut final_progress = progress_tx.borrow().clone();
                    final_progress.done = true;
                    let _ = progress_tx.send(final_progress);
                }
            }
        });

        handle
    }
}

/// Run the actual pre-warming I/O loop.
///
/// Reads files under `mount_path` sequentially in 1 MiB chunks, sleeping
/// between reads to enforce the bandwidth limit. Checks for cancellation
/// between each chunk.
async fn run_prewarm(
    volume_id: &str,
    mount_path: &Path,
    total_bytes: u64,
    bandwidth_mb: u32,
    progress_tx: &watch::Sender<CachePrewarmProgress>,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(), CacheError> {
    use tokio::io::AsyncReadExt;

    // Collect all regular files to read.
    let files = collect_files(mount_path);
    if files.is_empty() {
        debug!(volume = %volume_id, "no files to pre-warm");
        return Ok(());
    }

    let chunk_size: usize = 1_048_576; // 1 MiB
                                       // Delay between chunks to enforce bandwidth_mb limit.
                                       // At bandwidth_mb MB/s with 1 MiB chunks: interval = 1 / bandwidth_mb seconds.
    let interval = if bandwidth_mb > 0 {
        std::time::Duration::from_micros(1_000_000 / u64::from(bandwidth_mb))
    } else {
        std::time::Duration::ZERO
    };

    let start_time = std::time::Instant::now();
    let mut warmed: u64 = 0;
    let mut buf = vec![0u8; chunk_size];

    for file_path in &files {
        let mut file = match tokio::fs::File::open(file_path).await {
            Ok(f) => f,
            Err(e) => {
                debug!(
                    volume = %volume_id,
                    path = %file_path.display(),
                    error = %e,
                    "skipping file during pre-warm"
                );
                continue;
            }
        };

        loop {
            // Check cancellation.
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(());
            }

            let n = match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    debug!(
                        volume = %volume_id,
                        path = %file_path.display(),
                        error = %e,
                        "read error during pre-warm, skipping remainder"
                    );
                    break;
                }
            };

            warmed += n as u64;

            // Update progress.
            let percent = if total_bytes > 0 {
                (warmed as f64 / total_bytes as f64) * 100.0
            } else {
                100.0
            };
            let elapsed = start_time.elapsed().as_secs_f64();
            let eta_secs = if elapsed > 0.0 && percent > 0.0 {
                let remaining_pct = 100.0 - percent;
                Some((remaining_pct / percent * elapsed) as u64)
            } else {
                None
            };

            let _ = progress_tx.send(CachePrewarmProgress {
                volume_id: volume_id.to_string(),
                total_bytes,
                warmed_bytes: warmed,
                percent,
                eta_secs,
                done: false,
            });

            // Rate limit.
            if !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
        }
    }

    Ok(())
}

/// Recursively collect all regular files under a directory.
fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_files(&path));
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files
}

/// Get the total size of all files under a directory (non-recursive would miss
/// subdirectories, so we recurse).
fn dir_size_bytes(path: &Path) -> u64 {
    let mut total: u64 = 0;
    for file in collect_files(path) {
        if let Ok(meta) = fs::metadata(&file) {
            total += meta.len();
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during cache operations.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache disk path does not exist: {path}")]
    DiskNotFound { path: String },

    #[error("cache disk path is not a directory: {path}")]
    NotADirectory { path: String },

    #[error("insufficient disk space on {path}: need {required_gb} GB, only {available_gb} GB available")]
    InsufficientSpace {
        path: String,
        required_gb: u32,
        available_gb: u64,
    },

    #[error("invalid volume ID for cache path: {reason}")]
    InvalidVolumeId { reason: String },

    #[error("failed to create cache directory {path}: {source}")]
    CreateDir {
        path: String,
        source: std::io::Error,
    },

    #[error("failed to remove cache directory {path}: {source}")]
    RemoveDir {
        path: String,
        source: std::io::Error,
    },

    #[error("failed to query disk space for {path}: {source}")]
    DiskQuery {
        path: String,
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// Volume ID validation (SECURITY)
// ---------------------------------------------------------------------------

/// Validate that a volume ID is safe to use in a filesystem path.
///
/// Rejects any ID containing:
/// - Path separators (`/`, `\`)
/// - Parent-directory traversal (`..`)
/// - Null bytes
/// - Empty strings
/// - Strings that are exactly `.` or `..`
///
/// Valid volume IDs match the format `vol-{ULID}` — alphanumeric + hyphens only.
fn validate_volume_id(volume_id: &str) -> Result<(), CacheError> {
    if volume_id.is_empty() {
        return Err(CacheError::InvalidVolumeId {
            reason: "volume ID is empty".into(),
        });
    }

    if volume_id == "." || volume_id == ".." {
        return Err(CacheError::InvalidVolumeId {
            reason: format!("volume ID cannot be '{volume_id}'"),
        });
    }

    if volume_id.contains('/') || volume_id.contains('\\') {
        return Err(CacheError::InvalidVolumeId {
            reason: format!("volume ID contains path separator: '{volume_id}'"),
        });
    }

    if volume_id.contains("..") {
        return Err(CacheError::InvalidVolumeId {
            reason: format!("volume ID contains path traversal sequence: '{volume_id}'"),
        });
    }

    if volume_id.contains('\0') {
        return Err(CacheError::InvalidVolumeId {
            reason: "volume ID contains null byte".into(),
        });
    }

    // Enforce the expected format: only alphanumeric characters and hyphens.
    if !volume_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(CacheError::InvalidVolumeId {
            reason: format!(
                "volume ID contains invalid characters (expected alphanumeric + hyphens): '{volume_id}'"
            ),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate a cache disk path: check that it exists, is a directory, and
/// return disk space information.
///
/// # Errors
///
/// Returns [`CacheError::DiskNotFound`] if the path does not exist,
/// [`CacheError::NotADirectory`] if it is not a directory, or
/// [`CacheError::DiskQuery`] if statvfs fails.
pub fn validate_cache_disk(path: &Path) -> Result<CacheDiskInfo, CacheError> {
    if !path.exists() {
        return Err(CacheError::DiskNotFound {
            path: path.display().to_string(),
        });
    }

    if !path.is_dir() {
        return Err(CacheError::NotADirectory {
            path: path.display().to_string(),
        });
    }

    disk_space(path)
}

/// Create a per-volume cache directory at `{ssd_path}/{volume_id}/`.
///
/// Validates:
/// 1. `volume_id` does not contain path-traversal characters
/// 2. The SSD cache directory exists
/// 3. There is sufficient disk space for the configured cache size
///
/// Returns the path to the created directory.
pub fn create_volume_cache(
    config: &CacheConfig,
    volume_id: &str,
) -> Result<VolumeCacheDir, CacheError> {
    validate_volume_id(volume_id)?;

    // Validate the cache disk
    let disk_info = validate_cache_disk(&config.ssd_path)?;

    // Check sufficient space
    if disk_info.available_gb < u64::from(config.ssd_size_gb) {
        return Err(CacheError::InsufficientSpace {
            path: config.ssd_path.display().to_string(),
            required_gb: config.ssd_size_gb,
            available_gb: disk_info.available_gb,
        });
    }

    let cache_dir = config.ssd_path.join(volume_id);

    fs::create_dir_all(&cache_dir).map_err(|e| CacheError::CreateDir {
        path: cache_dir.display().to_string(),
        source: e,
    })?;

    // SECURITY: verify the resulting path is actually inside ssd_path.
    // canonicalize both paths and check containment.
    let canonical_parent = config
        .ssd_path
        .canonicalize()
        .map_err(|e| CacheError::CreateDir {
            path: config.ssd_path.display().to_string(),
            source: e,
        })?;
    let canonical_child = cache_dir
        .canonicalize()
        .map_err(|e| CacheError::CreateDir {
            path: cache_dir.display().to_string(),
            source: e,
        })?;

    if !canonical_child.starts_with(&canonical_parent) {
        // Clean up the directory we just created — it escaped the sandbox.
        let _ = fs::remove_dir(&cache_dir);
        return Err(CacheError::InvalidVolumeId {
            reason: format!(
                "resolved cache path escapes the SSD root: {}",
                canonical_child.display()
            ),
        });
    }

    Ok(VolumeCacheDir { path: cache_dir })
}

/// Remove a per-volume cache directory.
///
/// Validates the volume ID for path-traversal safety before constructing the
/// path. Returns `Ok(())` if the directory does not exist (idempotent).
pub fn cleanup_volume_cache(config: &CacheConfig, volume_id: &str) -> Result<(), CacheError> {
    validate_volume_id(volume_id)?;

    let cache_dir = config.ssd_path.join(volume_id);

    if !cache_dir.exists() {
        return Ok(());
    }

    fs::remove_dir_all(&cache_dir).map_err(|e| CacheError::RemoveDir {
        path: cache_dir.display().to_string(),
        source: e,
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// ZeroFS CLI argument builder
// ---------------------------------------------------------------------------

/// Build the ZeroFS cache-related CLI arguments.
///
/// Returns arguments suitable for passing to the `zerofs` binary:
/// ```text
/// --cache-dir {ssd_path}/{volume_id}/
/// --cache-size {ssd_size_gb}
/// --memory-size {memory_size_gb}
/// ```
///
/// # Errors
///
/// Returns [`CacheError::InvalidVolumeId`] if the volume ID is invalid.
pub fn zerofs_cache_args(config: &CacheConfig, volume_id: &str) -> Result<Vec<String>, CacheError> {
    validate_volume_id(volume_id)?;

    let cache_dir = config.ssd_path.join(volume_id);

    Ok(vec![
        "--cache-dir".to_string(),
        cache_dir.display().to_string(),
        "--cache-size".to_string(),
        config.ssd_size_gb.to_string(),
        "--memory-size".to_string(),
        config.memory_size_gb.to_string(),
    ])
}

// ---------------------------------------------------------------------------
// Internal: disk space query via statvfs
// ---------------------------------------------------------------------------

/// Query disk space using libc::statvfs.
fn disk_space(path: &Path) -> Result<CacheDiskInfo, CacheError> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let c_path =
        CString::new(path.to_string_lossy().as_bytes()).map_err(|_| CacheError::DiskQuery {
            path: path.display().to_string(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains null"),
        })?;

    let mut stat = MaybeUninit::<libc::statvfs>::uninit();

    // SAFETY: statvfs is a standard POSIX call. We pass a valid C string and an
    // uninitialised struct pointer. On success (return 0), the struct is fully
    // initialised.
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };

    if ret != 0 {
        return Err(CacheError::DiskQuery {
            path: path.display().to_string(),
            source: std::io::Error::last_os_error(),
        });
    }

    // SAFETY: statvfs returned 0, so the struct is initialised.
    let stat = unsafe { stat.assume_init() };

    // Cast through u64 explicitly to handle platforms where these fields
    // may be different sizes. The #[allow] suppresses the lint on platforms
    // where the fields are already u64.
    #[allow(clippy::unnecessary_cast)]
    let block_size = stat.f_frsize as u64;
    #[allow(clippy::unnecessary_cast)]
    let total_bytes = stat.f_blocks as u64 * block_size;
    #[allow(clippy::unnecessary_cast)]
    let available_bytes = stat.f_bavail as u64 * block_size;
    #[allow(clippy::unnecessary_cast)]
    let used_bytes = total_bytes.saturating_sub(stat.f_bfree as u64 * block_size);

    const GB: u64 = 1_073_741_824;

    Ok(CacheDiskInfo {
        total_gb: total_bytes / GB,
        available_gb: available_bytes / GB,
        used_gb: used_bytes / GB,
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Volume ID validation (security-critical)
    // -----------------------------------------------------------------------

    #[test]
    fn valid_volume_id() {
        assert!(validate_volume_id("vol-01JA0000000000000000000000").is_ok());
    }

    #[test]
    fn valid_volume_id_simple() {
        assert!(validate_volume_id("vol-abc123").is_ok());
    }

    #[test]
    fn rejects_empty_volume_id() {
        let err = validate_volume_id("").unwrap_err();
        assert!(err.to_string().contains("empty"), "error: {err}");
    }

    #[test]
    fn rejects_dot() {
        let err = validate_volume_id(".").unwrap_err();
        assert!(err.to_string().contains("cannot be '.'"), "error: {err}");
    }

    #[test]
    fn rejects_dotdot() {
        let err = validate_volume_id("..").unwrap_err();
        assert!(err.to_string().contains("cannot be '..'"), "error: {err}");
    }

    #[test]
    fn rejects_path_traversal_forward_slash() {
        let err = validate_volume_id("../../../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("path separator"), "error: {err}");
    }

    #[test]
    fn rejects_path_traversal_backslash() {
        let err = validate_volume_id("..\\..\\etc\\passwd").unwrap_err();
        assert!(err.to_string().contains("path separator"), "error: {err}");
    }

    #[test]
    fn rejects_embedded_dotdot() {
        let err = validate_volume_id("vol-..secret").unwrap_err();
        assert!(err.to_string().contains("path traversal"), "error: {err}");
    }

    #[test]
    fn rejects_null_byte() {
        let err = validate_volume_id("vol-abc\0xyz").unwrap_err();
        assert!(err.to_string().contains("null"), "error: {err}");
    }

    #[test]
    fn rejects_spaces() {
        let err = validate_volume_id("vol abc").unwrap_err();
        assert!(
            err.to_string().contains("invalid characters"),
            "error: {err}"
        );
    }

    #[test]
    fn rejects_special_chars() {
        for bad in &["vol;rm", "vol&bg", "vol|pipe", "vol$env", "vol`cmd`"] {
            let err = validate_volume_id(bad).unwrap_err();
            assert!(
                err.to_string().contains("invalid characters"),
                "should reject '{bad}': {err}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // CacheConfig serde roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn cache_config_serde_roundtrip() {
        let config = CacheConfig {
            ssd_path: PathBuf::from("/mnt/cache"),
            ssd_size_gb: 200,
            memory_size_gb: 8,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: CacheConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    // -----------------------------------------------------------------------
    // CacheDiskInfo serde roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn cache_disk_info_serde_roundtrip() {
        let info = CacheDiskInfo {
            total_gb: 500,
            available_gb: 350,
            used_gb: 150,
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: CacheDiskInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, parsed);
    }

    // -----------------------------------------------------------------------
    // validate_cache_disk
    // -----------------------------------------------------------------------

    #[test]
    fn validate_cache_disk_nonexistent() {
        let err = validate_cache_disk(Path::new("/nonexistent/cache/path")).unwrap_err();
        assert!(
            matches!(err, CacheError::DiskNotFound { .. }),
            "expected DiskNotFound, got: {err}"
        );
    }

    #[test]
    fn validate_cache_disk_not_a_directory() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let err = validate_cache_disk(tmp.path()).unwrap_err();
        assert!(
            matches!(err, CacheError::NotADirectory { .. }),
            "expected NotADirectory, got: {err}"
        );
    }

    #[test]
    fn validate_cache_disk_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = validate_cache_disk(tmp.path()).unwrap();
        // The temp dir is on a real filesystem, so we should get nonzero total.
        assert!(info.total_gb > 0, "total_gb should be > 0: {info:?}");
    }

    // -----------------------------------------------------------------------
    // create_volume_cache / cleanup_volume_cache
    // -----------------------------------------------------------------------

    #[test]
    fn create_and_cleanup_volume_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = CacheConfig {
            ssd_path: tmp.path().to_path_buf(),
            ssd_size_gb: 1, // small enough to fit on any disk
            memory_size_gb: 1,
        };

        // Create
        let vol_cache = create_volume_cache(&config, "vol-01JA0000000000000000000000").unwrap();
        assert!(vol_cache.path().exists());
        assert!(vol_cache.path().is_dir());
        assert!(vol_cache.path().ends_with("vol-01JA0000000000000000000000"));

        // Cleanup
        cleanup_volume_cache(&config, "vol-01JA0000000000000000000000").unwrap();
        assert!(!vol_cache.path().exists());
    }

    #[test]
    fn create_volume_cache_rejects_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = CacheConfig {
            ssd_path: tmp.path().to_path_buf(),
            ssd_size_gb: 1,
            memory_size_gb: 1,
        };

        let err = create_volume_cache(&config, "../escape").unwrap_err();
        assert!(
            matches!(err, CacheError::InvalidVolumeId { .. }),
            "expected InvalidVolumeId, got: {err}"
        );
    }

    #[test]
    fn cleanup_nonexistent_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = CacheConfig {
            ssd_path: tmp.path().to_path_buf(),
            ssd_size_gb: 1,
            memory_size_gb: 1,
        };

        // Should succeed even if the directory was never created
        cleanup_volume_cache(&config, "vol-nonexistent").unwrap();
    }

    #[test]
    fn cleanup_rejects_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = CacheConfig {
            ssd_path: tmp.path().to_path_buf(),
            ssd_size_gb: 1,
            memory_size_gb: 1,
        };

        let err = cleanup_volume_cache(&config, "../escape").unwrap_err();
        assert!(
            matches!(err, CacheError::InvalidVolumeId { .. }),
            "expected InvalidVolumeId, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // zerofs_cache_args
    // -----------------------------------------------------------------------

    #[test]
    fn zerofs_cache_args_happy_path() {
        let config = CacheConfig {
            ssd_path: PathBuf::from("/mnt/cache"),
            ssd_size_gb: 200,
            memory_size_gb: 8,
        };

        let args = zerofs_cache_args(&config, "vol-01JA0000000000000000000000").unwrap();
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], "--cache-dir");
        assert_eq!(args[1], "/mnt/cache/vol-01JA0000000000000000000000");
        assert_eq!(args[2], "--cache-size");
        assert_eq!(args[3], "200");
        assert_eq!(args[4], "--memory-size");
        assert_eq!(args[5], "8");
    }

    #[test]
    fn zerofs_cache_args_rejects_traversal() {
        let config = CacheConfig {
            ssd_path: PathBuf::from("/mnt/cache"),
            ssd_size_gb: 200,
            memory_size_gb: 8,
        };

        let err = zerofs_cache_args(&config, "../../etc").unwrap_err();
        assert!(
            matches!(err, CacheError::InvalidVolumeId { .. }),
            "expected InvalidVolumeId, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // disk_space (integration — runs on real filesystem)
    // -----------------------------------------------------------------------

    #[test]
    fn disk_space_on_tmp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = disk_space(tmp.path()).unwrap();
        assert!(info.total_gb > 0);
        assert!(info.available_gb <= info.total_gb);
    }

    #[test]
    fn disk_space_nonexistent() {
        let err = disk_space(Path::new("/nonexistent/path/for/test")).unwrap_err();
        assert!(
            matches!(err, CacheError::DiskQuery { .. }),
            "expected DiskQuery, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // CacheMetrics
    // -----------------------------------------------------------------------

    #[test]
    fn cache_metrics_default() {
        let m = CacheMetrics::default();
        assert_eq!(m.cache_hit_rate, 100.0);
        assert_eq!(m.dirty_bytes, 0);
        assert_eq!(m.cache_used_gb, 0.0);
        assert_eq!(m.eviction_rate, 0.0);
        assert_eq!(m.volumes_attached, 0);
        assert!(!m.s3_health);
    }

    #[test]
    fn cache_metrics_serde_roundtrip() {
        let m = CacheMetrics {
            cache_hit_rate: 92.5,
            dirty_bytes: 1_048_576,
            cache_used_gb: 45.2,
            eviction_rate: 3.1,
            volumes_attached: 4,
            s3_health: true,
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: CacheMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(m, parsed);
    }

    // -----------------------------------------------------------------------
    // CacheAlertThresholds + evaluate_alerts
    // -----------------------------------------------------------------------

    #[test]
    fn alert_thresholds_default() {
        let t = CacheAlertThresholds::default();
        assert_eq!(t.min_hit_rate_pct, 80.0);
        assert_eq!(t.max_dirty_ratio, 0.5);
    }

    #[test]
    fn evaluate_alerts_no_alerts_when_healthy() {
        let metrics = CacheMetrics {
            cache_hit_rate: 95.0,
            dirty_bytes: 100,
            ..Default::default()
        };
        let thresholds = CacheAlertThresholds::default();
        let alerts = evaluate_alerts(&metrics, &thresholds, 1000);
        assert!(alerts.is_empty());
    }

    #[test]
    fn evaluate_alerts_low_hit_rate() {
        let metrics = CacheMetrics {
            cache_hit_rate: 70.0,
            dirty_bytes: 0,
            ..Default::default()
        };
        let thresholds = CacheAlertThresholds::default();
        let alerts = evaluate_alerts(&metrics, &thresholds, 1000);
        assert_eq!(alerts.len(), 1);
        assert!(matches!(
            alerts[0],
            CacheAlert::LowHitRate {
                current_pct: 70,
                threshold_pct: 80,
            }
        ));
    }

    #[test]
    fn evaluate_alerts_high_dirty_ratio() {
        let metrics = CacheMetrics {
            cache_hit_rate: 95.0,
            dirty_bytes: 600,
            ..Default::default()
        };
        let thresholds = CacheAlertThresholds::default();
        let alerts = evaluate_alerts(&metrics, &thresholds, 1000);
        assert_eq!(alerts.len(), 1);
        assert!(matches!(
            alerts[0],
            CacheAlert::HighDirtyRatio {
                dirty_bytes: 600,
                cache_bytes: 1000,
            }
        ));
    }

    #[test]
    fn evaluate_alerts_both_triggered() {
        let metrics = CacheMetrics {
            cache_hit_rate: 50.0,
            dirty_bytes: 800,
            ..Default::default()
        };
        let thresholds = CacheAlertThresholds::default();
        let alerts = evaluate_alerts(&metrics, &thresholds, 1000);
        assert_eq!(alerts.len(), 2);
    }

    #[test]
    fn evaluate_alerts_zero_cache_no_dirty_alert() {
        let metrics = CacheMetrics {
            cache_hit_rate: 50.0,
            dirty_bytes: 100,
            ..Default::default()
        };
        let thresholds = CacheAlertThresholds::default();
        // zero total cache bytes -> no dirty ratio alert
        let alerts = evaluate_alerts(&metrics, &thresholds, 0);
        assert_eq!(alerts.len(), 1); // only hit rate alert
    }

    #[test]
    fn cache_alert_display() {
        let a = CacheAlert::LowHitRate {
            current_pct: 65,
            threshold_pct: 80,
        };
        assert!(a.to_string().contains("65%"));
        assert!(a.to_string().contains("80%"));

        let b = CacheAlert::HighDirtyRatio {
            dirty_bytes: 700,
            cache_bytes: 1000,
        };
        assert!(b.to_string().contains("700"));
        assert!(b.to_string().contains("70%"));
    }

    // -----------------------------------------------------------------------
    // CachePrewarmConfig
    // -----------------------------------------------------------------------

    #[test]
    fn prewarm_config_default() {
        let cfg = CachePrewarmConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.bandwidth_mb, 100);
    }

    #[test]
    fn prewarm_config_serde_roundtrip() {
        let cfg = CachePrewarmConfig {
            enabled: false,
            bandwidth_mb: 50,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: CachePrewarmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, parsed);
    }

    // -----------------------------------------------------------------------
    // CachePrewarmProgress
    // -----------------------------------------------------------------------

    #[test]
    fn prewarm_progress_new_starts_at_zero() {
        let p = CachePrewarmProgress::new("vol-123", 1_000_000);
        assert_eq!(p.volume_id, "vol-123");
        assert_eq!(p.total_bytes, 1_000_000);
        assert_eq!(p.warmed_bytes, 0);
        assert_eq!(p.percent, 0.0);
        assert!(!p.done);
    }

    #[test]
    fn prewarm_progress_completed() {
        let p = CachePrewarmProgress::new("vol-123", 1_000_000).completed();
        assert_eq!(p.warmed_bytes, 1_000_000);
        assert_eq!(p.percent, 100.0);
        assert_eq!(p.eta_secs, Some(0));
        assert!(p.done);
    }

    #[test]
    fn prewarm_progress_serde_roundtrip() {
        let p = CachePrewarmProgress {
            volume_id: "vol-abc".to_string(),
            total_bytes: 5_000_000,
            warmed_bytes: 2_500_000,
            percent: 50.0,
            eta_secs: Some(30),
            done: false,
        };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: CachePrewarmProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(p, parsed);
    }

    // -----------------------------------------------------------------------
    // CachePrewarmer — integration test with temp directory
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn prewarm_warms_temp_files() {
        use std::io::Write;

        let tmp = tempfile::TempDir::new().unwrap();
        // Create a small test file (4 KiB).
        let file_path = tmp.path().join("testfile.bin");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(&[0xAB; 4096]).unwrap();
        drop(f);

        let config = CachePrewarmConfig {
            enabled: true,
            bandwidth_mb: 1000, // fast for test
        };

        let handle = CachePrewarmer::start("vol-test", tmp.path(), &config);

        // Wait for completion (should be near-instant for 4 KiB).
        for _ in 0..100 {
            let progress = handle.progress();
            if progress.done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let final_progress = handle.progress();
        assert!(final_progress.done, "warmup should have completed");
        assert_eq!(final_progress.percent, 100.0);
        assert_eq!(final_progress.total_bytes, 4096);
        assert_eq!(final_progress.warmed_bytes, 4096);
    }

    #[tokio::test]
    async fn prewarm_cancel_stops_early() {
        use std::io::Write;

        let tmp = tempfile::TempDir::new().unwrap();
        // Create a larger file so we have time to cancel.
        let file_path = tmp.path().join("largefile.bin");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(&[0xCD; 1_048_576]).unwrap(); // 1 MiB
        drop(f);

        let config = CachePrewarmConfig {
            enabled: true,
            bandwidth_mb: 1, // very slow — 1 MB/s means ~1s for 1 MiB
        };

        let handle = CachePrewarmer::start("vol-cancel", tmp.path(), &config);

        // Cancel immediately.
        handle.cancel();

        // Wait for the task to notice the cancellation.
        for _ in 0..50 {
            let progress = handle.progress();
            if progress.done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let final_progress = handle.progress();
        assert!(
            final_progress.done,
            "warmup should be marked done after cancel"
        );
    }

    #[tokio::test]
    async fn prewarm_empty_directory_completes_immediately() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = CachePrewarmConfig::default();
        let handle = CachePrewarmer::start("vol-empty", tmp.path(), &config);

        // Should complete very quickly.
        for _ in 0..50 {
            if handle.progress().done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let p = handle.progress();
        assert!(p.done);
        assert_eq!(p.total_bytes, 0);
    }

    // -----------------------------------------------------------------------
    // collect_files / dir_size_bytes helpers
    // -----------------------------------------------------------------------

    #[test]
    fn collect_files_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(collect_files(tmp.path()).is_empty());
    }

    #[test]
    fn collect_files_nested() {
        use std::io::Write;

        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        let mut f1 = std::fs::File::create(tmp.path().join("a.txt")).unwrap();
        f1.write_all(b"hello").unwrap();
        let mut f2 = std::fs::File::create(sub.join("b.txt")).unwrap();
        f2.write_all(b"world").unwrap();

        let files = collect_files(tmp.path());
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn dir_size_bytes_counts_correctly() {
        use std::io::Write;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut f = std::fs::File::create(tmp.path().join("data.bin")).unwrap();
        f.write_all(&[0u8; 1024]).unwrap();
        drop(f);

        assert_eq!(dir_size_bytes(tmp.path()), 1024);
    }
}
