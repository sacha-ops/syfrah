//! Process utilities — paths, directories.
//!
//! No daemon, no fork, no PID file. Syfrah is a CLI orchestrator,
//! not a daemon. WireGuard runs in the kernel.

use std::path::PathBuf;

use crate::error::SyfrahError;

/// Default syfrah directory.
pub fn syfrah_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
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

/// Default control socket path (for future API server).
pub fn socket_path() -> PathBuf {
    syfrah_dir().join("control.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syfrah_dir_path() {
        let d = syfrah_dir();
        assert!(d.to_str().unwrap().contains(".syfrah"));
    }

    #[test]
    fn socket_path_in_dir() {
        let p = socket_path();
        assert!(p.to_str().unwrap().contains(".syfrah/control.sock"));
    }

    #[test]
    fn ensure_dir_creates() {
        let _ = ensure_syfrah_dir();
    }
}
