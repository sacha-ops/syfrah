//! Process management — daemon lifecycle, PID file, signals, lock file.
//!
//! # Features
//!
//! - **Double-fork** daemonize (proper Unix daemon pattern)
//! - **Lock file** (flock) prevents multiple daemon instances
//! - **PID file** with atomic write
//! - **FD cleanup** — close stdin/stdout/stderr, redirect to /dev/null
//! - **Health check** — wait for daemon to be ready after start
//! - **Re-exec** — replace running binary for zero-downtime updates
//! - **Graceful shutdown** — SIGTERM/SIGINT handling
//!
//! ```no_run
//! use syfrah_core::process;
//!
//! process::write_pid_file(&process::pid_path()).unwrap();
//!
//! if process::is_daemon_running() {
//!     println!("daemon is running");
//! }
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::SyfrahError;

// ═══════════════════════════════════════════════════
// Paths
// ═══════════════════════════════════════════════════

/// Default syfrah directory.
pub fn syfrah_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
}

/// Default PID file path.
pub fn pid_path() -> PathBuf {
    syfrah_dir().join("daemon.pid")
}

/// Default lock file path.
pub fn lock_path() -> PathBuf {
    syfrah_dir().join("daemon.lock")
}

/// Default control socket path.
pub fn socket_path() -> PathBuf {
    syfrah_dir().join("control.sock")
}

/// Ensure ~/.syfrah exists with 0o700 permissions.
pub fn ensure_syfrah_dir() -> Result<(), SyfrahError> {
    let dir = syfrah_dir();
    std::fs::create_dir_all(&dir).map_err(SyfrahError::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

// ═══════════════════════════════════════════════════
// PID file
// ═══════════════════════════════════════════════════

/// Write the current process PID to a file atomically.
pub fn write_pid_file(path: &Path) -> Result<(), SyfrahError> {
    ensure_syfrah_dir()?;
    let pid = std::process::id();
    // Write to temp file then rename (atomic on same filesystem)
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, pid.to_string()).map_err(SyfrahError::from)?;
    std::fs::rename(&tmp, path).map_err(SyfrahError::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Read a PID from a PID file.
pub fn read_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Remove a PID file.
pub fn remove_pid_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

// ═══════════════════════════════════════════════════
// 7. Lock file — prevent multiple instances
// ═══════════════════════════════════════════════════

/// Acquire an exclusive lock on the lock file.
/// Returns the file handle (must be held for daemon lifetime).
#[cfg(unix)]
pub fn acquire_lock(path: &Path) -> Result<std::fs::File, SyfrahError> {
    use std::os::unix::io::AsRawFd;

    ensure_syfrah_dir()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(SyfrahError::from)?;

    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        return Err(SyfrahError::conflict(
            "daemon",
            "syfrah",
            "another daemon instance is already running",
        ));
    }

    // Write PID into lock file
    use std::io::Write;
    let mut f = &file;
    let _ = write!(f, "{}", std::process::id());

    Ok(file)
}

#[cfg(not(unix))]
pub fn acquire_lock(path: &Path) -> Result<std::fs::File, SyfrahError> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .map_err(SyfrahError::from)
}

// ═══════════════════════════════════════════════════
// Process status
// ═══════════════════════════════════════════════════

/// Check if a process with the given PID is running.
#[cfg(unix)]
pub fn is_running(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn is_running(_pid: u32) -> bool {
    false
}

/// Check if the daemon is running (reads PID file + checks process).
pub fn is_daemon_running() -> bool {
    read_pid_file(&pid_path()).map(is_running).unwrap_or(false)
}

// ═══════════════════════════════════════════════════
// Stop daemon
// ═══════════════════════════════════════════════════

/// Stop the daemon by sending SIGTERM and waiting for exit.
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

    // Wait up to 10 seconds
    for _ in 0..100 {
        if !is_running(pid) {
            remove_pid_file(pid_path);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Force kill
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(200));
    remove_pid_file(pid_path);

    Ok(())
}

#[cfg(not(unix))]
pub fn stop_daemon(_pid_path: &Path) -> Result<(), SyfrahError> {
    Err(SyfrahError::not_implemented("stop_daemon on non-unix"))
}

// ═══════════════════════════════════════════════════
// 2. Double-fork daemonize
// ═══════════════════════════════════════════════════

/// Result of daemonize().
pub enum DaemonResult {
    /// We are the parent — child PID is returned.
    Parent(u32),
    /// We are the child daemon — continue running.
    Child,
}

/// Proper double-fork daemonize:
/// 1. First fork — parent exits
/// 2. setsid — become session leader
/// 3. Second fork — detach from terminal completely
/// 4. Close stdin/stdout/stderr, redirect to /dev/null
/// 5. chdir to /
#[cfg(unix)]
pub fn daemonize() -> Result<DaemonResult, SyfrahError> {
    // First fork
    match unsafe { libc::fork() } {
        -1 => return Err(SyfrahError::internal("first fork failed")),
        0 => {} // child continues
        _pid => return Ok(DaemonResult::Parent(_pid as u32)),
    }

    // Become session leader
    if unsafe { libc::setsid() } == -1 {
        return Err(SyfrahError::internal("setsid failed"));
    }

    // #3: Change working directory to /
    let _ = std::env::set_current_dir("/");

    // Second fork — fully detach
    match unsafe { libc::fork() } {
        -1 => return Err(SyfrahError::internal("second fork failed")),
        0 => {} // grandchild continues as daemon
        _pid => {
            // Intermediate child exits
            std::process::exit(0);
        }
    }

    // #4: Close and redirect standard file descriptors
    redirect_stdio();

    Ok(DaemonResult::Child)
}

#[cfg(not(unix))]
pub fn daemonize() -> Result<DaemonResult, SyfrahError> {
    Ok(DaemonResult::Child) // no fork on non-unix
}

/// #4: Redirect stdin/stdout/stderr to /dev/null.
#[cfg(unix)]
fn redirect_stdio() {
    let devnull: std::os::unix::io::RawFd =
        unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
    if devnull >= 0 {
        unsafe {
            libc::dup2(devnull, 0); // stdin
            libc::dup2(devnull, 1); // stdout
            libc::dup2(devnull, 2); // stderr
            if devnull > 2 {
                libc::close(devnull);
            }
        }
    }
}

// ═══════════════════════════════════════════════════
// 5. Re-exec — replace running binary
// ═══════════════════════════════════════════════════

/// Re-exec the current binary (for zero-downtime updates).
/// Replaces the running process with a fresh load of the binary on disk.
#[cfg(unix)]
pub fn re_exec() -> Result<(), SyfrahError> {
    let exe = std::env::current_exe()
        .map_err(|e| SyfrahError::internal(format!("failed to get current exe path: {e}")))?;

    let args: Vec<std::ffi::CString> = std::env::args()
        .map(|a| std::ffi::CString::new(a).unwrap())
        .collect();

    let exe_c = std::ffi::CString::new(exe.to_str().unwrap_or("syfrah"))
        .map_err(|e| SyfrahError::internal(format!("invalid exe path: {e}")))?;

    // execv replaces the current process
    nix::unistd::execv(&exe_c, &args)
        .map_err(|e| SyfrahError::internal(format!("re-exec failed: {e}")))?;

    unreachable!()
}

#[cfg(not(unix))]
pub fn re_exec() -> Result<(), SyfrahError> {
    Err(SyfrahError::not_implemented("re-exec on non-unix"))
}

// ═══════════════════════════════════════════════════
// 6. Health check — wait for daemon ready
// ═══════════════════════════════════════════════════

/// Wait for the daemon to become ready (control socket exists + responds).
/// Returns the daemon PID if healthy, error if timeout.
pub fn wait_for_healthy(timeout: Duration) -> Result<u32, SyfrahError> {
    let start = std::time::Instant::now();
    let sock = socket_path();
    let pid_file = pid_path();

    loop {
        if start.elapsed() > timeout {
            return Err(SyfrahError::timeout(
                "waiting for daemon",
                timeout.as_secs(),
            ));
        }

        // Check PID file exists and process is running
        if let Some(pid) = read_pid_file(&pid_file) {
            if is_running(pid) && sock.exists() {
                return Ok(pid);
            }
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

// ═══════════════════════════════════════════════════
// Graceful shutdown signal
// ═══════════════════════════════════════════════════

/// Wait for SIGTERM or SIGINT. Used in daemon main loops.
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
        assert!(pid_path().to_str().unwrap().contains(".syfrah/daemon.pid"));
    }

    #[test]
    fn lock_path_in_syfrah_dir() {
        assert!(lock_path()
            .to_str()
            .unwrap()
            .contains(".syfrah/daemon.lock"));
    }

    #[test]
    fn socket_path_in_syfrah_dir() {
        assert!(socket_path()
            .to_str()
            .unwrap()
            .contains(".syfrah/control.sock"));
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
    fn write_pid_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");
        write_pid_file(&path).unwrap();
        // No .tmp file should remain
        assert!(!dir.path().join("test.tmp").exists());
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
        assert!(!is_running(999_999_999));
    }

    #[test]
    fn is_daemon_running_no_pid_file() {
        // If no daemon.pid exists (fresh system), should return false
        // This may or may not be true depending on test environment
        let _ = is_daemon_running(); // just verify no panic
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
        remove_pid_file(Path::new("/nonexistent/pid"));
    }

    // #7: Lock file
    #[test]
    fn acquire_lock_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let _lock = acquire_lock(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn acquire_lock_prevents_double() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let _lock1 = acquire_lock(&path).unwrap();
        // Second lock should fail
        let result = acquire_lock(&path);
        assert!(result.is_err());
    }

    // #6: Health check
    #[test]
    fn wait_for_healthy_timeout() {
        // No daemon running → should timeout quickly
        let result = wait_for_healthy(Duration::from_millis(100));
        assert!(result.is_err());
    }
}
