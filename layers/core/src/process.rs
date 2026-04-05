//! Process management — daemon fork, PID file, signal handling.
//!
//! ```no_run
//! use syfrah_core::process;
//!
//! // Write PID file
//! process::write_pid_file(&process::pid_path()).unwrap();
//!
//! // Check if daemon is running
//! if let Some(pid) = process::read_pid_file(&process::pid_path()) {
//!     if process::is_running(pid) {
//!         println!("daemon is running (pid {pid})");
//!     }
//! }
//!
//! // Stop daemon
//! process::stop_daemon(&process::pid_path()).unwrap();
//! ```

use std::path::{Path, PathBuf};

use crate::error::SyfrahError;

/// Default PID file path.
pub fn pid_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
        .join("daemon.pid")
}

/// Default syfrah directory.
pub fn syfrah_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
}

/// Write the current process PID to a file.
pub fn write_pid_file(path: &Path) -> Result<(), SyfrahError> {
    let pid = std::process::id();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(SyfrahError::from)?;
    }
    std::fs::write(path, pid.to_string()).map_err(SyfrahError::from)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Read a PID from a PID file. Returns None if file doesn't exist or is invalid.
pub fn read_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Check if a process with the given PID is running.
#[cfg(unix)]
pub fn is_running(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn is_running(_pid: u32) -> bool {
    false
}

/// Remove a PID file.
pub fn remove_pid_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Stop the daemon by sending SIGTERM to the PID in the PID file.
#[cfg(unix)]
pub fn stop_daemon(pid_path: &Path) -> Result<(), SyfrahError> {
    let pid = read_pid_file(pid_path).ok_or_else(SyfrahError::daemon_unreachable)?;

    if !is_running(pid) {
        remove_pid_file(pid_path);
        return Err(SyfrahError::new(
            crate::error::ErrorCode::DaemonUnreachable,
            format!("daemon not running (stale PID {pid})"),
        ));
    }

    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    // Wait up to 5 seconds for process to exit
    for _ in 0..50 {
        if !is_running(pid) {
            remove_pid_file(pid_path);
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Err(SyfrahError::timeout("stop daemon", 5))
}

#[cfg(not(unix))]
pub fn stop_daemon(pid_path: &Path) -> Result<(), SyfrahError> {
    Err(SyfrahError::not_implemented("stop_daemon on non-unix"))
}

/// Fork the current process into background (Unix only).
/// Returns the child PID to the parent, or continues as the child.
#[cfg(unix)]
pub fn daemonize() -> Result<DaemonResult, SyfrahError> {
    match unsafe { libc::fork() } {
        -1 => Err(SyfrahError::internal("fork failed")),
        0 => {
            // Child — become session leader
            unsafe { libc::setsid() };
            Ok(DaemonResult::Child)
        }
        pid => Ok(DaemonResult::Parent(pid as u32)),
    }
}

#[cfg(not(unix))]
pub fn daemonize() -> Result<DaemonResult, SyfrahError> {
    Ok(DaemonResult::Child) // no fork on non-unix, run in foreground
}

/// Result of daemonize().
pub enum DaemonResult {
    /// We are the parent — child PID is returned.
    Parent(u32),
    /// We are the child — continue as daemon.
    Child,
}

/// Wait for a signal (SIGTERM or SIGINT). Used in daemon main loops.
pub async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT, shutting down");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_path_in_syfrah_dir() {
        let p = pid_path();
        assert!(p.to_str().unwrap().contains(".syfrah/daemon.pid"));
    }

    #[test]
    fn syfrah_dir_exists() {
        let d = syfrah_dir();
        assert!(d.to_str().unwrap().contains(".syfrah"));
    }

    #[test]
    fn write_and_read_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");

        write_pid_file(&path).unwrap();
        let pid = read_pid_file(&path).unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn read_missing_pid() {
        assert_eq!(read_pid_file(Path::new("/nonexistent/pid")), None);
    }

    #[test]
    fn read_invalid_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pid");
        std::fs::write(&path, "not-a-number").unwrap();
        assert_eq!(read_pid_file(&path), None);
    }

    #[test]
    fn is_running_current() {
        assert!(is_running(std::process::id()));
    }

    #[test]
    fn is_running_nonexistent() {
        // PID 999999999 should not exist
        assert!(!is_running(999_999_999));
    }

    #[test]
    fn remove_pid_file_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");
        std::fs::write(&path, "123").unwrap();
        remove_pid_file(&path);
        assert!(!path.exists());
    }

    #[test]
    fn remove_pid_file_nonexistent() {
        remove_pid_file(Path::new("/nonexistent/pid")); // no panic
    }
}
