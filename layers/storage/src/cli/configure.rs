//! `syfrah storage configure` subcommand handler.
//!
//! Validates flags, writes the encryption passphrase locally, and sends
//! the storage configuration to the daemon via the control socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::api::{send_storage_request, StorageRequest, StorageResponse};

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
                 Provide a valid block device or directory, e.g. --cache-disk /dev/nvme1n1"
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
        StorageResponse::StorageConfigured { region } => {
            println!("Storage configured for region '{region}'.");
            if encryption_passphrase.is_some() {
                println!("Encryption passphrase saved to {STORAGE_KEY_PATH}.");
            }
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
             Provide a valid block device or directory, e.g. --cache-disk /dev/nvme1n1"
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
}
