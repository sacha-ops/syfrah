use crate::types::VmId;

/// Top-level error type for the compute layer.
///
/// Forge pattern-matches on the variant to decide how to handle each error:
/// - User error (Config, Preflight) -> return to caller
/// - Infra error (Client, Process) -> retry or alert
/// - Transient error (Concurrency) -> retry with backoff
/// - Bug (Transition) -> log and escalate
#[derive(Debug, thiserror::Error)]
pub enum ComputeError {
    #[error("preflight check failed: {0}")]
    Preflight(#[from] PreflightError),

    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("cloud hypervisor client error: {0}")]
    Client(#[from] ClientError),

    #[error("process management error: {0}")]
    Process(#[from] ProcessError),

    #[error("invalid state transition: {0}")]
    Transition(#[from] TransitionError),

    #[error("concurrency error: {0}")]
    Concurrency(#[from] ConcurrencyError),

    #[error("VM {id} not found")]
    VmNotFound { id: String },

    #[error("image error: {0}")]
    Image(#[from] crate::image::error::ImageError),

    #[error("network setup error: {0}")]
    NetworkSetup(String),
}

/// Precondition not met before spawning a VM.
///
/// The preflight validator collects all failures in a single pass and returns
/// them together as `Vec<PreflightError>`.
#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("cloud-hypervisor binary not found (checked /usr/local/lib/syfrah/ and $PATH) — reinstall syfrah or set compute.ch_binary in config")]
    ChBinaryNotFound,

    #[error("/dev/kvm is not available — enable virtualization in BIOS and run `modprobe kvm_intel` (or `kvm_amd`)")]
    KvmNotAvailable,

    #[error("kernel image not found")]
    KernelNotFound,

    #[error("disk image not found")]
    ImageNotFound,

    #[error("TAP device not found")]
    TapDeviceNotFound,

    #[error("VFIO device not bound for PCI address {bdf}")]
    VfioNotBound { bdf: String },

    #[error("cgroup v2 is not available")]
    CgroupV2NotAvailable,

    #[error("socket path already occupied: {path}")]
    SocketPathOccupied { path: String },

    #[error("insufficient {resource}: available {available}, required {required}")]
    InsufficientResources {
        resource: String,
        available: String,
        required: String,
    },
}

/// Invalid or unresolvable VM spec.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid vcpu count: {value}")]
    InvalidVcpuCount { value: u32 },

    #[error("invalid memory size: {value} MB")]
    InvalidMemory { value: u32 },

    #[error("unknown image: {name}")]
    UnknownImage { name: String },

    #[error("invalid PCI BDF address: {bdf}")]
    InvalidBdf { bdf: String },

    #[error("kernel path is required but not provided")]
    MissingKernel,

    #[error("empty volume path at index {index}")]
    EmptyVolumePath { index: usize },

    #[error("empty TAP device name")]
    EmptyTapName,

    #[error("conflicting settings: {detail}")]
    ConflictingSettings { detail: String },
}

/// Cloud Hypervisor REST API call failure.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("API socket not found: {path}")]
    SocketNotFound { path: String },

    #[error("connection refused")]
    ConnectionRefused,

    #[error("timeout during {operation}")]
    Timeout { operation: String },

    #[error("unexpected HTTP status {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },

    #[error("invalid response: {detail}")]
    InvalidResponse { detail: String },
}

/// OS-level process management failure.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("failed to spawn process: {reason}")]
    SpawnFailed { reason: String },

    #[error("PID {pid} not found")]
    PidNotFound { pid: u32 },

    #[error("cgroup error: {detail}")]
    CgroupError { detail: String },

    #[error("failed to send signal {signal} to PID {pid}")]
    SignalFailed { signal: String, pid: u32 },

    #[error("orphan cleanup failed for {vm_id}: {reason}")]
    OrphanCleanupFailed { vm_id: String, reason: String },

    #[error("reconnect failed for {vm_id}: {reason}")]
    ReconnectFailed { vm_id: String, reason: String },
}

pub use crate::phase::TransitionError;

/// Operation blocked by a concurrent operation on the same VM.
#[derive(Debug, thiserror::Error)]
#[error("operation on VM {vm_id} blocked by {blocked_by}")]
pub struct ConcurrencyError {
    pub vm_id: VmId,
    pub blocked_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- PreflightError display -----------------------------------------------

    #[test]
    fn preflight_ch_binary_not_found_display() {
        let e = PreflightError::ChBinaryNotFound;
        assert!(e.to_string().contains("cloud-hypervisor binary not found"));
        assert!(e.to_string().contains("reinstall syfrah"));
    }

    #[test]
    fn preflight_vfio_not_bound_display() {
        let e = PreflightError::VfioNotBound {
            bdf: "0000:03:00.0".to_string(),
        };
        let msg = e.to_string();
        assert!(msg.contains("0000:03:00.0"));
        assert!(msg.contains("VFIO"));
    }

    #[test]
    fn preflight_insufficient_resources_display() {
        let e = PreflightError::InsufficientResources {
            resource: "memory".to_string(),
            available: "4096 MB".to_string(),
            required: "8192 MB".to_string(),
        };
        let msg = e.to_string();
        assert!(msg.contains("memory"));
        assert!(msg.contains("4096 MB"));
        assert!(msg.contains("8192 MB"));
    }

    // -- ConfigError display --------------------------------------------------

    #[test]
    fn config_invalid_vcpu_display() {
        let e = ConfigError::InvalidVcpuCount { value: 0 };
        assert!(e.to_string().contains('0'));
    }

    #[test]
    fn config_missing_kernel_display() {
        let e = ConfigError::MissingKernel;
        assert!(e.to_string().contains("kernel"));
    }

    #[test]
    fn config_conflicting_settings_display() {
        let e = ConfigError::ConflictingSettings {
            detail: "gpu and nested virt".to_string(),
        };
        assert!(e.to_string().contains("gpu and nested virt"));
    }

    // -- ClientError display --------------------------------------------------

    #[test]
    fn client_connection_refused_display() {
        let e = ClientError::ConnectionRefused;
        assert_eq!(e.to_string(), "connection refused");
    }

    #[test]
    fn client_timeout_display() {
        let e = ClientError::Timeout {
            operation: "boot".to_string(),
        };
        assert!(e.to_string().contains("boot"));
    }

    #[test]
    fn client_unexpected_status_display() {
        let e = ClientError::UnexpectedStatus {
            status: 500,
            body: "internal error".to_string(),
        };
        let msg = e.to_string();
        assert!(msg.contains("500"));
        assert!(msg.contains("internal error"));
    }

    // -- ProcessError display -------------------------------------------------

    #[test]
    fn process_spawn_failed_display() {
        let e = ProcessError::SpawnFailed {
            reason: "permission denied".to_string(),
        };
        assert!(e.to_string().contains("permission denied"));
    }

    #[test]
    fn process_signal_failed_display() {
        let e = ProcessError::SignalFailed {
            signal: "SIGTERM".to_string(),
            pid: 12345,
        };
        let msg = e.to_string();
        assert!(msg.contains("SIGTERM"));
        assert!(msg.contains("12345"));
    }

    // -- TransitionError display ----------------------------------------------

    #[test]
    fn transition_error_display() {
        use crate::phase::VmPhase;
        let e = TransitionError {
            from: VmPhase::Running,
            to: VmPhase::Pending,
        };
        let msg = e.to_string();
        assert!(msg.contains("Running"));
        assert!(msg.contains("Pending"));
    }

    // -- ComputeError From impls ----------------------------------------------

    #[test]
    fn compute_error_from_preflight() {
        let inner = PreflightError::KvmNotAvailable;
        let outer: ComputeError = inner.into();
        assert!(matches!(outer, ComputeError::Preflight(_)));
        assert!(outer.to_string().contains("preflight"));
    }

    #[test]
    fn compute_error_from_config() {
        let inner = ConfigError::InvalidMemory { value: 0 };
        let outer: ComputeError = inner.into();
        assert!(matches!(outer, ComputeError::Config(_)));
        assert!(outer.to_string().contains("configuration"));
    }

    #[test]
    fn compute_error_from_client() {
        let inner = ClientError::ConnectionRefused;
        let outer: ComputeError = inner.into();
        assert!(matches!(outer, ComputeError::Client(_)));
    }

    #[test]
    fn compute_error_from_process() {
        let inner = ProcessError::PidNotFound { pid: 99 };
        let outer: ComputeError = inner.into();
        assert!(matches!(outer, ComputeError::Process(_)));
    }

    #[test]
    fn compute_error_from_transition() {
        use crate::phase::VmPhase;
        let inner = TransitionError {
            from: VmPhase::Pending,
            to: VmPhase::Running,
        };
        let outer: ComputeError = inner.into();
        assert!(matches!(outer, ComputeError::Transition(_)));
    }

    #[test]
    fn compute_error_from_concurrency() {
        let inner = ConcurrencyError {
            vm_id: VmId("vm-1".to_string()),
            blocked_by: "delete".to_string(),
        };
        let outer: ComputeError = inner.into();
        assert!(matches!(outer, ComputeError::Concurrency(_)));
        assert!(outer.to_string().contains("vm-1"));
    }
}
