use std::fmt;

use serde::{Deserialize, Serialize};

use crate::phase::VmPhase;
use crate::runtime_backend::RuntimeType;

/// Unique identifier for a VM.
///
/// A thin wrapper around `String`, used as a key in maps and event payloads.
/// The inner value is typically a human-readable slug like `"vm-web-1"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct VmId(pub String);

impl fmt::Display for VmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Desired state of a VM, provided by forge when requesting creation.
///
/// This is the "what" — the declarative specification. Compute translates it
/// into Cloud Hypervisor configuration, resolves paths, and validates constraints.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VmSpec {
    /// Unique identifier for this VM.
    pub id: VmId,
    /// Number of virtual CPUs (must be > 0).
    pub vcpus: u32,
    /// Memory allocation in megabytes (must be >= 128, power of 2).
    pub memory_mb: u32,
    /// Root filesystem image name (e.g., `"ubuntu-24.04"`). Resolved to a path
    /// by the config pipeline.
    pub image: String,
    /// Path to the kernel. `None` uses the shared default vmlinux.
    pub kernel: Option<String>,
    /// Network configuration (TAP device), provided by overlay via forge.
    pub network: Option<NetworkConfig>,
    /// Block device volumes to attach (e.g., ZeroFS NBD devices).
    pub volumes: Vec<VolumeAttachment>,
    /// GPU passthrough mode.
    pub gpu: GpuMode,
    /// SSH public key to inject into the VM via cloud-init.
    #[serde(default)]
    pub ssh_key: Option<String>,
    /// Root disk size in megabytes. `None` uses the image default.
    #[serde(default)]
    pub disk_size_mb: Option<u32>,
    /// Resolved subnet info for overlay networking.
    /// TODO: wire into NetworkManager once #757 lands.
    #[serde(default)]
    pub subnet: Option<SubnetInfo>,
}

/// Resolved subnet information passed through to the VM creation flow.
///
/// Populated by the CLI's `--subnet` resolution logic and carried through
/// `VmSpec` so that downstream networking (overlay, IPAM) can allocate an
/// IP address and configure the VXLAN bridge for this VM.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SubnetInfo {
    /// Subnet name (e.g., `"frontend"`).
    pub name: String,
    /// Subnet CIDR (e.g., `"10.1.1.0/24"`).
    pub cidr: String,
    /// Gateway address (e.g., `"10.1.1.1"`).
    pub gateway: String,
    /// VPC ID that owns this subnet.
    pub vpc_id: String,
    /// Environment ID (e.g., `"acme/backend/production"`).
    pub env_id: String,
}

/// TAP device configuration, provided by overlay via forge.
///
/// Compute does not create or manage TAP devices — it receives this
/// configuration and passes it to Cloud Hypervisor's `--net` argument.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NetworkConfig {
    /// Name of the TAP device (e.g., `"tap-vm-web-1"`).
    pub tap_name: String,
    /// Optional MAC address override. `None` lets Cloud Hypervisor assign one.
    pub mac: Option<String>,
}

/// Block device attachment for a VM.
///
/// Volumes are provided by the storage layer (e.g., ZeroFS NBD devices)
/// and attached as additional `--disk` entries in Cloud Hypervisor.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VolumeAttachment {
    /// Path to the block device (e.g., `"/dev/nbd0"`).
    pub path: String,
    /// Whether the volume should be attached as read-only.
    pub read_only: bool,
}

/// GPU mode for a VM.
///
/// No `Shared` variant — virtio-gpu shared rendering is a future consideration.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum GpuMode {
    /// No GPU attached.
    #[default]
    None,
    /// VFIO passthrough of a PCI device. `bdf` is the PCI bus:device.function
    /// (e.g., "0000:01:00.0").
    Passthrough { bdf: String },
}

/// External view of a VM, exposed to forge and other layers.
///
/// This is what forge and the control plane see when querying VM state.
/// Internal details (PID, socket path, cgroup) are deliberately omitted.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VmStatus {
    /// Unique identifier of the VM.
    pub vm_id: VmId,
    /// Current lifecycle phase.
    pub phase: VmPhase,
    /// Number of virtual CPUs allocated.
    pub vcpus: u32,
    /// Memory allocation in megabytes.
    pub memory_mb: u32,
    /// Image name used to create this VM.
    #[serde(default)]
    pub image: Option<String>,
    /// Runtime backend type (VM or Container).
    #[serde(default)]
    pub runtime: Option<RuntimeType>,
    /// Unix timestamp of when the VM was created.
    pub created_at: Option<u64>,
    /// Seconds the VM has been running. `None` if not in the `Running` phase.
    pub uptime_secs: Option<u64>,
    /// IP address assigned to the VM (e.g. "10.1.1.3").
    #[serde(default)]
    pub ip: Option<String>,
    /// Subnet the VM belongs to (e.g. "frontend").
    #[serde(default)]
    pub subnet: Option<String>,
    /// VPC the VM belongs to (e.g. "default").
    #[serde(default)]
    pub vpc: Option<String>,
}

/// Observable events emitted to forge via a broadcast channel.
///
/// Delivery is best-effort, real-time. The source of truth for VM state is
/// always `info()` / `status()`, never the event stream alone.
#[derive(Clone, Debug)]
pub enum VmEvent {
    /// VM definition created and cloud-hypervisor process spawned.
    Created { vm_id: VmId },
    /// VM booted successfully — CH API is responding and `vm.boot` completed.
    Booted { vm_id: VmId },
    /// VM stopped cleanly via the kill chain (graceful or forced shutdown).
    Stopped { vm_id: VmId },
    /// VM crashed — process exited unexpectedly or API became unresponsive.
    Crashed { vm_id: VmId, error: String },
    /// VM deleted — all runtime artifacts cleaned up.
    Deleted { vm_id: VmId },
    /// VM successfully recovered after a daemon restart.
    ReconnectSucceeded { vm_id: VmId },
    /// VM failed to reconnect after a daemon restart.
    ReconnectFailed { vm_id: VmId, error: String },
    /// Orphaned runtime directory cleaned up during reconnect.
    VmOrphanCleaned { vm_id: VmId, reason: String },
    /// VM CPU/memory resized via hot-resize.
    Resized {
        vm_id: VmId,
        new_vcpus: u32,
        new_memory_mb: u32,
    },
    /// Device hot-attached to the VM (disk, network, or VFIO).
    DeviceAttached { vm_id: VmId, device: String },
    /// Device hot-detached from the VM.
    DeviceDetached { vm_id: VmId, device: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn vm_id_serde_roundtrip() {
        let id = VmId("vm-abc-123".to_string());
        let json = serde_json::to_string(&id).unwrap();
        let back: VmId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn vm_id_display() {
        let id = VmId("vm-test-42".to_string());
        assert_eq!(id.to_string(), "vm-test-42");
    }

    #[test]
    fn vm_id_as_hashmap_key() {
        let mut map = HashMap::new();
        let id = VmId("vm-1".to_string());
        map.insert(id.clone(), 42u64);
        assert_eq!(map.get(&id), Some(&42));
    }

    #[test]
    fn vm_spec_serde_roundtrip_full() {
        let spec = VmSpec {
            id: VmId("vm-full".to_string()),
            vcpus: 4,
            memory_mb: 8192,
            image: "ubuntu-22.04.qcow2".to_string(),
            kernel: Some("/boot/vmlinuz".to_string()),
            network: Some(NetworkConfig {
                tap_name: "tap0".to_string(),
                mac: Some("52:54:00:12:34:56".to_string()),
            }),
            volumes: vec![VolumeAttachment {
                path: "/dev/sda1".to_string(),
                read_only: false,
            }],
            gpu: GpuMode::Passthrough {
                bdf: "0000:01:00.0".to_string(),
            },
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: VmSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn vm_spec_serde_roundtrip_minimal() {
        let spec = VmSpec {
            id: VmId("vm-min".to_string()),
            vcpus: 1,
            memory_mb: 512,
            image: "alpine.qcow2".to_string(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: VmSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn gpu_mode_none_serde_roundtrip() {
        let mode = GpuMode::None;
        let json = serde_json::to_string(&mode).unwrap();
        let back: GpuMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn gpu_mode_passthrough_serde_roundtrip() {
        let mode = GpuMode::Passthrough {
            bdf: "0000:41:00.0".to_string(),
        };
        let json = serde_json::to_string(&mode).unwrap();
        let back: GpuMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, back);
        if let GpuMode::Passthrough { bdf } = &back {
            assert_eq!(bdf, "0000:41:00.0");
        } else {
            panic!("expected Passthrough variant");
        }
    }

    #[test]
    fn gpu_mode_default_is_none() {
        assert_eq!(GpuMode::default(), GpuMode::None);
    }

    #[test]
    fn network_config_serde_roundtrip() {
        let cfg = NetworkConfig {
            tap_name: "tap7".to_string(),
            mac: Some("aa:bb:cc:dd:ee:ff".to_string()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: NetworkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn volume_attachment_serde_roundtrip() {
        let vol = VolumeAttachment {
            path: "/mnt/data".to_string(),
            read_only: true,
        };
        let json = serde_json::to_string(&vol).unwrap();
        let back: VolumeAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(vol, back);
    }

    #[test]
    fn vm_status_serde_roundtrip() {
        let status = VmStatus {
            vm_id: VmId("vm-status-1".to_string()),
            phase: VmPhase::Running,
            vcpus: 2,
            memory_mb: 4096,
            image: Some("ubuntu-24.04".to_string()),
            runtime: Some(RuntimeType::Vm),
            created_at: Some(1700000000),
            uptime_secs: Some(3600),
            ip: None,
            subnet: None,
            vpc: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        let back: VmStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.vm_id, status.vm_id);
        assert_eq!(back.phase, VmPhase::Running);
        assert_eq!(back.vcpus, 2);
        assert_eq!(back.runtime, Some(RuntimeType::Vm));
        assert_eq!(back.uptime_secs, Some(3600));
    }

    #[test]
    fn vm_event_created_variant() {
        let event = VmEvent::Created {
            vm_id: VmId("vm-e1".to_string()),
        };
        let cloned = event.clone();
        assert!(format!("{cloned:?}").contains("Created"));
    }

    #[test]
    fn vm_event_crashed_carries_error() {
        let event = VmEvent::Crashed {
            vm_id: VmId("vm-crash".to_string()),
            error: "out of memory".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("out of memory"));
    }

    #[test]
    fn vm_event_resized_carries_new_values() {
        let event = VmEvent::Resized {
            vm_id: VmId("vm-resize".to_string()),
            new_vcpus: 8,
            new_memory_mb: 16384,
        };
        if let VmEvent::Resized {
            new_vcpus,
            new_memory_mb,
            ..
        } = event
        {
            assert_eq!(new_vcpus, 8);
            assert_eq!(new_memory_mb, 16384);
        } else {
            panic!("expected Resized");
        }
    }

    #[test]
    fn vm_spec_with_ssh_key_and_disk_size_serde_roundtrip() {
        let spec = VmSpec {
            id: VmId("vm-extended".to_string()),
            vcpus: 2,
            memory_mb: 1024,
            image: "ubuntu-24.04".to_string(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 user@host".to_string()),
            disk_size_mb: Some(20480),
            subnet: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: VmSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
        assert_eq!(
            back.ssh_key.as_deref(),
            Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 user@host")
        );
        assert_eq!(back.disk_size_mb, Some(20480));
    }

    #[test]
    fn vm_spec_without_new_fields_deserializes() {
        // Backward compatibility: JSON without ssh_key/disk_size_mb should deserialize
        let json = r#"{
            "id": "vm-old",
            "vcpus": 1,
            "memory_mb": 512,
            "image": "alpine",
            "kernel": null,
            "network": null,
            "volumes": [],
            "gpu": "None"
        }"#;
        let spec: VmSpec = serde_json::from_str(json).unwrap();
        assert!(spec.ssh_key.is_none());
        assert!(spec.disk_size_mb.is_none());
    }

    #[test]
    fn vm_list_shows_ip() {
        let status = VmStatus {
            vm_id: VmId("web-1".to_string()),
            phase: VmPhase::Running,
            vcpus: 2,
            memory_mb: 2048,
            image: Some("alpine-3.20".to_string()),
            runtime: None,
            created_at: Some(1700000000),
            uptime_secs: Some(60),
            ip: Some("10.1.1.3".to_string()),
            subnet: Some("frontend".to_string()),
            vpc: Some("default".to_string()),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["ip"].as_str(), Some("10.1.1.3"));
    }

    #[test]
    fn vm_get_shows_subnet() {
        let status = VmStatus {
            vm_id: VmId("db-1".to_string()),
            phase: VmPhase::Running,
            vcpus: 4,
            memory_mb: 8192,
            image: Some("ubuntu-24.04".to_string()),
            runtime: None,
            created_at: Some(1700000000),
            uptime_secs: Some(3600),
            ip: Some("10.1.2.3".to_string()),
            subnet: Some("database".to_string()),
            vpc: Some("default".to_string()),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["subnet"].as_str(), Some("database"));
    }

    #[test]
    fn vm_get_json_has_network_fields() {
        let status = VmStatus {
            vm_id: VmId("web-2".to_string()),
            phase: VmPhase::Running,
            vcpus: 2,
            memory_mb: 2048,
            image: Some("alpine-3.20".to_string()),
            runtime: None,
            created_at: Some(1700000000),
            uptime_secs: Some(120),
            ip: Some("10.1.1.4".to_string()),
            subnet: Some("frontend".to_string()),
            vpc: Some("prod-vpc".to_string()),
        };
        let json_str = serde_json::to_string(&status).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json["ip"].as_str(), Some("10.1.1.4"));
        assert_eq!(json["subnet"].as_str(), Some("frontend"));
        assert_eq!(json["vpc"].as_str(), Some("prod-vpc"));
    }

    #[test]
    fn vm_status_without_network_fields_deserializes() {
        // Backward compatibility: JSON without ip/subnet/vpc should deserialize
        let json = r#"{
            "vm_id": "vm-old",
            "phase": "Running",
            "vcpus": 2,
            "memory_mb": 1024,
            "image": "alpine",
            "runtime": null,
            "created_at": 1700000000,
            "uptime_secs": 60
        }"#;
        let status: VmStatus = serde_json::from_str(json).unwrap();
        assert!(status.ip.is_none());
        assert!(status.subnet.is_none());
        assert!(status.vpc.is_none());
    }
}
