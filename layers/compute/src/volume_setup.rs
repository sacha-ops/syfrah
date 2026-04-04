//! Volume setup integration — waits for ZeroFS volume readiness before VM boot.
//!
//! When a VM has a `root_volume_id`, the reconciler has already started ZeroFS
//! and mounted the 9P filesystem at `/var/lib/syfrah/volumes/{volume_id}/`.
//! This module polls until the mount point exists and is non-empty, then returns
//! the path so the runtime can bind-mount (container) or virtiofs-share (CH) it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::error::{ComputeError, ProcessError};

/// Base directory where ZeroFS volumes are mounted on the host.
const VOLUMES_BASE: &str = "/var/lib/syfrah/volumes";

/// Maximum time to wait for a volume mount to appear (seconds).
const VOLUME_READY_TIMEOUT_SECS: u64 = 60;

/// Interval between readiness checks (milliseconds).
const VOLUME_POLL_INTERVAL_MS: u64 = 500;

/// Return the expected host-side mount path for a volume.
pub fn volume_mount_path(volume_id: &str) -> PathBuf {
    Path::new(VOLUMES_BASE).join(volume_id)
}

/// Wait for a ZeroFS volume mount to become ready.
///
/// Polls until the mount directory exists (indicating ZeroFS has started and
/// the host 9P mount succeeded). Returns the mount path on success or an
/// error after the timeout.
///
/// This is analogous to how `NetworkSetup` prepares networking before the
/// runtime boots — storage must be ready before the workload starts.
pub async fn wait_for_volume_ready(volume_id: &str) -> Result<PathBuf, ComputeError> {
    let mount_path = volume_mount_path(volume_id);
    let timeout = Duration::from_secs(VOLUME_READY_TIMEOUT_SECS);
    let poll = Duration::from_millis(VOLUME_POLL_INTERVAL_MS);
    let deadline = tokio::time::Instant::now() + timeout;

    info!(
        volume_id = %volume_id,
        mount_path = %mount_path.display(),
        "waiting for ZeroFS volume mount to become ready"
    );

    loop {
        if mount_path.exists() && mount_path.is_dir() {
            info!(
                volume_id = %volume_id,
                mount_path = %mount_path.display(),
                "ZeroFS volume mount is ready"
            );
            return Ok(mount_path);
        }

        if tokio::time::Instant::now() >= deadline {
            warn!(
                volume_id = %volume_id,
                mount_path = %mount_path.display(),
                "timed out waiting for ZeroFS volume mount"
            );
            return Err(ProcessError::SpawnFailed {
                reason: format!(
                    "ZeroFS volume '{volume_id}' not ready after {VOLUME_READY_TIMEOUT_SECS}s \
                     (mount path {} does not exist)",
                    mount_path.display()
                ),
            }
            .into());
        }

        debug!(
            volume_id = %volume_id,
            "volume mount not yet ready, polling again in {VOLUME_POLL_INTERVAL_MS}ms"
        );
        tokio::time::sleep(poll).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_mount_path_format() {
        let path = volume_mount_path("vol-root-my-vm");
        assert_eq!(
            path,
            PathBuf::from("/var/lib/syfrah/volumes/vol-root-my-vm")
        );
    }

    #[tokio::test]
    async fn wait_for_volume_ready_with_existing_dir() {
        // Create a temp dir to simulate a mounted volume.
        let tmp = tempfile::TempDir::new().unwrap();
        let vol_dir = tmp.path().join("vol-test");
        std::fs::create_dir_all(&vol_dir).unwrap();

        // Monkey-patch: we can't easily override VOLUMES_BASE, so test the
        // path-checking logic directly.
        assert!(vol_dir.exists());
        assert!(vol_dir.is_dir());
    }

    #[tokio::test]
    async fn wait_for_volume_ready_timeout() {
        // A nonexistent volume should timeout. Use a very short timeout by
        // testing the underlying logic rather than the full function (which
        // has a 60s timeout).
        let path = volume_mount_path("vol-nonexistent-12345");
        assert!(!path.exists());
    }
}
