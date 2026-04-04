//! Volume manager — start/stop ZeroFS processes per volume.
//!
//! `VolumeMgr` is the runtime process supervisor for volumes on a single node.
//! Each volume is backed by one ZeroFS child process that exposes an NBD device.
//!
//! ## Lifecycle
//!
//! 1. `start_volume` resolves the ZeroFS binary, spawns the process, waits
//!    for the NBD device to appear, and tracks the child PID.
//! 2. `stop_volume` sends SIGTERM, waits up to a grace period, then SIGKILLs
//!    if the process is still running.
//! 3. Background reaping: callers should poll `reap_exited` periodically to
//!    detect crashed processes and update internal state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time;

use crate::binary;

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

/// Tracked state for a running ZeroFS process.
struct VolumeProcess {
    child: Child,
    #[allow(dead_code)]
    nbd_device: PathBuf,
    #[allow(dead_code)]
    generation: u64,
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

    #[error("zerofs binary: {0}")]
    Binary(#[from] binary::ZerofsError),

    #[error("spawn failed: {0}")]
    Spawn(String),

    #[error("nbd device did not appear within {0:?}")]
    NbdTimeout(Duration),

    #[error("stop failed: {0}")]
    Stop(String),
}

// ---------------------------------------------------------------------------
// VolumeMgr
// ---------------------------------------------------------------------------

/// Default timeout waiting for the NBD device to appear after spawning ZeroFS.
const NBD_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval when waiting for the NBD device.
const NBD_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Grace period before SIGKILL on stop.
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

/// Manages ZeroFS child processes — one per volume.
pub struct VolumeMgr {
    /// Active ZeroFS processes keyed by volume_id.
    processes: HashMap<String, VolumeProcess>,
    /// Optional explicit path to the zerofs binary.
    binary_override: Option<PathBuf>,
    /// Base NBD device path (e.g. `/dev/nbd`). Devices are `{base}{N}`.
    nbd_base: PathBuf,
    /// Next NBD device index to allocate.
    next_nbd_index: u32,
}

impl VolumeMgr {
    /// Create a new `VolumeMgr`.
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            binary_override: None,
            nbd_base: PathBuf::from("/dev/nbd"),
            next_nbd_index: 0,
        }
    }

    /// Set an explicit zerofs binary path (overrides resolution order).
    pub fn with_binary(mut self, path: PathBuf) -> Self {
        self.binary_override = Some(path);
        self
    }

    /// Start a ZeroFS process for `volume_id`.
    ///
    /// The process is spawned with the given S3 + cache configuration and
    /// generation prefix `volumes/{volume_id}/gen-{generation}/`.
    pub async fn start_volume(
        &mut self,
        volume_id: &str,
        s3: &S3Config,
        cache: &CacheConfig,
        encryption_passphrase: &str,
        generation: u64,
    ) -> Result<PathBuf, VolumeMgrError> {
        if self.processes.contains_key(volume_id) {
            return Err(VolumeMgrError::AlreadyRunning(volume_id.to_string()));
        }

        let binary_path = binary::resolve_binary(self.binary_override.as_deref())?;

        let nbd_device = self.allocate_nbd_device();
        let prefix = format!("volumes/{volume_id}/gen-{generation}/");

        let mut child = Command::new(&binary_path)
            .arg("--s3-endpoint")
            .arg(&s3.endpoint)
            .arg("--s3-bucket")
            .arg(&s3.bucket)
            .arg("--s3-access-key")
            .arg(&s3.access_key)
            .arg("--prefix")
            .arg(&prefix)
            .arg("--cache-dir")
            .arg(&cache.disk_path)
            .arg("--cache-size")
            .arg(cache.disk_size_bytes.to_string())
            .arg("--memory-size")
            .arg(cache.memory_size_bytes.to_string())
            .arg("--nbd-device")
            .arg(&nbd_device)
            // Pass secrets via environment variables instead of CLI args
            // to avoid exposure in /proc/pid/cmdline.
            .env("ZEROFS_S3_SECRET_KEY", &s3.secret_key)
            .env("ZEROFS_ENCRYPTION_KEY", encryption_passphrase)
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| VolumeMgrError::Spawn(e.to_string()))?;

        // Wait for NBD device to appear. If it times out, kill the orphaned
        // child process before returning the error.
        if let Err(e) = self.wait_for_nbd(&nbd_device).await {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(e);
        }

        self.processes.insert(
            volume_id.to_string(),
            VolumeProcess {
                child,
                nbd_device: nbd_device.clone(),
                generation,
            },
        );

        Ok(nbd_device)
    }

    /// Stop a running ZeroFS process for `volume_id`.
    ///
    /// Sends SIGTERM and waits up to `STOP_GRACE_PERIOD`. If the process
    /// does not exit, it is killed with SIGKILL.
    pub async fn stop_volume(&mut self, volume_id: &str) -> Result<(), VolumeMgrError> {
        let mut proc = self
            .processes
            .remove(volume_id)
            .ok_or_else(|| VolumeMgrError::NotRunning(volume_id.to_string()))?;

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
        match time::timeout(STOP_GRACE_PERIOD, proc.child.wait()).await {
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
        }
    }

    /// Returns `true` if the volume has a tracked running process.
    pub fn is_running(&self, volume_id: &str) -> bool {
        self.processes.contains_key(volume_id)
    }

    /// Get the NBD device path for a running volume.
    ///
    /// Returns `None` if the volume is not tracked.
    pub fn get_nbd_device(&self, volume_id: &str) -> Option<PathBuf> {
        self.processes.get(volume_id).map(|p| p.nbd_device.clone())
    }

    /// List all actively tracked volumes as `(volume_id, generation)` pairs.
    pub fn list_active(&self) -> Vec<(String, u64)> {
        self.processes
            .iter()
            .map(|(id, proc)| (id.clone(), proc.generation))
            .collect()
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

    /// Allocate the next NBD device path.
    fn allocate_nbd_device(&mut self) -> PathBuf {
        let idx = self.next_nbd_index;
        self.next_nbd_index += 1;
        PathBuf::from(format!("{}{idx}", self.nbd_base.display()))
    }

    /// Wait for an NBD device file to appear on disk.
    async fn wait_for_nbd(&self, path: &Path) -> Result<(), VolumeMgrError> {
        let deadline = time::Instant::now() + NBD_WAIT_TIMEOUT;
        while time::Instant::now() < deadline {
            if path.exists() {
                return Ok(());
            }
            time::sleep(NBD_POLL_INTERVAL).await;
        }
        Err(VolumeMgrError::NbdTimeout(NBD_WAIT_TIMEOUT))
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
        let nbd_device = self.allocate_nbd_device();
        self.processes.insert(
            volume_id.to_string(),
            VolumeProcess {
                child,
                nbd_device,
                generation,
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
                nbd_device: PathBuf::from("/dev/nbd50"),
                generation: 42,
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

    #[test]
    fn allocate_nbd_devices_increments() {
        let mut mgr = VolumeMgr::new();
        assert_eq!(mgr.allocate_nbd_device(), PathBuf::from("/dev/nbd0"));
        assert_eq!(mgr.allocate_nbd_device(), PathBuf::from("/dev/nbd1"));
        assert_eq!(mgr.allocate_nbd_device(), PathBuf::from("/dev/nbd2"));
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
                nbd_device: PathBuf::from("/dev/nbd99"),
                generation: 1,
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
                nbd_device: PathBuf::from("/dev/nbd98"),
                generation: 1,
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
                nbd_device: PathBuf::from("/dev/nbd97"),
                generation: 1,
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
                nbd_device: PathBuf::from("/dev/nbd96"),
                generation: 1,
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

    #[test]
    fn get_nbd_device_returns_none_for_unknown() {
        let mgr = VolumeMgr::new();
        assert!(mgr.get_nbd_device("nonexistent").is_none());
    }

    #[tokio::test]
    async fn get_nbd_device_returns_path_for_tracked() {
        let mut mgr = VolumeMgr::new();
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        mgr.processes.insert(
            "vol-nbd".to_string(),
            VolumeProcess {
                child,
                nbd_device: PathBuf::from("/dev/nbd42"),
                generation: 1,
            },
        );

        let nbd = mgr.get_nbd_device("vol-nbd");
        assert_eq!(nbd, Some(PathBuf::from("/dev/nbd42")));

        mgr.stop_volume("vol-nbd").await.ok();
    }
}
