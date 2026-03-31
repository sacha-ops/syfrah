//! Trait abstraction for compute runtime backends.
//!
//! `ComputeRuntime` defines the interface that both Cloud Hypervisor (VM) and
//! future container (crun+gVisor) backends must implement. This allows
//! `VmManager` to be backend-agnostic.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ComputeError;
use crate::phase::VmPhase;
use crate::types::{GpuMode, NetworkConfig};

// ---------------------------------------------------------------------------
// RuntimeSpec — common input for creating a workload
// ---------------------------------------------------------------------------

/// Specification for creating a workload through any runtime backend.
///
/// This is the backend-agnostic counterpart of `VmSpec`. The manager translates
/// a `VmSpec` into a `RuntimeSpec` before calling `ComputeRuntime::create`.
#[derive(Debug, Clone)]
pub struct RuntimeSpec {
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Memory allocation in megabytes.
    pub memory_mb: u32,
    /// Path to the root filesystem (.raw for VM, OCI dir for container).
    pub rootfs_path: PathBuf,
    /// Optional path to a cloud-init config drive.
    pub cloud_init_path: Option<PathBuf>,
    /// Network configuration.
    pub network: Option<NetworkConfig>,
    /// GPU passthrough mode.
    pub gpu: GpuMode,
    /// Image name (passed through so container meta can persist it for reconnect).
    pub image_name: Option<String>,
    /// SSH public key to inject into the workload for root access.
    pub ssh_public_key: Option<String>,
}

// ---------------------------------------------------------------------------
// RuntimeHandle — common output identifying a running workload
// ---------------------------------------------------------------------------

/// Handle to a running workload, returned by `ComputeRuntime::create`.
#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    /// Workload identifier (same as the VM/container ID).
    pub id: String,
    /// OS-level process ID of the runtime process.
    pub pid: u32,
    /// Which backend is managing this workload.
    pub runtime_type: RuntimeType,
    /// Path to the runtime directory containing socket, PID file, metadata.
    pub runtime_dir: PathBuf,
    /// Number of virtual CPUs (populated from metadata during reconnect).
    pub vcpus: Option<u32>,
    /// Memory allocation in megabytes (populated from metadata during reconnect).
    pub memory_mb: Option<u32>,
    /// Original launch time as Unix epoch seconds (populated from metadata during reconnect).
    pub launched_at: Option<u64>,
    /// Image name used to create this workload (populated from metadata during reconnect).
    pub image_name: Option<String>,
}

// ---------------------------------------------------------------------------
// RuntimeType — discriminator for backend type
// ---------------------------------------------------------------------------

/// Identifies which runtime backend manages a workload.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeType {
    /// Cloud Hypervisor VM.
    Vm,
    /// Container (crun + gVisor). Reserved for future use.
    Container,
}

impl std::fmt::Display for RuntimeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeType::Vm => f.write_str("vm"),
            RuntimeType::Container => f.write_str("container"),
        }
    }
}

impl std::str::FromStr for RuntimeType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "vm" => Ok(RuntimeType::Vm),
            "container" => Ok(RuntimeType::Container),
            other => Err(format!("unknown runtime type: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// RuntimeInfo — status information about a workload
// ---------------------------------------------------------------------------

/// Status information about a running workload.
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    /// Current lifecycle phase.
    pub phase: VmPhase,
    /// OS-level process ID.
    pub pid: u32,
    /// Seconds since the workload was started.
    pub uptime_secs: Option<u64>,
    /// Which backend manages this workload.
    pub runtime_type: RuntimeType,
}

// ---------------------------------------------------------------------------
// ComputeRuntime trait
// ---------------------------------------------------------------------------

/// Backend-agnostic interface for managing compute workloads.
///
/// Implementations wrap a specific runtime (Cloud Hypervisor, crun+gVisor)
/// and translate the common `RuntimeSpec` into backend-specific configuration.
#[async_trait]
pub trait ComputeRuntime: Send + Sync {
    /// Create and start a workload.
    async fn create(&self, id: &str, spec: &RuntimeSpec) -> Result<RuntimeHandle, ComputeError>;

    /// Start a stopped workload.
    ///
    /// Not all backends support restarting. The default returns an error.
    async fn start(&self, handle: &RuntimeHandle) -> Result<RuntimeHandle, ComputeError> {
        let _ = handle;
        Err(crate::error::ProcessError::SpawnFailed {
            reason: "start not supported by this runtime backend".to_string(),
        }
        .into())
    }

    /// Stop a running workload.
    ///
    /// When `force` is true, skip the graceful shutdown phase.
    async fn stop(&self, handle: &RuntimeHandle, force: bool) -> Result<(), ComputeError>;

    /// Delete a workload and clean up all artifacts.
    async fn delete(&self, handle: &RuntimeHandle) -> Result<(), ComputeError>;

    /// Get workload status information.
    async fn info(&self, handle: &RuntimeHandle) -> Result<RuntimeInfo, ComputeError>;

    /// Check whether the workload process is still alive.
    async fn is_alive(&self, handle: &RuntimeHandle) -> bool;

    /// Reconnect to existing workloads after a daemon restart.
    ///
    /// Scans the given runtime directory base for recoverable workloads.
    async fn reconnect(&self, runtime_dir: &Path) -> Vec<RuntimeHandle>;

    /// Human-readable name for this runtime backend (e.g., "cloud-hypervisor").
    fn name(&self) -> &str;

    /// Return runtime-specific health warnings.
    ///
    /// Each runtime checks its own prerequisites (e.g., ChRuntime checks KVM,
    /// CH binary, and kernel; ContainerRuntime checks crun and runsc).
    fn health_warnings(&self) -> Vec<String> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_type_display() {
        assert_eq!(RuntimeType::Vm.to_string(), "vm");
        assert_eq!(RuntimeType::Container.to_string(), "container");
    }

    #[test]
    fn runtime_type_from_str() {
        assert_eq!("vm".parse::<RuntimeType>(), Ok(RuntimeType::Vm));
        assert_eq!(
            "container".parse::<RuntimeType>(),
            Ok(RuntimeType::Container)
        );
        assert!("unknown".parse::<RuntimeType>().is_err());
    }

    #[test]
    fn runtime_type_serde_roundtrip() {
        let vm = RuntimeType::Vm;
        let json = serde_json::to_string(&vm).unwrap();
        let back: RuntimeType = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);

        let container = RuntimeType::Container;
        let json = serde_json::to_string(&container).unwrap();
        let back: RuntimeType = serde_json::from_str(&json).unwrap();
        assert_eq!(container, back);
    }

    #[test]
    fn runtime_spec_clone() {
        let spec = RuntimeSpec {
            vcpus: 4,
            memory_mb: 2048,
            rootfs_path: PathBuf::from("/tmp/rootfs.raw"),
            cloud_init_path: None,
            network: None,
            gpu: GpuMode::None,
            image_name: None,
            ssh_public_key: None,
        };
        let cloned = spec.clone();
        assert_eq!(cloned.vcpus, 4);
        assert_eq!(cloned.memory_mb, 2048);
    }

    #[test]
    fn runtime_handle_clone() {
        let handle = RuntimeHandle {
            id: "vm-1".to_string(),
            pid: 1234,
            runtime_type: RuntimeType::Vm,
            runtime_dir: PathBuf::from("/run/syfrah/vms/vm-1"),
            vcpus: None,
            memory_mb: None,
            launched_at: None,
            image_name: None,
        };
        let cloned = handle.clone();
        assert_eq!(cloned.id, "vm-1");
        assert_eq!(cloned.pid, 1234);
        assert_eq!(cloned.runtime_type, RuntimeType::Vm);
    }

    #[test]
    fn runtime_info_debug() {
        let info = RuntimeInfo {
            phase: VmPhase::Running,
            pid: 5678,
            uptime_secs: Some(120),
            runtime_type: RuntimeType::Vm,
        };
        let debug = format!("{info:?}");
        assert!(debug.contains("Running"));
        assert!(debug.contains("5678"));
    }
}
