use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::ConfigError;
use crate::types::{GpuMode, NetworkConfig, VmId, VmSpec};

// ---------------------------------------------------------------------------
// ValidatedSpec (#462)
// ---------------------------------------------------------------------------

/// A `VmSpec` that has passed logical coherence checks.
///
/// Same shape as `VmSpec` but construction is only possible through
/// [`validate`], which guarantees all invariants hold.
#[derive(Debug, Clone)]
pub struct ValidatedSpec {
    pub id: VmId,
    pub vcpus: u32,
    pub memory_mb: u32,
    pub image: String,
    pub kernel: Option<String>,
    pub network: Option<NetworkConfig>,
    pub volumes: Vec<ValidatedVolume>,
    pub gpu: GpuMode,
    pub ssh_key: Option<String>,
    pub disk_size_mb: Option<u32>,
}

/// Volume attachment that has passed validation (path is non-empty).
#[derive(Debug, Clone)]
pub struct ValidatedVolume {
    pub path: String,
    pub read_only: bool,
}

/// Validate logical coherence of a `VmSpec`.
///
/// Collects ALL errors rather than failing on the first one, so the caller
/// gets a complete picture of what needs to be fixed.
pub fn validate(spec: &VmSpec) -> Result<ValidatedSpec, Vec<ConfigError>> {
    let mut errors = Vec::new();

    // VM name: must be valid (no path traversal, special chars, etc.)
    if let Err(e) = validate_name(&spec.id.to_string(), "VM") {
        errors.push(e);
    }

    // vcpus: must be > 0 and <= 256 (Cloud Hypervisor max)
    if spec.vcpus == 0 || spec.vcpus > 256 {
        errors.push(ConfigError::InvalidVcpuCount { value: spec.vcpus });
    }

    // memory: must be >= 128 MB (minimum for Linux guest boot)
    if spec.memory_mb < 128 {
        errors.push(ConfigError::InvalidMemory {
            value: spec.memory_mb,
        });
    }
    // memory: must be <= 1_048_576 MB (1 TB practical cap)
    if spec.memory_mb > 1_048_576 {
        errors.push(ConfigError::InvalidMemory {
            value: spec.memory_mb,
        });
    }
    // memory: must be a multiple of 2 (CH alignment requirement for hotplug)
    if spec.memory_mb >= 128 && !spec.memory_mb.is_multiple_of(2) {
        errors.push(ConfigError::InvalidMemory {
            value: spec.memory_mb,
        });
    }

    // image: must not be empty
    if spec.image.is_empty() {
        errors.push(ConfigError::UnknownImage {
            name: spec.image.clone(),
        });
    }

    // GPU: if Passthrough, validate BDF format DDDD:BB:DD.F
    if let GpuMode::Passthrough { ref bdf } = spec.gpu {
        if !is_valid_bdf(bdf) {
            errors.push(ConfigError::InvalidBdf { bdf: bdf.clone() });
        }
    }

    // volumes: each path must not be empty
    for (i, vol) in spec.volumes.iter().enumerate() {
        if vol.path.is_empty() {
            errors.push(ConfigError::EmptyVolumePath { index: i });
        }
    }

    // ssh_key: if provided, must not be empty after trim
    if let Some(ref key) = spec.ssh_key {
        if key.trim().is_empty() {
            errors.push(ConfigError::ConflictingSettings {
                detail: "ssh_key must not be empty or whitespace-only".to_string(),
            });
        }
    }

    // disk_size_mb: if provided, must be >= 128 (minimum bootable disk)
    if let Some(size) = spec.disk_size_mb {
        if size < 128 {
            errors.push(ConfigError::ConflictingSettings {
                detail: format!("disk_size_mb must be >= 128, got {size}"),
            });
        }
    }

    // network: tap_name must not be empty
    if let Some(ref net) = spec.network {
        if net.tap_name.is_empty() {
            errors.push(ConfigError::EmptyTapName);
        }
    }

    if errors.is_empty() {
        Ok(ValidatedSpec {
            id: spec.id.clone(),
            vcpus: spec.vcpus,
            memory_mb: spec.memory_mb,
            image: spec.image.clone(),
            kernel: spec.kernel.clone(),
            network: spec.network.clone(),
            volumes: spec
                .volumes
                .iter()
                .map(|v| ValidatedVolume {
                    path: v.path.clone(),
                    read_only: v.read_only,
                })
                .collect(),
            gpu: spec.gpu.clone(),
            ssh_key: spec.ssh_key.clone(),
            disk_size_mb: spec.disk_size_mb,
        })
    } else {
        Err(errors)
    }
}

/// Validate a resource name (VM or image).
///
/// Rules: 1-63 chars, ASCII alphanumeric plus `-`, `_`, and `.`, must start
/// with alphanumeric. Double-dot (`..`) is rejected to prevent path traversal.
/// Rejects empty, slashes, spaces, unicode.
pub(crate) fn validate_name(name: &str, kind: &str) -> Result<(), ConfigError> {
    if name.is_empty() {
        return Err(ConfigError::ConflictingSettings {
            detail: format!("{kind} name cannot be empty"),
        });
    }
    if name.len() > 63 {
        return Err(ConfigError::ConflictingSettings {
            detail: format!("{kind} name too long (max 63 chars)"),
        });
    }
    // Only allow: alphanumeric, hyphens, underscores, dots. Must start with alphanumeric.
    let valid = name.chars().enumerate().all(|(i, c)| {
        if i == 0 {
            c.is_ascii_alphanumeric()
        } else {
            c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
        }
    });
    if !valid {
        return Err(ConfigError::ConflictingSettings {
            detail: format!(
                "{kind} name contains invalid characters \
                 (only a-z, A-Z, 0-9, -, _, . allowed, must start with alphanumeric)"
            ),
        });
    }
    if name.contains("..") {
        return Err(ConfigError::ConflictingSettings {
            detail: format!("{kind} name must not contain '..' (path traversal)"),
        });
    }
    Ok(())
}

/// Validate a PCI BDF address: DDDD:BB:DD.F
/// - 4 hex digits, colon, 2 hex digits, colon, 2 hex digits, dot, 1 hex digit
fn is_valid_bdf(bdf: &str) -> bool {
    let bytes = bdf.as_bytes();
    if bytes.len() != 12 {
        return false;
    }
    // DDDD:BB:DD.F
    // 0123456789AB
    bytes[0..4].iter().all(|b| b.is_ascii_hexdigit())
        && bytes[4] == b':'
        && bytes[5..7].iter().all(|b| b.is_ascii_hexdigit())
        && bytes[7] == b':'
        && bytes[8..10].iter().all(|b| b.is_ascii_hexdigit())
        && bytes[10] == b'.'
        && bytes[11].is_ascii_hexdigit()
}

// ---------------------------------------------------------------------------
// ResolvedSpec (#464)
// ---------------------------------------------------------------------------

/// A `ValidatedSpec` with all names resolved to filesystem paths.
///
/// Note: `resolve` does NOT check whether the paths exist on disk.
/// That is the preflight validator's responsibility.
#[derive(Debug, Clone)]
pub struct ResolvedSpec {
    pub vm_id: VmId,
    pub vcpus: u32,
    pub memory_mb: u32,
    pub kernel_path: PathBuf,
    pub rootfs_path: PathBuf,
    pub network: Option<NetworkConfig>,
    pub volume_paths: Vec<ResolvedVolume>,
    pub gpu_sysfs_path: Option<PathBuf>,
}

/// A volume with its path resolved to a `PathBuf`.
#[derive(Debug, Clone)]
pub struct ResolvedVolume {
    pub path: PathBuf,
    pub read_only: bool,
}

/// Resolve names in a `ValidatedSpec` to filesystem paths.
///
/// - `image_dir`: directory where raw images live (e.g., `/opt/syfrah/images`)
/// - `default_kernel`: path to the shared kernel (e.g., `/opt/syfrah/vmlinux`)
///
/// Does NOT check whether paths exist on disk — that is preflight's job.
pub fn resolve(
    spec: &ValidatedSpec,
    image_dir: &Path,
    default_kernel: &Path,
) -> Result<ResolvedSpec, Vec<ConfigError>> {
    let kernel_path = match spec.kernel {
        Some(ref k) => PathBuf::from(k),
        None => default_kernel.to_path_buf(),
    };

    let rootfs_path = image_dir.join(format!("{}.raw", spec.image));

    let gpu_sysfs_path = match spec.gpu {
        GpuMode::Passthrough { ref bdf } => {
            Some(PathBuf::from(format!("/sys/bus/pci/devices/{bdf}/")))
        }
        GpuMode::None => None,
    };

    let volume_paths = spec
        .volumes
        .iter()
        .map(|v| ResolvedVolume {
            path: PathBuf::from(&v.path),
            read_only: v.read_only,
        })
        .collect();

    Ok(ResolvedSpec {
        vm_id: spec.id.clone(),
        vcpus: spec.vcpus,
        memory_mb: spec.memory_mb,
        kernel_path,
        rootfs_path,
        network: spec.network.clone(),
        volume_paths,
        gpu_sysfs_path,
    })
}

// ---------------------------------------------------------------------------
// map (#465)
// ---------------------------------------------------------------------------

/// Map a `ResolvedSpec` to Cloud Hypervisor `VmConfig` JSON.
///
/// Returns `serde_json::Value` rather than a typed struct because CH's API is
/// complex and evolving — a typed struct would be over-engineering at this stage.
///
/// - `socket_path`: path to the per-VM API socket (used by the caller for
///   spawning CH via CLI arg, not embedded in VmConfig JSON)
pub fn map(spec: &ResolvedSpec, socket_path: &Path) -> serde_json::Value {
    let _ = socket_path; // CH receives the socket path via CLI arg, not in VmConfig JSON

    let memory_bytes = u64::from(spec.memory_mb) * 1024 * 1024;

    // Build disks array: rootfs first, then volumes
    let mut disks = vec![json!({
        "path": spec.rootfs_path.to_string_lossy(),
    })];
    for vol in &spec.volume_paths {
        let mut disk = serde_json::Map::new();
        disk.insert(
            "path".to_string(),
            json!(vol.path.to_string_lossy().into_owned()),
        );
        if vol.read_only {
            disk.insert("readonly".to_string(), json!(true));
        }
        disks.push(serde_json::Value::Object(disk));
    }

    let mut config = json!({
        "payload": {
            "kernel": spec.kernel_path.to_string_lossy(),
            "cmdline": "console=ttyS0 root=/dev/vda1 rw",
        },
        "cpus": {
            "boot_vcpus": spec.vcpus,
            "max_vcpus": spec.vcpus,
        },
        "memory": {
            "size": memory_bytes,
        },
        "disks": disks,
        "rng": {
            "src": "/dev/urandom",
        },
        "serial": {
            "mode": "Null",
        },
        "console": {
            "mode": "Off",
        },
    });

    // Network: optional
    if let Some(ref net) = spec.network {
        let mut net_entry = serde_json::Map::new();
        net_entry.insert("tap".to_string(), json!(net.tap_name));
        if let Some(ref mac) = net.mac {
            net_entry.insert("mac".to_string(), json!(mac));
        }
        config["net"] = json!([serde_json::Value::Object(net_entry)]);
    }

    // GPU: optional VFIO passthrough
    if let Some(ref gpu_path) = spec.gpu_sysfs_path {
        config["devices"] = json!([{
            "path": gpu_path.to_string_lossy(),
        }]);
    }

    config
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GpuMode, NetworkConfig, VmId, VmSpec, VolumeAttachment};

    fn minimal_spec() -> VmSpec {
        VmSpec {
            id: VmId("vm-test-1".to_string()),
            vcpus: 2,
            memory_mb: 512,
            image: "ubuntu-24.04".to_string(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        }
    }

    fn full_spec() -> VmSpec {
        VmSpec {
            id: VmId("vm-full".to_string()),
            vcpus: 4,
            memory_mb: 4096,
            image: "ubuntu-24.04".to_string(),
            kernel: Some("/boot/vmlinuz-custom".to_string()),
            network: Some(NetworkConfig {
                tap_name: "tap-vm-full".to_string(),
                mac: Some("52:54:00:12:34:56".to_string()),
            }),
            volumes: vec![
                VolumeAttachment {
                    path: "/dev/nbd0".to_string(),
                    read_only: false,
                    volume_id: None,
                    is_root: false,
                },
                VolumeAttachment {
                    path: "/dev/nbd1".to_string(),
                    read_only: true,
                    volume_id: None,
                    is_root: false,
                },
            ],
            gpu: GpuMode::Passthrough {
                bdf: "0000:01:00.0".to_string(),
            },
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        }
    }

    // -- validate tests -------------------------------------------------------

    #[test]
    fn validate_minimal_spec_ok() {
        let result = validate(&minimal_spec());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_full_spec_ok() {
        let result = validate(&full_spec());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_zero_vcpus() {
        let mut spec = minimal_spec();
        spec.vcpus = 0;
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidVcpuCount { value: 0 })));
    }

    #[test]
    fn validate_excessive_vcpus() {
        let mut spec = minimal_spec();
        spec.vcpus = 257;
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidVcpuCount { value: 257 })));
    }

    #[test]
    fn validate_256_vcpus_ok() {
        let mut spec = minimal_spec();
        spec.vcpus = 256;
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn validate_memory_too_low() {
        let mut spec = minimal_spec();
        spec.memory_mb = 64;
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidMemory { value: 64 })));
    }

    #[test]
    fn validate_memory_128_ok() {
        let mut spec = minimal_spec();
        spec.memory_mb = 128;
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn validate_memory_odd_rejected() {
        let mut spec = minimal_spec();
        spec.memory_mb = 129;
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidMemory { value: 129 })));
    }

    #[test]
    fn validate_empty_image() {
        let mut spec = minimal_spec();
        spec.image = String::new();
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::UnknownImage { .. })));
    }

    #[test]
    fn validate_invalid_bdf() {
        let mut spec = minimal_spec();
        spec.gpu = GpuMode::Passthrough {
            bdf: "invalid".to_string(),
        };
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidBdf { .. })));
    }

    #[test]
    fn validate_valid_bdf_formats() {
        for bdf in ["0000:01:00.0", "abcd:ef:12.3", "ABCD:EF:12.3"] {
            let mut spec = minimal_spec();
            spec.gpu = GpuMode::Passthrough {
                bdf: bdf.to_string(),
            };
            assert!(validate(&spec).is_ok(), "BDF {bdf} should be valid");
        }
    }

    #[test]
    fn validate_empty_volume_path() {
        let mut spec = minimal_spec();
        spec.volumes = vec![VolumeAttachment {
            path: String::new(),
            read_only: false,
            volume_id: None,
            is_root: false,
        }];
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::EmptyVolumePath { index: 0 })));
    }

    #[test]
    fn validate_empty_tap_name() {
        let mut spec = minimal_spec();
        spec.network = Some(NetworkConfig {
            tap_name: String::new(),
            mac: None,
        });
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::EmptyTapName)));
    }

    #[test]
    fn validate_collects_all_errors() {
        let spec = VmSpec {
            id: VmId("vm-bad".to_string()),
            vcpus: 0,
            memory_mb: 10,
            image: String::new(),
            kernel: None,
            network: Some(NetworkConfig {
                tap_name: String::new(),
                mac: None,
            }),
            volumes: vec![VolumeAttachment {
                path: String::new(),
                read_only: false,
                volume_id: None,
                is_root: false,
            }],
            gpu: GpuMode::Passthrough {
                bdf: "bad".to_string(),
            },
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };
        let errors = validate(&spec).unwrap_err();
        // Should have at least 5 errors: vcpus, memory, image, bdf, volume, tap
        assert!(
            errors.len() >= 5,
            "expected >= 5 errors, got {}: {:?}",
            errors.len(),
            errors
        );
    }

    // -- resolve tests --------------------------------------------------------

    #[test]
    fn resolve_minimal() {
        let validated = validate(&minimal_spec()).unwrap();
        let image_dir = Path::new("/opt/syfrah/images");
        let kernel = Path::new("/opt/syfrah/vmlinux");
        let resolved = resolve(&validated, image_dir, kernel).unwrap();

        assert_eq!(resolved.kernel_path, PathBuf::from("/opt/syfrah/vmlinux"));
        assert_eq!(
            resolved.rootfs_path,
            PathBuf::from("/opt/syfrah/images/ubuntu-24.04.raw")
        );
        assert!(resolved.gpu_sysfs_path.is_none());
        assert!(resolved.volume_paths.is_empty());
        assert!(resolved.network.is_none());
    }

    #[test]
    fn resolve_custom_kernel() {
        let mut spec = minimal_spec();
        spec.kernel = Some("/boot/custom-vmlinuz".to_string());
        let validated = validate(&spec).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        assert_eq!(resolved.kernel_path, PathBuf::from("/boot/custom-vmlinuz"));
    }

    #[test]
    fn resolve_gpu_passthrough() {
        let validated = validate(&full_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        assert_eq!(
            resolved.gpu_sysfs_path,
            Some(PathBuf::from("/sys/bus/pci/devices/0000:01:00.0/"))
        );
    }

    #[test]
    fn resolve_volumes() {
        let validated = validate(&full_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        assert_eq!(resolved.volume_paths.len(), 2);
        assert_eq!(resolved.volume_paths[0].path, PathBuf::from("/dev/nbd0"));
        assert!(!resolved.volume_paths[0].read_only);
        assert_eq!(resolved.volume_paths[1].path, PathBuf::from("/dev/nbd1"));
        assert!(resolved.volume_paths[1].read_only);
    }

    // -- map tests ------------------------------------------------------------

    #[test]
    fn map_minimal() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let socket = Path::new("/run/syfrah/vms/vm-test-1/api.sock");
        let json = map(&resolved, socket);

        assert_eq!(json["payload"]["kernel"], "/opt/syfrah/vmlinux");
        assert_eq!(
            json["payload"]["cmdline"],
            "console=ttyS0 root=/dev/vda1 rw"
        );
        assert_eq!(json["cpus"]["boot_vcpus"], 2);
        assert_eq!(json["cpus"]["max_vcpus"], 2);
        assert_eq!(json["memory"]["size"], 512 * 1024 * 1024);
        assert_eq!(json["disks"].as_array().unwrap().len(), 1);
        assert_eq!(json["rng"]["src"], "/dev/urandom");
        assert!(json.get("net").is_none());
        assert!(json.get("devices").is_none());
    }

    #[test]
    fn map_full() {
        let validated = validate(&full_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let socket = Path::new("/run/syfrah/vms/vm-full/api.sock");
        let json = map(&resolved, socket);

        // Network
        let net = json["net"].as_array().unwrap();
        assert_eq!(net.len(), 1);
        assert_eq!(net[0]["tap"], "tap-vm-full");
        assert_eq!(net[0]["mac"], "52:54:00:12:34:56");

        // Disks: rootfs + 2 volumes
        let disks = json["disks"].as_array().unwrap();
        assert_eq!(disks.len(), 3);
        assert_eq!(disks[0]["path"], "/opt/syfrah/images/ubuntu-24.04.raw");
        assert_eq!(disks[1]["path"], "/dev/nbd0");
        assert!(disks[1].get("readonly").is_none());
        assert_eq!(disks[2]["path"], "/dev/nbd1");
        assert_eq!(disks[2]["readonly"], true);

        // GPU
        let devices = json["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["path"], "/sys/bus/pci/devices/0000:01:00.0/");

        // Memory in bytes
        assert_eq!(json["memory"]["size"], 4096u64 * 1024 * 1024);
    }

    #[test]
    fn map_memory_in_bytes() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        assert_eq!(json["memory"]["size"], 536_870_912u64); // 512 * 1024 * 1024
    }

    // -- validate: additional positive cases ----------------------------------

    #[test]
    fn validate_truly_minimal_spec_1vcpu_128mb() {
        let spec = VmSpec {
            id: VmId("vm-tiny".to_string()),
            vcpus: 1,
            memory_mb: 128,
            image: "alpine".to_string(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn validate_max_vcpus_large_memory() {
        let spec = VmSpec {
            id: VmId("vm-max".to_string()),
            vcpus: 256,
            memory_mb: 1_048_576, // 1 TB
            image: "ubuntu-24.04".to_string(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };
        assert!(validate(&spec).is_ok());
    }

    // -- validate: additional negative cases ----------------------------------

    #[test]
    fn validate_memory_127_rejected() {
        let mut spec = minimal_spec();
        spec.memory_mb = 127;
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidMemory { value: 127 })));
    }

    #[test]
    fn validate_bdf_valid_looking_but_wrong_hex() {
        let mut spec = minimal_spec();
        spec.gpu = GpuMode::Passthrough {
            bdf: "0000:GG:00.0".to_string(),
        };
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidBdf { .. })));
    }

    #[test]
    fn validate_bdf_not_a_bdf() {
        let mut spec = minimal_spec();
        spec.gpu = GpuMode::Passthrough {
            bdf: "not-a-bdf".to_string(),
        };
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidBdf { .. })));
    }

    #[test]
    fn validate_multiple_errors_vcpus_memory_image() {
        let spec = VmSpec {
            id: VmId("vm-multi-err".to_string()),
            vcpus: 0,
            memory_mb: 0,
            image: String::new(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };
        let errors = validate(&spec).unwrap_err();
        assert!(
            errors.len() >= 3,
            "expected >= 3 errors (vcpus, memory, image), got {}: {:?}",
            errors.len(),
            errors
        );
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidVcpuCount { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidMemory { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::UnknownImage { .. })));
    }

    // -- resolve: additional cases --------------------------------------------

    #[test]
    fn resolve_succeeds_with_nonexistent_paths() {
        // resolve must NOT check filesystem existence — that is preflight's job
        let spec = VmSpec {
            id: VmId("vm-nopath".to_string()),
            vcpus: 2,
            memory_mb: 256,
            image: "doesnotexist".to_string(),
            kernel: Some("/nonexistent/kernel".to_string()),
            network: None,
            volumes: vec![VolumeAttachment {
                path: "/nonexistent/volume".to_string(),
                read_only: false,
                volume_id: None,
                is_root: false,
            }],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };
        let validated = validate(&spec).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/fake/images"),
            Path::new("/fake/vmlinux"),
        );
        assert!(resolved.is_ok());
        let resolved = resolved.unwrap();
        assert_eq!(
            resolved.rootfs_path,
            PathBuf::from("/fake/images/doesnotexist.raw")
        );
        assert_eq!(resolved.kernel_path, PathBuf::from("/nonexistent/kernel"));
        assert_eq!(
            resolved.volume_paths[0].path,
            PathBuf::from("/nonexistent/volume")
        );
    }

    #[test]
    fn resolve_network_preserved() {
        let validated = validate(&full_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let net = resolved.network.as_ref().unwrap();
        assert_eq!(net.tap_name, "tap-vm-full");
        assert_eq!(net.mac.as_deref(), Some("52:54:00:12:34:56"));
    }

    #[test]
    fn resolve_image_name_to_raw_path() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        assert_eq!(
            resolved.rootfs_path,
            PathBuf::from("/opt/syfrah/images/ubuntu-24.04.raw")
        );
    }

    // -- map: additional structure checks -------------------------------------

    #[test]
    fn map_cpus_boot_vcpus_is_integer() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        // Ensure boot_vcpus is a JSON number (u64), not a string
        assert!(json["cpus"]["boot_vcpus"].is_u64());
        assert_eq!(json["cpus"]["boot_vcpus"].as_u64().unwrap(), 2);
    }

    #[test]
    fn map_rootfs_is_first_disk() {
        let validated = validate(&full_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        let disks = json["disks"].as_array().unwrap();
        assert_eq!(disks[0]["path"], "/opt/syfrah/images/ubuntu-24.04.raw");
        // Volumes come after rootfs
        assert_eq!(disks[1]["path"], "/dev/nbd0");
        assert_eq!(disks[2]["path"], "/dev/nbd1");
    }

    #[test]
    fn map_no_network_means_no_net_key() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        assert!(json.get("net").is_none());
    }

    #[test]
    fn map_with_network_has_net_array() {
        let mut spec = minimal_spec();
        spec.network = Some(NetworkConfig {
            tap_name: "tap0".to_string(),
            mac: None,
        });
        let validated = validate(&spec).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        let net = json["net"].as_array().unwrap();
        assert_eq!(net.len(), 1);
        assert_eq!(net[0]["tap"], "tap0");
    }

    #[test]
    fn map_no_gpu_means_no_devices_key() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        assert!(json.get("devices").is_none());
    }

    #[test]
    fn map_gpu_passthrough_has_devices_array() {
        let mut spec = minimal_spec();
        spec.gpu = GpuMode::Passthrough {
            bdf: "0000:41:00.0".to_string(),
        };
        let validated = validate(&spec).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        let devices = json["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["path"], "/sys/bus/pci/devices/0000:41:00.0/");
    }

    #[test]
    fn map_rng_src_is_dev_urandom() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        assert_eq!(json["rng"]["src"], "/dev/urandom");
    }

    #[test]
    fn map_has_payload_kernel_field() {
        let validated = validate(&minimal_spec()).unwrap();
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .unwrap();
        let json = map(&resolved, Path::new("/tmp/test.sock"));
        assert!(json["payload"]["kernel"].is_string());
        assert_eq!(json["payload"]["kernel"], "/opt/syfrah/vmlinux");
    }

    // -- full pipeline end-to-end ---------------------------------------------

    #[test]
    fn full_pipeline_validate_resolve_map() {
        let spec = VmSpec {
            id: VmId("vm-e2e".to_string()),
            vcpus: 4,
            memory_mb: 4096,
            image: "debian-12".to_string(),
            kernel: None,
            network: Some(NetworkConfig {
                tap_name: "tap-e2e".to_string(),
                mac: Some("52:54:00:aa:bb:cc".to_string()),
            }),
            volumes: vec![VolumeAttachment {
                path: "/dev/nbd0".to_string(),
                read_only: false,
                volume_id: None,
                is_root: false,
            }],
            gpu: GpuMode::Passthrough {
                bdf: "0000:03:00.0".to_string(),
            },
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };

        // Step 1: validate
        let validated = validate(&spec).expect("validation should pass");
        assert_eq!(validated.vcpus, 4);
        assert_eq!(validated.memory_mb, 4096);

        // Step 2: resolve
        let resolved = resolve(
            &validated,
            Path::new("/opt/syfrah/images"),
            Path::new("/opt/syfrah/vmlinux"),
        )
        .expect("resolve should pass");
        assert_eq!(resolved.kernel_path, PathBuf::from("/opt/syfrah/vmlinux"));
        assert_eq!(
            resolved.rootfs_path,
            PathBuf::from("/opt/syfrah/images/debian-12.raw")
        );
        assert_eq!(
            resolved.gpu_sysfs_path,
            Some(PathBuf::from("/sys/bus/pci/devices/0000:03:00.0/"))
        );

        // Step 3: map
        let json = map(&resolved, Path::new("/run/syfrah/vms/vm-e2e/api.sock"));
        assert_eq!(json["payload"]["kernel"], "/opt/syfrah/vmlinux");
        assert_eq!(json["cpus"]["boot_vcpus"], 4);
        assert_eq!(json["memory"]["size"], 4096u64 * 1024 * 1024);
        assert_eq!(json["disks"].as_array().unwrap().len(), 2); // rootfs + 1 volume
        assert_eq!(json["net"].as_array().unwrap().len(), 1);
        assert_eq!(json["net"].as_array().unwrap()[0]["tap"], "tap-e2e");
        assert_eq!(json["devices"].as_array().unwrap().len(), 1);
        assert_eq!(json["rng"]["src"], "/dev/urandom");
    }

    // -- ssh_key and disk_size_mb validation (#538) ---------------------------

    #[test]
    fn validate_ssh_key_empty_rejected() {
        let mut spec = minimal_spec();
        spec.ssh_key = Some("".to_string());
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::ConflictingSettings { .. })));
    }

    #[test]
    fn validate_ssh_key_whitespace_only_rejected() {
        let mut spec = minimal_spec();
        spec.ssh_key = Some("  \n".to_string());
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::ConflictingSettings { .. })));
    }

    #[test]
    fn validate_ssh_key_valid_passes() {
        let mut spec = minimal_spec();
        spec.ssh_key = Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 user@host".to_string());
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn validate_disk_size_mb_too_small_rejected() {
        let mut spec = minimal_spec();
        spec.disk_size_mb = Some(50);
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::ConflictingSettings { .. })));
    }

    #[test]
    fn validate_disk_size_mb_128_passes() {
        let mut spec = minimal_spec();
        spec.disk_size_mb = Some(128);
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn validate_disk_size_mb_none_passes() {
        let spec = minimal_spec();
        assert!(validate(&spec).is_ok());
        assert!(spec.disk_size_mb.is_none());
    }

    // -- validate_name tests --------------------------------------------------

    #[test]
    fn validate_name_valid_web_1() {
        assert!(validate_name("web-1", "VM").is_ok());
    }

    #[test]
    fn validate_name_valid_my_vm() {
        assert!(validate_name("my_vm", "VM").is_ok());
    }

    #[test]
    fn validate_name_valid_alpine_3_20() {
        assert!(validate_name("alpine-3-20", "image").is_ok());
    }

    #[test]
    fn validate_name_valid_with_dot() {
        assert!(validate_name("ubuntu-24.04", "image").is_ok());
        assert!(validate_name("debian-12.1", "image").is_ok());
    }

    #[test]
    fn validate_name_reject_double_dot() {
        let err = validate_name("name..bad", "VM").unwrap_err();
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn validate_name_reject_empty() {
        let err = validate_name("", "VM").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn validate_name_reject_slash() {
        let err = validate_name("my/vm", "VM").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_name_reject_path_traversal() {
        let err = validate_name("../hack", "VM").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_name_reject_space() {
        let err = validate_name("my vm", "VM").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_name_reject_too_long() {
        let long = "a".repeat(64);
        let err = validate_name(&long, "VM").unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn validate_name_reject_emoji() {
        let err = validate_name("\u{1f680}vm", "VM").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_name_reject_starts_with_hyphen() {
        let err = validate_name("-leading", "VM").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_name_max_63_chars_ok() {
        let name = "a".repeat(63);
        assert!(validate_name(&name, "VM").is_ok());
    }

    #[test]
    fn validate_vm_name_in_spec_rejected() {
        let mut spec = minimal_spec();
        spec.id = VmId("../escape".to_string());
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::ConflictingSettings { .. })));
    }

    #[test]
    fn validate_memory_1tb_ok() {
        let mut spec = minimal_spec();
        spec.memory_mb = 1_048_576; // exactly 1 TB
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn validate_memory_exceeds_1tb_rejected() {
        let mut spec = minimal_spec();
        spec.memory_mb = 1_048_578; // just over 1 TB (and even)
        let errors = validate(&spec).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidMemory { .. })));
    }
}
