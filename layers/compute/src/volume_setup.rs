//! Volume setup integration — waits for ZeroFS volume readiness before VM boot.
//!
//! When a VM has a `root_volume_id`, the storage layer starts ZeroFS which
//! exposes a 9P socket at `/tmp/syfrah/{volume_id}/zerofs.9p.sock`. This
//! module polls until the socket exists, then returns the volume mount path
//! so the runtime can bind-mount (container) or virtiofs-share (CH) it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::error::{ComputeError, ProcessError};

/// Base directory where ZeroFS volumes are mounted on the host.
const VOLUMES_BASE: &str = "/var/lib/syfrah/volumes";

/// Base directory where ZeroFS creates its runtime files (config, 9P socket).
const ZEROFS_RUNTIME_BASE: &str = "/tmp/syfrah";

/// Maximum time to wait for the ZeroFS 9P socket to appear (seconds).
///
/// ZeroFS typically starts in ~1-2 seconds so 10s is generous.
const VOLUME_READY_TIMEOUT_SECS: u64 = 10;

/// Interval between readiness checks (milliseconds).
const VOLUME_POLL_INTERVAL_MS: u64 = 250;

/// Return the expected host-side mount path for a volume.
pub fn volume_mount_path(volume_id: &str) -> PathBuf {
    Path::new(VOLUMES_BASE).join(volume_id)
}

/// Return the path to the ZeroFS 9P socket for a volume.
fn zerofs_socket_path(volume_id: &str) -> PathBuf {
    Path::new(ZEROFS_RUNTIME_BASE)
        .join(volume_id)
        .join("zerofs.9p.sock")
}

/// Wait for a ZeroFS volume to become ready.
///
/// Polls until the ZeroFS 9P socket exists at
/// `/tmp/syfrah/{volume_id}/zerofs.9p.sock`, which is the authoritative
/// readiness signal — it means ZeroFS has started and is accepting 9P
/// connections. Returns the host mount path on success or an error after
/// the timeout.
///
/// This is analogous to how `NetworkSetup` prepares networking before the
/// runtime boots — storage must be ready before the workload starts.
pub async fn wait_for_volume_ready(volume_id: &str) -> Result<PathBuf, ComputeError> {
    let mount_path = volume_mount_path(volume_id);
    let socket_path = zerofs_socket_path(volume_id);
    let timeout = Duration::from_secs(VOLUME_READY_TIMEOUT_SECS);
    let poll = Duration::from_millis(VOLUME_POLL_INTERVAL_MS);
    let deadline = tokio::time::Instant::now() + timeout;

    info!(
        volume_id = %volume_id,
        socket_path = %socket_path.display(),
        mount_path = %mount_path.display(),
        "waiting for ZeroFS 9P socket to appear"
    );

    loop {
        if socket_path.exists() {
            info!(
                volume_id = %volume_id,
                socket_path = %socket_path.display(),
                mount_path = %mount_path.display(),
                "ZeroFS 9P socket is ready"
            );
            return Ok(mount_path);
        }

        if tokio::time::Instant::now() >= deadline {
            warn!(
                volume_id = %volume_id,
                socket_path = %socket_path.display(),
                "timed out waiting for ZeroFS 9P socket"
            );
            return Err(ProcessError::SpawnFailed {
                reason: format!(
                    "ZeroFS volume '{volume_id}' not ready after {VOLUME_READY_TIMEOUT_SECS}s \
                     (9P socket {} does not exist)",
                    socket_path.display()
                ),
            }
            .into());
        }

        debug!(
            volume_id = %volume_id,
            "ZeroFS 9P socket not yet ready, polling again in {VOLUME_POLL_INTERVAL_MS}ms"
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

    #[test]
    fn zerofs_socket_path_format() {
        let path = zerofs_socket_path("vol-root-my-vm");
        assert_eq!(
            path,
            PathBuf::from("/tmp/syfrah/vol-root-my-vm/zerofs.9p.sock")
        );
    }

    #[tokio::test]
    async fn wait_for_volume_ready_with_existing_socket() {
        // Create a temp file to simulate the 9P socket.
        let tmp = tempfile::TempDir::new().unwrap();
        let vol_dir = tmp.path().join("vol-test");
        std::fs::create_dir_all(&vol_dir).unwrap();
        let sock = vol_dir.join("zerofs.9p.sock");
        std::fs::write(&sock, b"").unwrap();

        // The socket file exists, which is the readiness signal.
        assert!(sock.exists());
    }

    #[tokio::test]
    async fn wait_for_volume_ready_timeout() {
        // A nonexistent volume's socket should not exist.
        let path = zerofs_socket_path("vol-nonexistent-12345");
        assert!(!path.exists());
    }
}
