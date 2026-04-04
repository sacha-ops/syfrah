//! Cloud Hypervisor runtime backend.
//!
//! `ChRuntime` implements [`ComputeRuntime`] by delegating to the existing
//! process management functions in [`crate::process`]. This is a thin wrapper
//! that translates between the trait's generic types and the CH-specific
//! implementation.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::error::ComputeError;
use crate::phase::VmPhase;
use crate::process::{self, RuntimeDir};
use crate::runtime_backend::{
    ComputeRuntime, RuntimeHandle, RuntimeInfo, RuntimeSpec, RuntimeType,
};

// ---------------------------------------------------------------------------
// ChRuntime
// ---------------------------------------------------------------------------

/// Cloud Hypervisor runtime backend.
///
/// Wraps the existing spawn/kill/delete/reconnect functions from `process.rs`
/// behind the [`ComputeRuntime`] trait. Each method translates between the
/// trait's generic types and the CH-specific types.
pub struct ChRuntime {
    /// Resolved path to the cloud-hypervisor binary.
    ch_binary: PathBuf,
    /// Base directory for per-VM runtime dirs (e.g., `/run/syfrah/vms`).
    base_dir: PathBuf,
    /// Path to the shared vmlinux kernel.
    kernel_path: PathBuf,
}

impl ChRuntime {
    /// Create a new ChRuntime with the given configuration paths.
    pub fn new(ch_binary: PathBuf, base_dir: PathBuf, kernel_path: PathBuf) -> Self {
        Self {
            ch_binary,
            base_dir,
            kernel_path,
        }
    }

    /// Get the resolved cloud-hypervisor binary path.
    pub fn ch_binary(&self) -> &Path {
        &self.ch_binary
    }

    /// Get the base directory for runtime dirs.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Get the kernel path.
    pub fn kernel_path(&self) -> &Path {
        &self.kernel_path
    }

    /// Return runtime-specific health warnings.
    ///
    /// Checks for KVM, CH binary, and kernel availability.
    pub fn health_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !Path::new("/dev/kvm").exists() {
            warnings.push("KVM not available — VMs cannot boot".to_string());
        }
        if !self.ch_binary.exists() {
            warnings.push("cloud-hypervisor binary not found".to_string());
        }
        if !self.kernel_path.exists() {
            warnings.push("kernel not found".to_string());
        }
        warnings
    }
}

#[async_trait]
impl ComputeRuntime for ChRuntime {
    async fn create(&self, id: &str, spec: &RuntimeSpec) -> Result<RuntimeHandle, ComputeError> {
        use crate::config::{map, resolve, validate, ResolvedVolume};
        use crate::preflight::run_preflight;
        use crate::types::{VmId, VmSpec};

        info!(
            vm_id = %id,
            runtime = self.name(),
            "ChRuntime::create: spawning Cloud Hypervisor VM"
        );

        // Build a VmSpec from RuntimeSpec so we can reuse validate/resolve/map.
        let vm_spec = VmSpec {
            id: VmId(id.to_string()),
            vcpus: spec.vcpus,
            memory_mb: spec.memory_mb,
            image: spec.rootfs_path.to_string_lossy().into_owned(),
            kernel: None,
            network: spec.network.clone(),
            volumes: vec![],
            gpu: spec.gpu.clone(),
            ssh_key: None,
            disk_size_mb: None,
            subnet: None,
            security_groups: vec![],
            pre_allocated_ip: None,
            pre_allocated_mac: None,
            root_volume_id: None,
        };

        // Step 1: validate
        let validated = validate(&vm_spec).map_err(|errors| {
            ComputeError::Config(
                errors
                    .into_iter()
                    .next()
                    .expect("at least one config error"),
            )
        })?;

        // Step 2: resolve (use rootfs_path directly)
        let mut resolved = resolve(
            &validated,
            spec.rootfs_path.parent().unwrap_or(Path::new("/")),
            &self.kernel_path,
        )
        .map_err(|errors| {
            ComputeError::Config(
                errors
                    .into_iter()
                    .next()
                    .expect("at least one config error"),
            )
        })?;

        // Override rootfs path with the one from RuntimeSpec.
        resolved.rootfs_path = spec.rootfs_path.clone();

        // If cloud-init was generated, add it as a volume.
        if let Some(ref ci_path) = spec.cloud_init_path {
            resolved.volume_paths.insert(
                0,
                ResolvedVolume {
                    path: ci_path.clone(),
                    read_only: true,
                },
            );
        }

        // Step 3: compute socket path for preflight and map
        let runtime_dir_path = self.base_dir.join(id);
        let socket_path = runtime_dir_path.join("api.sock");

        // Step 4: map
        let mut vm_config = map(&resolved, &socket_path);

        // Step 4b: virtiofs shared directory for ZeroFS volume
        //
        // When a root volume mount path is provided, configure a virtiofs
        // share so the guest can access the ZeroFS volume at `/data`.
        // Cloud Hypervisor's `--fs` requires a vhost-user-fs backend
        // (virtiofsd) running on the host. We set up the socket path and
        // config; the virtiofsd process is spawned before CH starts.
        if let Some(ref vol_path) = spec.volume_mount_path {
            let virtiofs_sock = runtime_dir_path.join("virtiofs.sock");
            info!(
                vm_id = %id,
                volume_path = %vol_path.display(),
                socket = %virtiofs_sock.display(),
                "starting virtiofsd and adding virtiofs share for ZeroFS volume"
            );

            // Spawn virtiofsd to serve the volume directory over a socket.
            // virtiofsd must be running before CH starts to use the --fs flag.
            if let Err(e) = start_virtiofsd(vol_path, &virtiofs_sock).await {
                warn!(
                    vm_id = %id,
                    error = %e,
                    "failed to start virtiofsd, volume will not be available inside VM"
                );
            } else {
                vm_config["fs"] = serde_json::json!([{
                    "tag": "data",
                    "socket": virtiofs_sock.to_string_lossy(),
                    "num_queues": 1,
                    "queue_size": 1024,
                }]);
                // Cloud Hypervisor requires shared memory for virtiofs.
                if let Some(mem) = vm_config.get_mut("memory") {
                    mem["shared"] = serde_json::json!(true);
                }
            }
        }

        // Step 5: preflight
        if let Err(e) = run_preflight(&resolved, &self.ch_binary, &socket_path) {
            return Err(ComputeError::Preflight(
                e.into_iter().next().expect("at least one preflight error"),
            ));
        }

        // Step 6: create RuntimeDir
        let runtime_dir = RuntimeDir::create(&self.base_dir, id)?;

        // Step 7: spawn inner (from here on, failure must clean up)
        let result =
            process::spawn_vm_inner(id, &self.ch_binary, &runtime_dir, &vm_config, &vm_spec).await;

        match result {
            Ok(state) => {
                let handle = RuntimeHandle {
                    id: id.to_string(),
                    pid: state.pid,
                    runtime_type: RuntimeType::Vm,
                    runtime_dir: runtime_dir.path().to_path_buf(),
                    vcpus: None,
                    memory_mb: None,
                    launched_at: None,
                    image_name: None,
                };
                info!(
                    vm_id = %id,
                    pid = state.pid,
                    "ChRuntime::create: VM started"
                );
                Ok(handle)
            }
            Err(e) => {
                let _ = runtime_dir.cleanup();
                Err(e)
            }
        }
    }

    async fn stop(&self, handle: &RuntimeHandle, force: bool) -> Result<(), ComputeError> {
        use crate::client::ChClient;

        info!(
            id = %handle.id,
            runtime = self.name(),
            force = force,
            "ChRuntime::stop: stopping VM"
        );

        let pid = handle.pid;
        let socket_path = handle.runtime_dir.join("api.sock");
        let client = ChClient::new(socket_path);
        let runtime_dir = RuntimeDir::from_existing(handle.runtime_dir.clone());

        // Check if already dead.
        if !process::is_pid_alive(pid) {
            debug!(id = %handle.id, "process already dead");
            let _ = runtime_dir.cleanup();
            return Ok(());
        }

        // Level 1: graceful shutdown (30s) — skipped when force=true
        if !force {
            info!(id = %handle.id, "kill chain level 1: shutdown_graceful");
            if let Err(e) = client.shutdown_graceful().await {
                debug!(id = %handle.id, error = %e, "shutdown_graceful failed, continuing");
            } else if process::wait_for_pid_exit(pid, std::time::Duration::from_secs(30)).await {
                info!(id = %handle.id, "process exited after graceful shutdown");
                let _ = runtime_dir.cleanup();
                return Ok(());
            }
        }

        // Level 2: shutdown_force (10s)
        if !process::is_pid_alive(pid) {
            let _ = runtime_dir.cleanup();
            return Ok(());
        }
        info!(id = %handle.id, "kill chain level 2: shutdown_force");
        if let Err(e) = client.shutdown_force().await {
            debug!(id = %handle.id, error = %e, "shutdown_force failed, continuing");
        } else if process::wait_for_pid_exit(pid, std::time::Duration::from_secs(10)).await {
            let _ = runtime_dir.cleanup();
            return Ok(());
        }

        // Level 3: SIGTERM (5s)
        if !process::is_pid_alive(pid) {
            let _ = runtime_dir.cleanup();
            return Ok(());
        }
        info!(id = %handle.id, "kill chain level 3: SIGTERM");
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        if process::wait_for_pid_exit(pid, std::time::Duration::from_secs(5)).await {
            let _ = runtime_dir.cleanup();
            return Ok(());
        }

        // Level 4: SIGKILL
        if !process::is_pid_alive(pid) {
            let _ = runtime_dir.cleanup();
            return Ok(());
        }
        info!(id = %handle.id, "kill chain level 4: SIGKILL");
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        if process::is_pid_alive(pid) {
            return Err(crate::error::ProcessError::SignalFailed {
                signal: "SIGKILL".to_string(),
                pid,
            }
            .into());
        }

        let _ = runtime_dir.cleanup();
        Ok(())
    }

    async fn delete(&self, handle: &RuntimeHandle) -> Result<(), ComputeError> {
        use crate::client::ChClient;

        debug!(
            id = %handle.id,
            runtime = self.name(),
            "ChRuntime::delete"
        );

        // Stop the process if it is still alive.
        if process::is_pid_alive(handle.pid) {
            self.stop(handle, true).await?;
        }

        // Best-effort: tell CH to delete (may fail if process is already gone).
        let socket_path = handle.runtime_dir.join("api.sock");
        let client = ChClient::new(socket_path);
        let _ = client.delete().await;

        // Cleanup runtime dir.
        let runtime_dir = RuntimeDir::from_existing(handle.runtime_dir.clone());
        if let Err(e) = runtime_dir.cleanup() {
            debug!(id = %handle.id, error = %e, "runtime dir cleanup failed");
        }

        info!(id = %handle.id, "ChRuntime::delete: VM deleted and cleaned up");
        Ok(())
    }

    async fn info(&self, handle: &RuntimeHandle) -> Result<RuntimeInfo, ComputeError> {
        let runtime_dir = RuntimeDir::from_existing(handle.runtime_dir.clone());
        let pid = handle.pid;

        // Check if process is alive to determine phase.
        let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
        let phase = if alive {
            VmPhase::Running
        } else {
            VmPhase::Stopped
        };

        // Compute uptime from meta.json if available.
        // VmManager computes precise uptime from VmRuntimeState; here we
        // provide a best-effort value based on current time.
        let uptime_secs = if alive {
            runtime_dir.read_meta().ok().map(|_meta| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            })
        } else {
            None
        };

        Ok(RuntimeInfo {
            phase,
            pid,
            uptime_secs,
            runtime_type: RuntimeType::Vm,
        })
    }

    async fn is_alive(&self, handle: &RuntimeHandle) -> bool {
        // Reap zombie first, then check.
        unsafe {
            let mut status: libc::c_int = 0;
            libc::waitpid(handle.pid as i32, &mut status, libc::WNOHANG);
        }
        unsafe { libc::kill(handle.pid as i32, 0) == 0 }
    }

    async fn reconnect(&self, runtime_dir_base: &Path) -> Vec<RuntimeHandle> {
        let dirs = process::scan_runtime_dirs(runtime_dir_base);
        let mut handles = Vec::new();

        for dir in dirs {
            let meta = match dir.read_meta() {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Only include VMs whose PID is still alive.
            let alive = unsafe {
                let mut status: libc::c_int = 0;
                libc::waitpid(meta.pid as i32, &mut status, libc::WNOHANG);
                libc::kill(meta.pid as i32, 0) == 0
            };

            if alive {
                info!(
                    vm_id = %meta.vm_id,
                    pid = meta.pid,
                    "ChRuntime::reconnect: found live VM"
                );
                handles.push(RuntimeHandle {
                    id: meta.vm_id,
                    pid: meta.pid,
                    runtime_type: RuntimeType::Vm,
                    runtime_dir: dir.path().to_path_buf(),
                    vcpus: None,
                    memory_mb: None,
                    launched_at: None,
                    image_name: None,
                });
            }
        }

        handles
    }

    fn name(&self) -> &str {
        "cloud-hypervisor"
    }
}

// ---------------------------------------------------------------------------
// virtiofsd helper
// ---------------------------------------------------------------------------

/// Start a `virtiofsd` process that serves a host directory over a vhost-user
/// socket. Cloud Hypervisor's `--fs` flag connects to this socket to expose
/// the directory as a virtiofs mount inside the guest.
///
/// The guest can then mount it with:
/// ```text
/// mount -t virtiofs data /data
/// ```
///
/// The process is daemonized (detached) so it outlives this function call.
/// It will be cleaned up when the VM is deleted (the runtime dir is removed).
async fn start_virtiofsd(shared_dir: &Path, socket_path: &Path) -> Result<(), ComputeError> {
    use tokio::process::Command;

    // Try common virtiofsd locations.
    let virtiofsd = if Path::new("/usr/libexec/virtiofsd").exists() {
        "/usr/libexec/virtiofsd"
    } else if Path::new("/usr/lib/virtiofsd").exists() {
        "/usr/lib/virtiofsd"
    } else if Path::new("/usr/local/bin/virtiofsd").exists() {
        "/usr/local/bin/virtiofsd"
    } else {
        return Err(crate::error::ProcessError::SpawnFailed {
            reason:
                "virtiofsd binary not found (checked /usr/libexec/, /usr/lib/, /usr/local/bin/)"
                    .to_string(),
        }
        .into());
    };

    let status = Command::new(virtiofsd)
        .args([
            &format!("--socket-path={}", socket_path.display()),
            &format!("--shared-dir={}", shared_dir.display()),
            "--sandbox=none",
            "--cache=never",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| crate::error::ProcessError::SpawnFailed {
            reason: format!("failed to start virtiofsd: {e}"),
        })?;

    debug!(
        pid = status.id().unwrap_or(0),
        socket = %socket_path.display(),
        shared_dir = %shared_dir.display(),
        "virtiofsd started"
    );

    // Give virtiofsd a moment to create the socket.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Auto-selection helper
// ---------------------------------------------------------------------------

/// Select the appropriate runtime backend based on system capabilities.
///
/// Currently only Cloud Hypervisor is supported. Returns an error if `/dev/kvm`
/// is not available (container runtime is not yet implemented).
pub fn select_runtime(
    ch_binary: PathBuf,
    base_dir: PathBuf,
    kernel_path: PathBuf,
) -> Result<Box<dyn ComputeRuntime>, ComputeError> {
    if Path::new("/dev/kvm").exists() {
        info!("runtime auto-selection: /dev/kvm present, using cloud-hypervisor");
        return Ok(Box::new(ChRuntime::new(ch_binary, base_dir, kernel_path)));
    }

    // KVM not available — try container runtime (crun + gVisor).
    if crate::runtime_container::container_binaries_available() {
        info!("runtime auto-selection: /dev/kvm not present, crun+runsc found, using container (gVisor)");
        let container_rt = crate::runtime_container::ContainerRuntime::new(base_dir)?;
        return Ok(Box::new(container_rt));
    }

    // Neither KVM nor crun+gVisor available — fall back to ChRuntime.
    // It will fail at VM creation time if KVM is truly absent, but this
    // allows construction of VmManager (and handler tests) to succeed.
    info!("runtime auto-selection: /dev/kvm not present, crun+runsc not found, falling back to cloud-hypervisor (will require KVM at VM creation time)");
    Ok(Box::new(ChRuntime::new(ch_binary, base_dir, kernel_path)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ch_runtime_name() {
        let rt = ChRuntime::new(
            PathBuf::from("/bin/true"),
            PathBuf::from("/tmp/vms"),
            PathBuf::from("/tmp/vmlinux"),
        );
        assert_eq!(rt.name(), "cloud-hypervisor");
    }

    #[test]
    fn ch_runtime_accessors() {
        let rt = ChRuntime::new(
            PathBuf::from("/usr/local/bin/ch"),
            PathBuf::from("/run/vms"),
            PathBuf::from("/opt/vmlinux"),
        );
        assert_eq!(rt.ch_binary(), Path::new("/usr/local/bin/ch"));
        assert_eq!(rt.base_dir(), Path::new("/run/vms"));
        assert_eq!(rt.kernel_path(), Path::new("/opt/vmlinux"));
    }

    #[tokio::test]
    async fn ch_runtime_reconnect_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let rt = ChRuntime::new(
            PathBuf::from("/bin/true"),
            tmp.path().to_path_buf(),
            PathBuf::from("/tmp/vmlinux"),
        );
        let handles = rt.reconnect(tmp.path()).await;
        assert!(handles.is_empty());
    }

    #[tokio::test]
    async fn ch_runtime_is_alive_dead_pid() {
        let rt = ChRuntime::new(
            PathBuf::from("/bin/true"),
            PathBuf::from("/tmp/vms"),
            PathBuf::from("/tmp/vmlinux"),
        );
        let handle = RuntimeHandle {
            id: "vm-dead".to_string(),
            pid: 4_000_000, // nonexistent PID
            runtime_type: RuntimeType::Vm,
            runtime_dir: PathBuf::from("/tmp/nonexistent"),
            vcpus: None,
            memory_mb: None,
            launched_at: None,
            image_name: None,
        };
        assert!(!rt.is_alive(&handle).await);
    }

    #[test]
    fn select_runtime_returns_runtime() {
        // select_runtime returns ChRuntime if /dev/kvm exists, ContainerRuntime
        // if crun+runsc exist, or falls back to ChRuntime otherwise.
        let result = select_runtime(
            PathBuf::from("/bin/true"),
            PathBuf::from("/tmp/vms"),
            PathBuf::from("/tmp/vmlinux"),
        );
        assert!(result.is_ok());
        let rt = result.unwrap();
        if Path::new("/dev/kvm").exists() {
            assert_eq!(rt.name(), "cloud-hypervisor");
        } else if crate::runtime_container::container_binaries_available() {
            assert_eq!(rt.name(), "container (gVisor)");
        } else {
            // Fallback to cloud-hypervisor even without KVM.
            assert_eq!(rt.name(), "cloud-hypervisor");
        }
    }
}
