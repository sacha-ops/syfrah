use std::path::PathBuf;

use crate::network::NetworkInfo;
use crate::phase::VmPhase;
use crate::runtime_backend::{RuntimeHandle, RuntimeType};
use crate::types::{VmId, VmStatus};

/// How this VM's runtime state was established.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum ReconnectSource {
    /// The VM was freshly spawned by this daemon instance.
    FreshSpawn,
    /// The VM was already running and recovered after a daemon restart.
    Recovered,
}

/// Compute-internal runtime state for a VM. Never leaked outside the crate.
///
/// This tracks everything compute needs to manage a Cloud Hypervisor process:
/// the OS PID, socket path, cgroup, binary version, health-check timestamps,
/// and the current lifecycle phase.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct VmRuntimeState {
    pub(crate) vm_id: VmId,
    pub(crate) pid: u32,
    pub(crate) socket_path: PathBuf,
    pub(crate) cgroup_path: Option<PathBuf>,
    pub(crate) ch_binary_path: PathBuf,
    pub(crate) ch_binary_version: String,
    /// Number of virtual CPUs allocated (from VmSpec).
    pub(crate) vcpus: u32,
    /// Memory allocation in megabytes (from VmSpec).
    pub(crate) memory_mb: u32,
    /// Unix timestamp when the VM was launched.
    pub(crate) launched_at: u64,
    /// Unix timestamp of the last successful health-check ping.
    pub(crate) last_ping_at: Option<u64>,
    /// Last error observed during health checks or lifecycle operations.
    pub(crate) last_error: Option<String>,
    pub(crate) current_phase: VmPhase,
    pub(crate) reconnect_source: ReconnectSource,
    /// Image name used to create this VM (for refcount tracking).
    pub(crate) image_name: Option<String>,
    /// Path to the instance directory (for cleanup on delete).
    pub(crate) instance_dir_path: Option<PathBuf>,
    /// Handle returned by the runtime backend that created this workload.
    pub(crate) runtime_handle: Option<RuntimeHandle>,
    /// IP address assigned to the VM (e.g. "10.1.1.3").
    pub(crate) ip: Option<String>,
    /// Subnet the VM belongs to (e.g. "frontend").
    pub(crate) subnet: Option<String>,
    /// VPC the VM belongs to (e.g. "default").
    pub(crate) vpc: Option<String>,
    /// Security groups attached to the VM's NIC.
    pub(crate) security_groups: Vec<String>,
    /// Network metadata for teardown on delete. None if VM has no networking.
    pub(crate) network_info: Option<NetworkInfo>,
    /// Hypervisor name/id this VM is running on.
    pub(crate) hypervisor_id: Option<String>,
    /// Region of the hypervisor.
    pub(crate) region: Option<String>,
    /// Zone of the hypervisor.
    pub(crate) zone: Option<String>,
}

#[allow(dead_code)]
impl VmRuntimeState {
    /// Build a `RuntimeHandle` from this state, using the stored handle if
    /// available, or constructing one from the pid / base_dir.
    pub(crate) fn to_runtime_handle(&self, base_dir: &std::path::Path) -> RuntimeHandle {
        if let Some(ref h) = self.runtime_handle {
            return h.clone();
        }
        RuntimeHandle {
            id: self.vm_id.0.clone(),
            pid: self.pid,
            runtime_type: RuntimeType::Vm,
            runtime_dir: base_dir.join(&self.vm_id.0),
            vcpus: None,
            memory_mb: None,
            launched_at: None,
            image_name: None,
        }
    }

    /// Produce the public external view of this VM's state.
    ///
    /// This is what forge and the control plane see. Internal details like
    /// PID, socket path, cgroup, and reconnect source are deliberately omitted.
    pub(crate) fn to_status(&self, now_unix: u64) -> VmStatus {
        let uptime_secs = if self.current_phase == VmPhase::Running {
            Some(now_unix.saturating_sub(self.launched_at))
        } else {
            None
        };

        VmStatus {
            vm_id: self.vm_id.clone(),
            phase: self.current_phase,
            vcpus: self.vcpus,
            memory_mb: self.memory_mb,
            image: self.image_name.clone(),
            runtime: self.runtime_handle.as_ref().map(|h| h.runtime_type),
            created_at: Some(self.launched_at),
            uptime_secs,
            ip: self.ip.clone(),
            subnet: self.subnet.clone(),
            vpc: self.vpc.clone(),
            security_groups: self.security_groups.clone(),
            hypervisor_id: self.hypervisor_id.clone(),
            region: self.region.clone(),
            zone: self.zone.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_runtime(phase: VmPhase) -> VmRuntimeState {
        VmRuntimeState {
            vm_id: VmId("vm-rt-1".to_string()),
            pid: 4242,
            socket_path: PathBuf::from("/tmp/ch-vm-rt-1.sock"),
            cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/syfrah/vm-rt-1")),
            ch_binary_path: PathBuf::from("/usr/bin/cloud-hypervisor"),
            ch_binary_version: "38.0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 1_700_000_000,
            last_ping_at: Some(1_700_000_060),
            last_error: None,
            current_phase: phase,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
            ip: None,
            subnet: None,
            vpc: None,
            security_groups: vec![],
            network_info: None,
            hypervisor_id: None,
            region: None,
            zone: None,
        }
    }

    #[test]
    fn to_status_running_has_uptime() {
        let rt = sample_runtime(VmPhase::Running);
        let now = 1_700_000_100;
        let status = rt.to_status(now);
        assert_eq!(status.vm_id, VmId("vm-rt-1".to_string()));
        assert_eq!(status.phase, VmPhase::Running);
        assert_eq!(status.uptime_secs, Some(100));
        assert_eq!(status.created_at, Some(1_700_000_000));
    }

    #[test]
    fn to_status_stopped_has_no_uptime() {
        let rt = sample_runtime(VmPhase::Stopped);
        let status = rt.to_status(1_700_000_200);
        assert_eq!(status.phase, VmPhase::Stopped);
        assert_eq!(status.uptime_secs, None);
    }

    #[test]
    fn to_status_pending_has_no_uptime() {
        let rt = sample_runtime(VmPhase::Pending);
        let status = rt.to_status(1_700_000_050);
        assert_eq!(status.uptime_secs, None);
    }

    #[test]
    fn reconnect_source_clone_and_debug() {
        let fresh = ReconnectSource::FreshSpawn;
        let cloned = fresh.clone();
        assert!(format!("{cloned:?}").contains("FreshSpawn"));

        let recovered = ReconnectSource::Recovered;
        assert!(format!("{recovered:?}").contains("Recovered"));
    }
}
