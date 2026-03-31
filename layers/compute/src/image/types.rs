use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ImageMeta (#535)
// ---------------------------------------------------------------------------

/// Full metadata for an image in the catalog.
///
/// Every field that the image management system needs is here from day one so
/// downstream consumers (image service, disk service, CLI, catalog) never need
/// schema migrations.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ImageMeta {
    /// Image name / slug (e.g. `"ubuntu-24.04"`).
    pub name: String,
    /// CPU architecture (e.g. `"aarch64"`, `"x86_64"`).
    pub arch: String,
    /// OS family (e.g. `"linux"`, `"windows"`).
    pub os_family: String,
    /// Optional variant tag (e.g. `"minimal"`, `"desktop"`).
    pub variant: Option<String>,
    /// Disk image format (e.g. `"raw"`, `"qcow2"`).
    pub format: String,
    /// Compression algorithm used on the download artifact (e.g. `"zstd"`, `"gzip"`).
    pub compression: Option<String>,
    /// Boot mode (e.g. `"uefi"`, `"bios"`).
    pub boot_mode: String,
    /// SHA-256 checksum of the uncompressed image.
    pub sha256: String,
    /// Image size in megabytes.
    pub size_mb: u64,
    /// Minimum disk size in megabytes required to boot the image.
    pub min_disk_mb: u64,
    /// Whether the image ships with cloud-init support.
    pub cloud_init: bool,
    /// Default login username baked into the image.
    pub default_username: Option<String>,
    /// Root filesystem type (e.g. `"ext4"`, `"btrfs"`).
    pub rootfs_fs: Option<String>,
    /// How the image was sourced (e.g. `"catalog"`, `"import"`, `"build"`).
    pub source_kind: String,
    /// Filename of the image artifact in the catalog.
    pub file: String,
    /// Filename of the OCI container image artifact in the catalog.
    /// `None` for images that only have a VM (raw) variant.
    #[serde(default)]
    pub container_file: Option<String>,
    /// SHA-256 checksum of the uncompressed OCI container image.
    /// `None` when no container variant is available.
    #[serde(default)]
    pub container_sha256: Option<String>,
    /// Local-only: timestamp of when the image was imported. `None` for
    /// catalog-only entries that have not been pulled yet.
    pub imported_at: Option<String>,
}

// ---------------------------------------------------------------------------
// ImageCatalog (#535)
// ---------------------------------------------------------------------------

/// A versioned collection of images served by a catalog endpoint.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ImageCatalog {
    /// Schema version of the catalog format.
    pub version: u32,
    /// Base URL where image artifacts can be downloaded.
    pub base_url: String,
    /// List of available images.
    pub images: Vec<ImageMeta>,
}

// ---------------------------------------------------------------------------
// PullPolicy (#535)
// ---------------------------------------------------------------------------

/// Controls when the image service should fetch an image from the catalog.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum PullPolicy {
    /// Only pull if the image is not already present locally.
    #[default]
    IfNotPresent,
    /// Always pull, even if a local copy exists.
    Always,
    /// Never pull — fail if the image is not present locally.
    Never,
}

// ---------------------------------------------------------------------------
// RuntimeMode (#608)
// ---------------------------------------------------------------------------

/// Specifies which image format to pull based on the target runtime.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Pull the VM raw disk image (default, for KVM / Cloud Hypervisor).
    #[default]
    Vm,
    /// Pull the OCI container image (for crun + gVisor).
    Container,
}

// ---------------------------------------------------------------------------
// InstanceId (#536)
// ---------------------------------------------------------------------------

/// Unique identifier for a VM instance directory, backed by a v4 UUID.
///
/// Used as filesystem path component and HashMap key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(Uuid);

impl InstanceId {
    /// Generate a new random instance ID (UUID v4).
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// CloudInitNetworkConfig (#756)
// ---------------------------------------------------------------------------

/// Structured network configuration for cloud-init config-drive.
///
/// Generates a Netplan v2 `network-config` file that cloud-init's NoCloud
/// data source reads from the config-drive ISO.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CloudInitNetworkConfig {
    /// Static IPv4 address (e.g. `"10.0.1.5"`).
    pub ip: String,
    /// Subnet prefix length (e.g. `24` for `/24`).
    pub prefix_len: u8,
    /// Default gateway (e.g. `"10.0.1.1"`).
    pub gateway: String,
    /// MTU value (e.g. `1350` for VXLAN overlay).
    pub mtu: u16,
    /// DNS nameserver addresses.
    pub dns: Vec<String>,
}

impl CloudInitNetworkConfig {
    /// Render Netplan v2 YAML suitable for the `network-config` file.
    pub fn to_yaml(&self) -> String {
        let mut dns_entries = String::new();
        for addr in &self.dns {
            dns_entries.push_str(&format!("            - {addr}\n"));
        }

        format!(
            "network:\n\
             \x20 version: 2\n\
             \x20 ethernets:\n\
             \x20   id0:\n\
             \x20     match:\n\
             \x20       name: en*\n\
             \x20     addresses:\n\
             \x20       - {ip}/{prefix}\n\
             \x20     gateway4: {gw}\n\
             \x20     mtu: {mtu}\n\
             \x20     nameservers:\n\
             \x20       addresses:\n\
{dns}",
            ip = self.ip,
            prefix = self.prefix_len,
            gw = self.gateway,
            mtu = self.mtu,
            dns = dns_entries,
        )
    }
}

// ---------------------------------------------------------------------------
// CloudInitConfig (#536)
// ---------------------------------------------------------------------------

/// Structured cloud-init configuration for VM provisioning.
///
/// All optional fields default to `None` / empty so callers can build minimal
/// configs without specifying every knob.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CloudInitConfig {
    /// Hostname to assign to the guest.
    pub hostname: String,
    /// SSH public keys to inject into the default user's `authorized_keys`.
    pub ssh_authorized_keys: Vec<String>,
    /// Name of the default user account.
    pub default_user: String,
    /// Additional user accounts to create.
    #[serde(default)]
    pub users: Vec<UserConfig>,
    /// Structured network configuration for the config-drive.
    pub network_config: Option<CloudInitNetworkConfig>,
    /// Extra user-data YAML appended to the generated user-data file.
    pub user_data_extra: Option<String>,
}

/// A user account to create inside the guest via cloud-init.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserConfig {
    /// Username.
    pub name: String,
    /// Groups the user should belong to.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Sudo rule (e.g. `"ALL=(ALL) NOPASSWD:ALL"`).
    pub sudo: Option<String>,
    /// Login shell path.
    pub shell: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn sample_image_meta() -> ImageMeta {
        ImageMeta {
            name: "ubuntu-24.04".to_string(),
            arch: "aarch64".to_string(),
            os_family: "linux".to_string(),
            variant: Some("minimal".to_string()),
            format: "raw".to_string(),
            compression: Some("zstd".to_string()),
            boot_mode: "uefi".to_string(),
            sha256: "abc123def456".to_string(),
            size_mb: 2048,
            min_disk_mb: 4096,
            cloud_init: true,
            default_username: Some("ubuntu".to_string()),
            rootfs_fs: Some("ext4".to_string()),
            source_kind: "catalog".to_string(),
            file: "ubuntu-24.04-aarch64-minimal.raw.zst".to_string(),
            container_file: None,
            container_sha256: None,
            imported_at: Some("2025-01-15T12:00:00Z".to_string()),
        }
    }

    fn minimal_image_meta() -> ImageMeta {
        ImageMeta {
            name: "alpine-3.20".to_string(),
            arch: "x86_64".to_string(),
            os_family: "linux".to_string(),
            variant: None,
            format: "raw".to_string(),
            compression: None,
            boot_mode: "bios".to_string(),
            sha256: "deadbeef".to_string(),
            size_mb: 256,
            min_disk_mb: 512,
            cloud_init: false,
            default_username: None,
            rootfs_fs: None,
            source_kind: "import".to_string(),
            file: "alpine-3.20-x86_64.raw".to_string(),
            container_file: None,
            container_sha256: None,
            imported_at: None,
        }
    }

    // -- ImageMeta tests (#535) -----------------------------------------------

    #[test]
    fn image_meta_serde_roundtrip_full() {
        let meta = sample_image_meta();
        let json = serde_json::to_string(&meta).unwrap();
        let back: ImageMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn image_meta_serde_roundtrip_minimal() {
        let meta = minimal_image_meta();
        let json = serde_json::to_string(&meta).unwrap();
        let back: ImageMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
        assert!(back.imported_at.is_none());
        assert!(back.variant.is_none());
    }

    // -- ImageCatalog tests (#535) --------------------------------------------

    #[test]
    fn image_catalog_serde_roundtrip() {
        let catalog = ImageCatalog {
            version: 1,
            base_url: "https://images.syfrah.dev/v1".to_string(),
            images: vec![sample_image_meta(), minimal_image_meta()],
        };
        let json = serde_json::to_string(&catalog).unwrap();
        let back: ImageCatalog = serde_json::from_str(&json).unwrap();
        assert_eq!(catalog, back);
        assert_eq!(back.images.len(), 2);
    }

    #[test]
    fn image_catalog_parse_from_json_string() {
        let json = r#"{
            "version": 1,
            "base_url": "https://images.syfrah.dev/v1",
            "images": [
                {
                    "name": "ubuntu-24.04",
                    "arch": "aarch64",
                    "os_family": "linux",
                    "variant": null,
                    "format": "raw",
                    "compression": "zstd",
                    "boot_mode": "uefi",
                    "sha256": "abc123",
                    "size_mb": 2048,
                    "min_disk_mb": 4096,
                    "cloud_init": true,
                    "default_username": "ubuntu",
                    "rootfs_fs": "ext4",
                    "source_kind": "catalog",
                    "file": "ubuntu-24.04.raw.zst",
                    "imported_at": null
                }
            ]
        }"#;
        let catalog: ImageCatalog = serde_json::from_str(json).unwrap();
        assert_eq!(catalog.version, 1);
        assert_eq!(catalog.images.len(), 1);
        assert_eq!(catalog.images[0].name, "ubuntu-24.04");
    }

    // -- PullPolicy tests (#535) ----------------------------------------------

    #[test]
    fn pull_policy_default_is_if_not_present() {
        assert_eq!(PullPolicy::default(), PullPolicy::IfNotPresent);
    }

    #[test]
    fn pull_policy_serde_roundtrip() {
        for policy in [
            PullPolicy::IfNotPresent,
            PullPolicy::Always,
            PullPolicy::Never,
        ] {
            let json = serde_json::to_string(&policy).unwrap();
            let back: PullPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(policy, back);
        }
    }

    // -- RuntimeMode tests (#608) ----------------------------------------------

    #[test]
    fn runtime_mode_default_is_vm() {
        assert_eq!(RuntimeMode::default(), RuntimeMode::Vm);
    }

    #[test]
    fn runtime_mode_serde_roundtrip() {
        for mode in [RuntimeMode::Vm, RuntimeMode::Container] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: RuntimeMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn image_meta_with_container_fields_roundtrip() {
        let mut meta = sample_image_meta();
        meta.container_file = Some("ubuntu-24.04-oci.tar.gz".to_string());
        meta.container_sha256 = Some("sha256_of_oci_image".to_string());
        let json = serde_json::to_string(&meta).unwrap();
        let back: ImageMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
        assert_eq!(
            back.container_file.as_deref(),
            Some("ubuntu-24.04-oci.tar.gz")
        );
    }

    #[test]
    fn image_meta_without_container_fields_deserializes() {
        // Backward compatibility: existing JSON without container fields should
        // still deserialize, with container_file and container_sha256 as None.
        let json = r#"{
            "name": "alpine-3.20",
            "arch": "x86_64",
            "os_family": "linux",
            "variant": null,
            "format": "raw",
            "compression": null,
            "boot_mode": "bios",
            "sha256": "deadbeef",
            "size_mb": 256,
            "min_disk_mb": 512,
            "cloud_init": false,
            "default_username": null,
            "rootfs_fs": null,
            "source_kind": "import",
            "file": "alpine-3.20-x86_64.raw",
            "imported_at": null
        }"#;
        let meta: ImageMeta = serde_json::from_str(json).unwrap();
        assert!(meta.container_file.is_none());
        assert!(meta.container_sha256.is_none());
    }

    // -- InstanceId tests (#536) ----------------------------------------------

    #[test]
    fn instance_id_unique_generation() {
        let ids: Vec<InstanceId> = (0..100).map(|_| InstanceId::new()).collect();
        let unique: HashSet<_> = ids.iter().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "all 100 InstanceIds must be unique"
        );
    }

    #[test]
    fn instance_id_display() {
        let id = InstanceId::new();
        let display = id.to_string();
        // UUID v4 format: 8-4-4-4-12 hex digits
        assert_eq!(display.len(), 36);
        assert_eq!(display.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn instance_id_serde_roundtrip() {
        let id = InstanceId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: InstanceId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn instance_id_as_hashmap_key() {
        let mut map = HashMap::new();
        let id = InstanceId::new();
        map.insert(id.clone(), "running");
        assert_eq!(map.get(&id), Some(&"running"));
    }

    // -- CloudInitConfig tests (#536) -----------------------------------------

    #[test]
    fn cloud_init_config_serde_minimal() {
        let config = CloudInitConfig {
            hostname: "vm-web-1".to_string(),
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA...".to_string()],
            default_user: "ubuntu".to_string(),
            users: vec![],
            network_config: None,
            user_data_extra: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: CloudInitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
        assert!(back.users.is_empty());
        assert!(back.network_config.is_none());
    }

    #[test]
    fn cloud_init_config_serde_full() {
        let config = CloudInitConfig {
            hostname: "vm-db-1".to_string(),
            ssh_authorized_keys: vec![
                "ssh-ed25519 AAAA...".to_string(),
                "ssh-rsa BBBB...".to_string(),
            ],
            default_user: "admin".to_string(),
            users: vec![
                UserConfig {
                    name: "deploy".to_string(),
                    groups: vec!["docker".to_string(), "sudo".to_string()],
                    sudo: Some("ALL=(ALL) NOPASSWD:ALL".to_string()),
                    shell: Some("/bin/bash".to_string()),
                },
                UserConfig {
                    name: "monitor".to_string(),
                    groups: vec![],
                    sudo: None,
                    shell: None,
                },
            ],
            network_config: Some(CloudInitNetworkConfig {
                ip: "10.0.1.5".to_string(),
                prefix_len: 24,
                gateway: "10.0.1.1".to_string(),
                mtu: 1350,
                dns: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
            }),
            user_data_extra: Some("runcmd:\n  - echo hello\n".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: CloudInitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
        assert_eq!(back.users.len(), 2);
        assert!(back.network_config.is_some());
    }

    #[test]
    fn user_config_serde_roundtrip() {
        let user = UserConfig {
            name: "testuser".to_string(),
            groups: vec!["wheel".to_string()],
            sudo: Some("ALL=(ALL) ALL".to_string()),
            shell: Some("/bin/zsh".to_string()),
        };
        let json = serde_json::to_string(&user).unwrap();
        let back: UserConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(user, back);
    }

    // -- CloudInitNetworkConfig tests (#756) ----------------------------------

    #[test]
    fn cloud_init_network_config_serde_roundtrip() {
        let cfg = CloudInitNetworkConfig {
            ip: "10.0.1.5".to_string(),
            prefix_len: 24,
            gateway: "10.0.1.1".to_string(),
            mtu: 1350,
            dns: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: CloudInitNetworkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn cloud_init_network_config_to_yaml() {
        let cfg = CloudInitNetworkConfig {
            ip: "10.0.1.5".to_string(),
            prefix_len: 24,
            gateway: "10.0.1.1".to_string(),
            mtu: 1350,
            dns: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
        };
        let yaml = cfg.to_yaml();
        assert!(yaml.contains("version: 2"));
        assert!(yaml.contains("10.0.1.5/24"));
        assert!(yaml.contains("gateway4: 10.0.1.1"));
        assert!(yaml.contains("mtu: 1350"));
        assert!(yaml.contains("8.8.8.8"));
        assert!(yaml.contains("1.1.1.1"));
    }
}
