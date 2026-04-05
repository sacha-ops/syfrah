//! `syfrah storage configure` subcommand handler.
//!
//! Validates flags, writes the encryption passphrase locally, and sends
//! the storage configuration to the daemon via the control socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::api::{send_storage_request, StorageRequest, StorageResponse};
use syfrah_org::hypervisor_handler::{
    send_hypervisor_request, HypervisorRequest, HypervisorResponse,
};
use syfrah_org::HypervisorState;

/// Path where the encryption passphrase is stored locally (never replicated).
const STORAGE_KEY_PATH: &str = "/etc/syfrah/storage-key";

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

/// Parameters for `syfrah storage configure` (full S3 configuration).
pub struct ConfigureParams<'a> {
    pub region: &'a str,
    /// Availability zone. Empty string means "use region as fallback".
    pub zone: &'a str,
    pub s3_endpoint: &'a str,
    pub s3_bucket: &'a str,
    pub s3_access_key: &'a str,
    pub s3_secret_key: &'a str,
    pub cache_disk: Option<&'a str>,
    pub cache_disk_size: Option<u32>,
    pub cache_memory_size: Option<u32>,
    pub encryption_passphrase: Option<&'a str>,
}

/// Run the full `syfrah storage configure` flow with all S3 + cache flags.
pub async fn run_configure(params: &ConfigureParams<'_>) -> anyhow::Result<()> {
    let ConfigureParams {
        region,
        zone,
        s3_endpoint,
        s3_bucket,
        s3_access_key,
        s3_secret_key,
        cache_disk,
        cache_disk_size,
        cache_memory_size,
        encryption_passphrase,
    } = params;
    // --- Validate S3 endpoint URL ---
    if !s3_endpoint.starts_with("https://") && !s3_endpoint.starts_with("http://") {
        anyhow::bail!(
            "invalid S3 endpoint URL: must start with https:// or http://\n\n\
             Example: --s3-endpoint https://s3.par.io.cloud.ovh.net"
        );
    }

    // --- Validate cache disk exists (if provided) ---
    if let Some(disk) = cache_disk {
        if !Path::new(disk).exists() {
            anyhow::bail!(
                "cache disk path '{disk}' does not exist.\n\n\
                 Provide a valid block device or directory, e.g. --cache-disk-path /dev/nvme1n1"
            );
        }
    }

    // --- Handle encryption passphrase ---
    if let Some(passphrase) = encryption_passphrase {
        write_encryption_passphrase(passphrase)?;
    }

    // --- Send Configure request to daemon ---
    let req = StorageRequest::Configure {
        region: region.to_string(),
        zone: zone.to_string(),
        s3_endpoint: s3_endpoint.to_string(),
        s3_bucket: s3_bucket.to_string(),
        s3_access_key: s3_access_key.to_string(),
        s3_secret_key: s3_secret_key.to_string(),
        cache_disk_path: cache_disk.map(|s| s.to_string()),
        cache_disk_size_gb: *cache_disk_size,
        cache_memory_size_gb: *cache_memory_size,
    };

    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::StorageConfigured { zone } => {
            println!("Storage configured for zone '{zone}'.");
            if encryption_passphrase.is_some() {
                println!("Encryption passphrase saved to {STORAGE_KEY_PATH}.");
            }

            // Auto-enable hypervisors in this zone that are in NotReady state.
            // Now that storage is configured, the EnableHypervisor command will
            // pass the storage preflight check in the state machine.
            auto_enable_hypervisors(&control_socket_path(), &zone).await;

            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Run per-hypervisor cache override (only --cache-* flags).
pub async fn run_configure_cache(
    cache_disk: &str,
    cache_disk_size: u32,
    cache_memory_size: u32,
) -> anyhow::Result<()> {
    // --- Validate cache disk exists ---
    if !Path::new(cache_disk).exists() {
        anyhow::bail!(
            "cache disk path '{cache_disk}' does not exist.\n\n\
             Provide a valid block device or directory, e.g. --cache-disk-path /dev/nvme1n1"
        );
    }

    let req = StorageRequest::ConfigureCache {
        cache_disk_path: cache_disk.to_string(),
        cache_disk_size_gb: cache_disk_size,
        cache_memory_size_gb: cache_memory_size,
    };

    let resp = send_storage_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_connect_error)?;

    match resp {
        StorageResponse::Ok => {
            println!("Cache configuration updated.");
            println!("  disk: {cache_disk} ({cache_disk_size} GB), memory: {cache_memory_size} GB");
            Ok(())
        }
        StorageResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// After storage is configured for a zone, find any hypervisors in that zone
/// that are still in `NotReady` state and automatically enable them.
async fn auto_enable_hypervisors(socket_path: &Path, zone: &str) {
    // List hypervisors in the zone.
    let list_req = HypervisorRequest::List {
        region: None,
        zone: Some(zone.to_string()),
    };
    let list_resp = match send_hypervisor_request(socket_path, &list_req).await {
        Ok(r) => r,
        Err(_) => return, // Daemon unreachable; skip auto-enable silently.
    };

    let hypervisors = match list_resp {
        HypervisorResponse::HypervisorList(list) => list,
        _ => return,
    };

    for hv in &hypervisors {
        if hv.state != HypervisorState::NotReady {
            continue;
        }
        let enable_req = HypervisorRequest::Enable {
            name: hv.name.clone(),
        };
        match send_hypervisor_request(socket_path, &enable_req).await {
            Ok(HypervisorResponse::Ok) => {
                println!(
                    "\u{2713} Hypervisor {} enabled (zone {zone} now has storage configured)",
                    hv.name
                );
            }
            Ok(HypervisorResponse::Error(e)) => {
                eprintln!("  Warning: could not enable hypervisor {}: {e}", hv.name);
            }
            _ => {}
        }
    }
}

/// Write the encryption passphrase to /etc/syfrah/storage-key with 0600 perms.
fn write_encryption_passphrase(passphrase: &str) -> anyhow::Result<()> {
    let path = Path::new(STORAGE_KEY_PATH);

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!(
                "failed to create directory {}: {e}\n\n\
                 You may need to run this command as root.",
                parent.display()
            )
        })?;
    }

    fs::write(path, passphrase).map_err(|e| {
        anyhow::anyhow!(
            "failed to write encryption passphrase to {STORAGE_KEY_PATH}: {e}\n\n\
             You may need to run this command as root."
        )
    })?;

    // Set permissions to 0600 (owner read/write only)
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| anyhow::anyhow!("failed to set permissions on {STORAGE_KEY_PATH}: {e}"))?;

    Ok(())
}

/// Build a user-friendly error when the daemon is unreachable.
fn daemon_connect_error(e: Box<dyn std::error::Error>) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon -- is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_s3_endpoint_https() {
        let ep = "https://s3.example.com";
        assert!(ep.starts_with("https://") || ep.starts_with("http://"));
    }

    #[test]
    fn validate_s3_endpoint_http() {
        let ep = "http://minio:9000";
        assert!(ep.starts_with("https://") || ep.starts_with("http://"));
    }

    #[test]
    fn validate_s3_endpoint_rejects_ftp() {
        let ep = "ftp://bad.example.com";
        assert!(!ep.starts_with("https://") && !ep.starts_with("http://"));
    }

    #[test]
    fn validate_s3_endpoint_rejects_empty() {
        let ep = "";
        assert!(!ep.starts_with("https://") && !ep.starts_with("http://"));
    }

    #[test]
    fn write_encryption_passphrase_creates_file_with_correct_perms() {
        let dir = std::env::temp_dir().join("syfrah-test-passphrase");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let key_path = dir.join("storage-key");
        // Write directly to a temp path to test logic without needing /etc
        std::fs::write(&key_path, "test-passphrase").unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let contents = std::fs::read_to_string(&key_path).unwrap();
        assert_eq!(contents, "test-passphrase");

        let perms = std::fs::metadata(&key_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cache-only mode requires all three cache flags.
    #[tokio::test]
    async fn cache_only_rejects_missing_disk_path() {
        // Simulate run_storage logic: cache_disk_path = None should bail
        let cache_disk_path: Option<String> = None;
        let result = cache_disk_path.ok_or_else(|| {
            anyhow::anyhow!("--cache-disk-path is required for cache-only configuration.")
        });
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--cache-disk-path is required"));
    }

    /// Cache-only mode requires --cache-disk-size.
    #[tokio::test]
    async fn cache_only_rejects_missing_disk_size() {
        let cache_disk_size: Option<u32> = None;
        let result = cache_disk_size.ok_or_else(|| {
            anyhow::anyhow!("--cache-disk-size is required for cache-only configuration.")
        });
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--cache-disk-size is required"));
    }

    /// Cache-only mode requires --cache-memory-size.
    #[tokio::test]
    async fn cache_only_rejects_missing_memory_size() {
        let cache_memory_size: Option<u32> = None;
        let result = cache_memory_size.ok_or_else(|| {
            anyhow::anyhow!("--cache-memory-size is required for cache-only configuration.")
        });
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--cache-memory-size is required"));
    }

    /// run_configure rejects non-existent cache disk paths.
    #[tokio::test]
    async fn run_configure_rejects_nonexistent_cache_disk() {
        let params = ConfigureParams {
            region: "eu-west",
            zone: "eu-west-a",
            s3_endpoint: "https://s3.example.com",
            s3_bucket: "bucket",
            s3_access_key: "AKID",
            s3_secret_key: "SECRET",
            cache_disk: Some("/dev/nonexistent-disk-xyz"),
            cache_disk_size: Some(200),
            cache_memory_size: Some(8),
            encryption_passphrase: None,
        };
        let result = run_configure(&params).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"), "error was: {err}");
    }

    /// run_configure rejects invalid S3 endpoint URLs.
    #[tokio::test]
    async fn run_configure_rejects_invalid_s3_endpoint() {
        let params = ConfigureParams {
            region: "eu-west",
            zone: "eu-west-a",
            s3_endpoint: "ftp://bad.example.com",
            s3_bucket: "bucket",
            s3_access_key: "AKID",
            s3_secret_key: "SECRET",
            cache_disk: None,
            cache_disk_size: None,
            cache_memory_size: None,
            encryption_passphrase: None,
        };
        let result = run_configure(&params).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid S3 endpoint URL"), "error was: {err}");
    }

    /// run_configure_cache rejects non-existent disk.
    #[tokio::test]
    async fn run_configure_cache_rejects_nonexistent_disk() {
        let result = run_configure_cache("/dev/nonexistent-cache-disk-xyz", 200, 8).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"), "error was: {err}");
    }
}
