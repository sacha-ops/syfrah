//! ZeroFS binary resolution and version management.
//!
//! Follows the same pattern as Cloud Hypervisor (`layers/compute/src/binary.rs`):
//! - Pinned version from `ZEROFS_VERSION` (compile-time constant)
//! - Resolution order: explicit path → `/usr/local/lib/syfrah/zerofs` → `$PATH`
//! - Version checking and mismatch warnings

use std::path::{Path, PathBuf};

/// The ZeroFS version pinned at compile time.
///
/// Read from the `ZEROFS_VERSION` file at the repo root via `build.rs`.
const PINNED_VERSION: &str = env!("ZEROFS_VERSION");

/// Returns the pinned ZeroFS version (compile-time constant).
pub fn pinned_version() -> &'static str {
    PINNED_VERSION
}

/// Error type for ZeroFS binary resolution and version checking.
#[derive(Debug, thiserror::Error)]
pub enum ZerofsError {
    #[error("{reason}")]
    NotFound { reason: String },
    #[error("{reason}")]
    VersionCheck { reason: String },
}

/// Resolve the zerofs binary path.
///
/// Resolution order:
/// 1. `explicit` path (if provided, must exist and be executable)
/// 2. `zerofs` on `$PATH` (via `which`)
/// 3. `/usr/local/lib/syfrah/zerofs` (standard install location)
///
/// Returns the first match or an error if none found.
pub fn resolve_binary(explicit: Option<&Path>) -> Result<PathBuf, ZerofsError> {
    // 1. Explicit path from config
    if let Some(path) = explicit {
        if is_executable(path) {
            return Ok(path.to_path_buf());
        }
        if !path.exists() {
            return Err(ZerofsError::NotFound {
                reason: format!("configured zerofs binary not found: {}", path.display()),
            });
        }
        // File exists but is not executable — fall through.
    }

    // 2. Search $PATH via `which`
    if let Ok(output) = std::process::Command::new("which").arg("zerofs").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let path = PathBuf::from(path_str.trim());
            if is_executable(&path) {
                return Ok(path);
            }
        }
    }

    // 3. Standard installation path
    let installed = PathBuf::from("/usr/local/lib/syfrah/zerofs");
    if is_executable(&installed) {
        return Ok(installed);
    }

    Err(ZerofsError::NotFound {
        reason: "zerofs not found (checked $PATH and /usr/local/lib/syfrah/zerofs)".to_string(),
    })
}

/// Run `binary --version` and parse the version string from the output.
///
/// ZeroFS outputs something like: `zerofs v0.1.0`
/// Returns the version token (e.g., `v0.1.0`).
pub fn check_version(binary: &Path) -> Result<String, ZerofsError> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| ZerofsError::VersionCheck {
            reason: format!("failed to run {} --version: {e}", binary.display()),
        })?;

    if !output.status.success() {
        return Err(ZerofsError::VersionCheck {
            reason: format!(
                "{} --version exited with status {}",
                binary.display(),
                output.status,
            ),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_version_output(&stdout).ok_or_else(|| ZerofsError::VersionCheck {
        reason: format!(
            "could not parse version from '{}' --version output: {}",
            binary.display(),
            stdout.trim(),
        ),
    })
}

/// Compare the version reported by `binary --version` with the pinned version.
///
/// Returns `Ok(())` if they match, `Err(warning_message)` if they differ.
pub fn verify_version(binary: &Path) -> Result<(), String> {
    let disk_version = check_version(binary).map_err(|e| e.to_string())?;
    if disk_version == PINNED_VERSION {
        Ok(())
    } else {
        Err(format!(
            "zerofs version mismatch: pinned {PINNED_VERSION}, found {disk_version}"
        ))
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Check if a path exists and is executable (Unix only).
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

/// Parse a version string from `zerofs --version` output.
///
/// Expected format: `zerofs vX.Y.Z` — returns the token starting with 'v'.
fn parse_version_output(output: &str) -> Option<String> {
    for token in output.split_whitespace() {
        if token.starts_with('v') && token.len() > 1 && token[1..].chars().next()?.is_ascii_digit()
        {
            return Some(token.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn make_fake_binary(dir: &Path, name: &str, output: &str) -> PathBuf {
        let path = dir.join(name);
        let content = format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n");
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let dir_fd = fs::File::open(dir).unwrap();
        dir_fd.sync_all().unwrap();
        path
    }

    fn make_non_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "not executable").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        path
    }

    #[test]
    fn pinned_version_returns_nonempty_string() {
        let v = pinned_version();
        assert!(!v.is_empty());
        assert!(
            v.starts_with('v'),
            "pinned version should start with 'v', got: {v}"
        );
    }

    #[test]
    fn resolve_binary_explicit_path_exists_and_executable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = make_fake_binary(tmp.path(), "zerofs-fake", "zerofs v0.1.0");
        let result = resolve_binary(Some(&bin));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), bin);
    }

    #[test]
    fn resolve_binary_explicit_path_not_found() {
        let result = resolve_binary(Some(Path::new("/nonexistent/path/zerofs")));
        assert!(result.is_err());
    }

    #[test]
    fn resolve_binary_explicit_not_executable_falls_through() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = make_non_executable(tmp.path(), "zerofs-noexec");
        let result = resolve_binary(Some(&bin));
        assert!(result.is_err());
    }

    #[test]
    fn check_version_happy_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = make_fake_binary(tmp.path(), "zerofs-good", "zerofs v0.1.0");
        let result = check_version(&bin);
        assert!(result.is_ok(), "check_version failed: {result:?}");
        assert_eq!(result.unwrap(), "v0.1.0");
    }

    #[test]
    fn check_version_garbage_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = make_fake_binary(tmp.path(), "zerofs-garbage", "this is not a version");
        let result = check_version(&bin);
        assert!(result.is_err());
    }

    #[test]
    fn check_version_nonexistent_binary() {
        let result = check_version(Path::new("/nonexistent/zerofs"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_version_standard_format() {
        assert_eq!(
            parse_version_output("zerofs v0.1.0"),
            Some("v0.1.0".to_string())
        );
    }

    #[test]
    fn parse_version_garbage() {
        assert_eq!(parse_version_output("not a version"), None);
    }

    #[test]
    fn parse_version_empty() {
        assert_eq!(parse_version_output(""), None);
    }

    #[test]
    fn is_executable_on_executable_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = make_fake_binary(tmp.path(), "exec-test", "hello");
        assert!(is_executable(&path));
    }

    #[test]
    fn is_executable_on_non_executable_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = make_non_executable(tmp.path(), "noexec-test");
        assert!(!is_executable(&path));
    }

    #[test]
    fn is_executable_on_nonexistent_path() {
        assert!(!is_executable(Path::new("/does/not/exist")));
    }
}
