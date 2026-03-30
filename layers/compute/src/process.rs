use std::collections::HashMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::client::ChClient;
use crate::config::{map, resolve, validate};
use crate::disk;
use crate::error::{ComputeError, ProcessError};
use crate::events;
use crate::image;
use crate::image::error::ImageError;
use crate::image::store::ImageStore;
use crate::image::types::{CloudInitConfig, ImageCatalog, ImageMeta, InstanceId, PullPolicy};
use crate::phase::VmPhase;
use crate::preflight::run_preflight;
use crate::runtime::{ReconnectSource, VmRuntimeState};
use crate::runtime_backend::RuntimeType;
use crate::types::{VmEvent, VmId, VmSpec};

// ---------------------------------------------------------------------------
// RuntimeDir (#475)
// ---------------------------------------------------------------------------

/// Manages the per-VM runtime directory at `/run/syfrah/vms/{id}/`.
///
/// Contains all artifacts needed for process management and reconnect:
/// `api.sock`, `pid`, `meta.json`, `ch-version`, `stdout.log`.
pub struct RuntimeDir {
    base: PathBuf,
}

impl RuntimeDir {
    /// Create a new runtime directory for a VM.
    ///
    /// Creates `{base_dir}/{vm_id}/` with 0o700 permissions.
    pub fn create(base_dir: &Path, vm_id: &str) -> Result<Self, ProcessError> {
        let base = base_dir.join(vm_id);
        fs::create_dir_all(&base).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to create runtime dir {}: {e}", base.display()),
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).map_err(|e| {
                ProcessError::SpawnFailed {
                    reason: format!("failed to set permissions on {}: {e}", base.display()),
                }
            })?;
        }

        Ok(Self { base })
    }

    /// Wrap an existing runtime directory path.
    pub fn from_existing(path: PathBuf) -> Self {
        Self { base: path }
    }

    /// Path to the Cloud Hypervisor API socket.
    pub fn socket_path(&self) -> PathBuf {
        self.base.join("api.sock")
    }

    /// Path to the PID file.
    pub fn pid_path(&self) -> PathBuf {
        self.base.join("pid")
    }

    /// Path to the metadata file.
    pub fn meta_path(&self) -> PathBuf {
        self.base.join("meta.json")
    }

    /// Path to the CH version file.
    pub fn ch_version_path(&self) -> PathBuf {
        self.base.join("ch-version")
    }

    /// Path to the stdout/stderr log file.
    pub fn log_path(&self) -> PathBuf {
        self.base.join("stdout.log")
    }

    /// The runtime directory itself.
    pub fn path(&self) -> &Path {
        &self.base
    }

    /// Write meta.json atomically (write to .tmp, fsync, then rename).
    pub fn write_meta(&self, meta: &VmMeta) -> Result<(), ProcessError> {
        let tmp_path = self.base.join(".meta.json.tmp");
        let final_path = self.meta_path();

        let json = serde_json::to_string_pretty(meta).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to serialize meta.json: {e}"),
        })?;

        let mut f = fs::File::create(&tmp_path).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to create {}: {e}", tmp_path.display()),
        })?;
        f.write_all(json.as_bytes())
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to write {}: {e}", tmp_path.display()),
            })?;
        f.sync_all().map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to sync {}: {e}", tmp_path.display()),
        })?;

        fs::rename(&tmp_path, &final_path).map_err(|e| ProcessError::SpawnFailed {
            reason: format!(
                "failed to rename {} -> {}: {e}",
                tmp_path.display(),
                final_path.display()
            ),
        })?;

        Ok(())
    }

    /// Read and parse meta.json.
    pub fn read_meta(&self) -> Result<VmMeta, ProcessError> {
        let path = self.meta_path();
        let data = fs::read_to_string(&path).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to read {}: {e}", path.display()),
        })?;
        serde_json::from_str(&data).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to parse {}: {e}", path.display()),
        })
    }

    /// Write the PID file.
    pub fn write_pid(&self, pid: u32) -> Result<(), ProcessError> {
        fs::write(self.pid_path(), pid.to_string()).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to write pid file: {e}"),
        })
    }

    /// Read the PID from the PID file.
    pub fn read_pid(&self) -> Result<u32, ProcessError> {
        let data = fs::read_to_string(self.pid_path()).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to read pid file: {e}"),
        })?;
        data.trim().parse().map_err(|e| ProcessError::SpawnFailed {
            reason: format!("invalid pid file content: {e}"),
        })
    }

    /// Write the CH version file.
    pub fn write_ch_version(&self, version: &str) -> Result<(), ProcessError> {
        fs::write(self.ch_version_path(), version).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to write ch-version file: {e}"),
        })
    }

    /// Remove the entire runtime directory recursively.
    pub fn cleanup(&self) -> Result<(), ProcessError> {
        if self.base.exists() {
            // Internal event: SocketRemoved (runtime dir includes the API socket)
            debug!(path = %self.base.display(), "SocketRemoved: cleaning up runtime dir");
            fs::remove_dir_all(&self.base).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to remove runtime dir {}: {e}", self.base.display()),
            })?;
        }
        Ok(())
    }

    /// Check whether the runtime directory exists.
    pub fn exists(&self) -> bool {
        self.base.exists()
    }
}

/// Metadata stored in meta.json for reconnect after daemon restart.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VmMeta {
    pub vm_id: String,
    pub created_at: String,
    pub socket_path: String,
    pub pid: u32,
    pub ch_binary: String,
    pub ch_version: String,
    pub spec_hash: String,
    /// Number of virtual CPUs (persisted for reconnect).
    #[serde(default)]
    pub vcpus: u32,
    /// Memory in megabytes (persisted for reconnect).
    #[serde(default)]
    pub memory_mb: u32,
    /// Image name used to create this VM.
    #[serde(default)]
    pub image_name: Option<String>,
    /// Disk size in megabytes (persisted for reconnect).
    #[serde(default)]
    pub disk_size_mb: Option<u32>,
}

/// Scan a base directory for runtime dirs that contain meta.json.
///
/// Returns a `RuntimeDir` for each subdirectory that has a valid meta.json.
pub fn scan_runtime_dirs(base: &Path) -> Vec<RuntimeDir> {
    let entries = match fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("meta.json").exists() {
            dirs.push(RuntimeDir::from_existing(path));
        }
    }
    dirs
}

// ---------------------------------------------------------------------------
// PID helpers
// ---------------------------------------------------------------------------

/// Check whether a process with the given PID is alive.
///
/// First attempts `waitpid(WNOHANG)` to reap zombie children (the CH process
/// is our direct child but its `Child` handle was forgotten to avoid pidfd
/// issues). Without this, zombies appear alive to `kill(pid, 0)`.
pub(crate) fn is_pid_alive(pid: u32) -> bool {
    // SAFETY: waitpid with WNOHANG is non-blocking and only reaps our own children.
    unsafe {
        let mut status: libc::c_int = 0;
        libc::waitpid(pid as i32, &mut status, libc::WNOHANG);
    }
    // SAFETY: kill with signal 0 only checks process existence, no signal is sent.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Poll every 200ms until the PID exits or timeout is reached.
/// Returns `true` if the process exited within the timeout.
pub(crate) async fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let poll_interval = Duration::from_millis(200);
    let start = tokio::time::Instant::now();

    loop {
        if !is_pid_alive(pid) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Get the current time as an ISO 8601 string (UTC).
fn now_iso8601() -> String {
    // Use a simple approach: seconds since epoch formatted as ISO 8601.
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Simple UTC timestamp without external crate
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Calculate year/month/day from days since epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm based on Howard Hinnant's civil_from_days
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as u64, m, d)
}

/// Get the current Unix timestamp.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Spawn (#476)
// ---------------------------------------------------------------------------

/// Map the Rust `std::env::consts::ARCH` to the image catalog arch convention.
/// Rust uses "x86_64" / "aarch64", catalog uses "amd64" / "arm64".
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Spawn a VM through the full pipeline.
///
/// Steps:
/// 1. validate(spec) -> ValidatedSpec
/// 2. Image check/pull (if image_store is Some)
/// 3. Arch validation
/// 4. Create instance dir, clone image, generate cloud-init
/// 5. resolve(validated, image_dir, default_kernel) -> ResolvedSpec
///    (with instance paths when image management is enabled)
/// 6. map(resolved, socket_path) -> VmConfig JSON
/// 7. run_preflight(resolved, ch_binary, socket_path)
/// 8. Create RuntimeDir
/// 9. Spawn cloud-hypervisor process
/// 10. Write pid, meta.json, ch-version
/// 11. Poll ping() until ready (100ms interval, 10s timeout)
/// 12. client.create(vm_config)
/// 13. client.boot()
/// 14. Return VmRuntimeState with phase Running
///
/// On ANY failure: cleanup (kill process if spawned, remove runtime dir,
/// remove instance dir).
#[allow(clippy::too_many_arguments)]
pub async fn spawn_vm(
    spec: &VmSpec,
    ch_binary: &Path,
    base_dir: &Path,
    image_dir: &Path,
    default_kernel: &Path,
    image_store: Option<&ImageStore>,
    catalog: &ImageCatalog,
    pull_policy: &PullPolicy,
    instance_base: &Path,
) -> Result<VmRuntimeState, ComputeError> {
    let vm_id_str = spec.id.0.clone();
    info!(vm_id = %vm_id_str, "starting VM spawn");

    // Step 1: validate
    let validated = validate(spec).map_err(|errors| {
        ComputeError::Config(
            errors
                .into_iter()
                .next()
                .expect("at least one config error"),
        )
    })?;

    // -- Image management steps (only when image_store is provided) -----------
    let mut instance_dir_path: Option<PathBuf> = None;
    let mut instance_rootfs: Option<PathBuf> = None;
    let mut instance_cloud_init: Option<PathBuf> = None;

    if let Some(store) = image_store {
        // Step 2: Image check/pull
        let image_meta: ImageMeta = match store.get(&spec.image)? {
            Some(meta) => {
                info!(image = %spec.image, "image found in local cache");
                meta
            }
            None if *pull_policy != PullPolicy::Never => {
                info!(image = %spec.image, "pulling image from catalog");
                image::pull::pull(store, &spec.image, catalog).await?
            }
            None => {
                return Err(ImageError::ImageNotFound {
                    name: spec.image.clone(),
                }
                .into());
            }
        };

        // Step 3: Arch validation
        // Treat "unknown" arch (from scan) as compatible to avoid false rejections.
        if image_meta.arch != "unknown" && image_meta.arch != node_arch() {
            return Err(ImageError::ArchMismatch {
                image_arch: image_meta.arch.clone(),
                node_arch: node_arch().to_string(),
            }
            .into());
        }

        // Step 4a: Create instance dir
        let instance_id = InstanceId::new();
        let inst_dir = disk::InstanceDir::create(instance_base, &instance_id)?;
        let inst_path = inst_dir.path().to_path_buf();

        // Step 4b: Clone base image (cleanup on failure)
        let base_image_path = store.image_path(&spec.image);
        let min_disk = image_meta.min_disk_mb as u32;
        let effective_size_bytes =
            match disk::clone_image(&base_image_path, &inst_dir, spec.disk_size_mb, min_disk) {
                Ok(size) => {
                    info!(
                        vm_id = %vm_id_str,
                        instance = %instance_id,
                        "image cloned to instance dir"
                    );
                    size
                }
                Err(e) => {
                    inst_dir.cleanup().ok();
                    return Err(e.into());
                }
            };

        // Step 4c: Generate cloud-init (if applicable)
        if image_meta.cloud_init && spec.ssh_key.is_some() {
            let cloud_config = CloudInitConfig {
                hostname: vm_id_str.clone(),
                ssh_authorized_keys: vec![spec.ssh_key.clone().unwrap()],
                default_user: image_meta
                    .default_username
                    .clone()
                    .unwrap_or_else(|| "ubuntu".to_string()),
                users: vec![],
                network_config: None,
                user_data_extra: None,
            };
            match disk::generate_cloud_init(&cloud_config, &inst_dir, &instance_id) {
                Ok(ci_path) => {
                    info!(vm_id = %vm_id_str, "cloud-init config-drive generated");
                    instance_cloud_init = Some(ci_path);
                }
                Err(e) => {
                    inst_dir.cleanup().ok();
                    return Err(e.into());
                }
            }
        }

        // Step 4d: Write instance metadata
        let effective_mb = (effective_size_bytes / (1024 * 1024)) as u32;
        let inst_meta = disk::InstanceMeta {
            image_source: spec.image.clone(),
            image_sha: image_meta.sha256.clone(),
            arch: image_meta.arch.clone(),
            requested_disk_size_mb: spec.disk_size_mb,
            effective_disk_size_mb: effective_mb,
            hostname: vm_id_str.clone(),
            created_at: now_iso8601(),
            vm_name: vm_id_str.clone(),
        };
        if let Err(e) = inst_dir.write_metadata(&inst_meta) {
            inst_dir.cleanup().ok();
            return Err(e.into());
        }

        instance_rootfs = Some(inst_dir.rootfs_path());
        instance_dir_path = Some(inst_path);
    }

    // Step 5: resolve — use instance paths if available, else base image_dir
    let effective_image_dir = if let Some(ref rootfs) = instance_rootfs {
        // Point resolve at the instance dir parent; the rootfs file is named
        // rootfs.raw there, but resolve expects {image}.raw. We override
        // rootfs_path below instead.
        rootfs.parent().unwrap_or(image_dir)
    } else {
        image_dir
    };

    let mut resolved =
        resolve(&validated, effective_image_dir, default_kernel).map_err(|errors| {
            // Cleanup instance dir on resolve failure
            if let Some(ref p) = instance_dir_path {
                let _ = fs::remove_dir_all(p);
            }
            ComputeError::Config(
                errors
                    .into_iter()
                    .next()
                    .expect("at least one config error"),
            )
        })?;

    // Override rootfs path with instance rootfs if image management is active.
    if let Some(ref rootfs) = instance_rootfs {
        resolved.rootfs_path = rootfs.clone();
    }

    // If cloud-init was generated, add it to the resolved spec's volume list
    // so it appears as a second disk in the CH config.
    if let Some(ref ci_path) = instance_cloud_init {
        resolved.volume_paths.insert(
            0,
            crate::config::ResolvedVolume {
                path: ci_path.clone(),
                read_only: true,
            },
        );
    }

    // Compute socket path for preflight and map
    let runtime_dir_path_rt = base_dir.join(&vm_id_str);
    let socket_path = runtime_dir_path_rt.join("api.sock");

    // Step 6: map
    let vm_config = map(&resolved, &socket_path);

    // Step 7: preflight
    if let Err(e) = run_preflight(&resolved, ch_binary, &socket_path) {
        // Cleanup instance dir on preflight failure
        if let Some(ref p) = instance_dir_path {
            let _ = fs::remove_dir_all(p);
        }
        return Err(ComputeError::Preflight(
            e.into_iter().next().expect("at least one preflight error"),
        ));
    }

    // Step 8: Create RuntimeDir
    let runtime_dir = match RuntimeDir::create(base_dir, &vm_id_str) {
        Ok(d) => d,
        Err(e) => {
            if let Some(ref p) = instance_dir_path {
                let _ = fs::remove_dir_all(p);
            }
            return Err(e.into());
        }
    };
    // Internal event: SocketCreated (runtime dir with socket path is ready)
    debug!(vm_id = %vm_id_str, path = %runtime_dir.path().display(), "SocketCreated: created runtime dir");

    // From here on, any failure must clean up.
    let result = spawn_vm_inner(&vm_id_str, ch_binary, &runtime_dir, &vm_config, spec).await;

    match result {
        Ok(mut state) => {
            state.image_name = Some(spec.image.clone());
            state.instance_dir_path = instance_dir_path;
            Ok(state)
        }
        Err(e) => {
            // Capture the CH process log before cleanup for diagnostics.
            let log_contents = fs::read_to_string(runtime_dir.log_path()).unwrap_or_default();
            if !log_contents.is_empty() {
                error!(vm_id = %vm_id_str, log = %log_contents, "CH process log");
            }
            error!(vm_id = %vm_id_str, error = %e, "spawn failed, cleaning up");
            debug!(vm_id = %vm_id_str, path = %runtime_dir.path().display(), "SocketRemoved: cleaning up runtime dir");
            // Best-effort cleanup
            let _ = runtime_dir.cleanup();
            // Also cleanup instance dir
            if let Some(ref p) = instance_dir_path {
                let _ = fs::remove_dir_all(p);
            }
            Err(e)
        }
    }
}

/// Inner spawn logic. Separated so the caller can catch errors and clean up.
pub(crate) async fn spawn_vm_inner(
    vm_id_str: &str,
    ch_binary: &Path,
    runtime_dir: &RuntimeDir,
    vm_config: &serde_json::Value,
    spec: &VmSpec,
) -> Result<VmRuntimeState, ComputeError> {
    let socket_path = runtime_dir.socket_path();
    let log_path = runtime_dir.log_path();

    // Get CH version BEFORE spawning to avoid running two instances concurrently.
    // This is best-effort — failure is not fatal.
    let ch_version = get_ch_version(ch_binary).unwrap_or_else(|| "unknown".to_string());

    // Step 6: Spawn cloud-hypervisor process
    let log_file = fs::File::create(&log_path).map_err(|e| ProcessError::SpawnFailed {
        reason: format!("failed to create log file {}: {e}", log_path.display()),
    })?;
    let stderr_file = log_file
        .try_clone()
        .map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to clone log file handle: {e}"),
        })?;

    // SAFETY: pre_exec runs after fork but before exec in the child process.
    // We call setsid() to create a new session so the CH process is not
    // affected by signals sent to the daemon's process group.
    let mut cmd = std::process::Command::new(ch_binary);
    cmd.arg("--api-socket")
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file));
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| ProcessError::SpawnFailed {
        reason: format!("failed to exec {}: {e}", ch_binary.display()),
    })?;

    let pid = child.id();
    // Internal event: Spawned
    info!(vm_id = %vm_id_str, pid = pid, "Spawned: cloud-hypervisor process started");

    // Leak the Child handle to prevent its Drop from closing the pidfd,
    // which on some kernel/musl configurations may signal the child.
    // We manage the process lifecycle via PID from this point forward.
    std::mem::forget(child);

    // Step 7: Write pid, meta.json, ch-version
    runtime_dir.write_pid(pid)?;
    runtime_dir.write_ch_version(&ch_version)?;

    let meta = VmMeta {
        vm_id: vm_id_str.to_string(),
        created_at: now_iso8601(),
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid,
        ch_binary: ch_binary.to_string_lossy().into_owned(),
        ch_version: ch_version.clone(),
        spec_hash: compute_spec_hash(spec),
        vcpus: spec.vcpus,
        memory_mb: spec.memory_mb,
        image_name: Some(spec.image.clone()),
        disk_size_mb: spec.disk_size_mb,
    };
    runtime_dir.write_meta(&meta)?;

    // Step 8: Poll ping() until ready (100ms interval, 10s timeout)
    let client = ChClient::new(socket_path.clone());
    let ping_timeout = Duration::from_secs(10);
    let ping_interval = Duration::from_millis(100);
    let ping_start = tokio::time::Instant::now();

    loop {
        match client.ping().await {
            Ok(true) => {
                // Internal event: ApiReady
                debug!(vm_id = %vm_id_str, "ApiReady: CH REST API responding to ping");
                break;
            }
            Ok(false) | Err(_) => {
                if ping_start.elapsed() >= ping_timeout {
                    // Internal event: PingTimeout
                    debug!(vm_id = %vm_id_str, timeout_secs = ping_timeout.as_secs(), "PingTimeout: API did not become ready");
                    // Kill the process before returning error
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                    return Err(ProcessError::SpawnFailed {
                        reason: format!(
                            "API did not become ready within {}s",
                            ping_timeout.as_secs()
                        ),
                    }
                    .into());
                }
                // Check if process is still alive
                if !is_pid_alive(pid) {
                    // Internal event: ProcessExited
                    debug!(vm_id = %vm_id_str, pid = pid, "ProcessExited: CH process died before API ready");
                    return Err(ProcessError::SpawnFailed {
                        reason: "cloud-hypervisor process exited before API became ready"
                            .to_string(),
                    }
                    .into());
                }
                tokio::time::sleep(ping_interval).await;
            }
        }
    }

    // Step 9: client.create(vm_config)
    client
        .create(vm_config.clone())
        .await
        .map_err(|e| -> ComputeError {
            // Kill the process on create failure
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            e.into()
        })?;

    // Step 10: client.boot()
    client.boot().await.map_err(|e| -> ComputeError {
        // Kill the process on boot failure
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        e.into()
    })?;

    // Step 11: Return VmRuntimeState with phase Running
    info!(vm_id = %vm_id_str, pid = pid, "VM is running");
    Ok(VmRuntimeState {
        vm_id: spec.id.clone(),
        pid,
        socket_path,
        cgroup_path: None,
        ch_binary_path: ch_binary.to_path_buf(),
        ch_binary_version: ch_version,
        vcpus: spec.vcpus,
        memory_mb: spec.memory_mb,
        launched_at: now_unix(),
        last_ping_at: Some(now_unix()),
        last_error: None,
        current_phase: VmPhase::Running,
        reconnect_source: ReconnectSource::FreshSpawn,
        image_name: Some(spec.image.clone()),
        instance_dir_path: None, // Caller (spawn_vm) sets these
        runtime_handle: None,    // Caller sets from RuntimeHandle
    })
}

/// Get the Cloud Hypervisor version by running `ch_binary --version`.
pub(crate) fn get_ch_version(ch_binary: &Path) -> Option<String> {
    let output = std::process::Command::new(ch_binary)
        .arg("--version")
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output is typically "cloud-hypervisor v43.0" — extract the version part
    stdout.split_whitespace().last().map(|s| s.to_string())
}

/// Compute a SHA256 hash of the VmSpec JSON for change detection.
pub(crate) fn compute_spec_hash(spec: &VmSpec) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Use a deterministic hash of the JSON representation.
    // A proper SHA256 would require an extra dependency; this is sufficient
    // for change detection within the same binary version.
    let json = serde_json::to_string(spec).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    format!("hash:{:016x}", hasher.finish())
}

// ---------------------------------------------------------------------------
// Kill chain (#479)
// ---------------------------------------------------------------------------

/// Execute the 4-level kill chain to stop a VM.
///
/// Levels:
/// 1. `shutdown_graceful` via CH API (30s timeout, poll PID)
/// 2. `shutdown_force` via CH API (10s timeout)
/// 3. SIGTERM on PID (5s timeout)
/// 4. SIGKILL on PID
///
/// The chain is interruptible: if the process dies at any level, remaining
/// levels are skipped.
pub async fn kill_vm(
    state: &mut VmRuntimeState,
    client: &ChClient,
    runtime_dir: &RuntimeDir,
) -> Result<(), ComputeError> {
    kill_vm_inner(state, client, runtime_dir, false).await
}

/// Like [`kill_vm`] but skips level 1 (graceful shutdown), starting directly
/// from `shutdown_force`.
pub async fn kill_vm_force(
    state: &mut VmRuntimeState,
    client: &ChClient,
    runtime_dir: &RuntimeDir,
) -> Result<(), ComputeError> {
    kill_vm_inner(state, client, runtime_dir, true).await
}

async fn kill_vm_inner(
    state: &mut VmRuntimeState,
    client: &ChClient,
    runtime_dir: &RuntimeDir,
    force: bool,
) -> Result<(), ComputeError> {
    let pid = state.pid;
    let vm_id_str = state.vm_id.0.clone();

    info!(vm_id = %vm_id_str, pid = pid, force = force, "starting kill chain");

    // Transition to Stopping (allow from Running, Starting, or if already Stopping)
    if state.current_phase != VmPhase::Stopping {
        state.current_phase = state.current_phase.transition(VmPhase::Stopping)?;
    }

    // Check if already dead before starting the chain
    if !is_pid_alive(pid) {
        debug!(vm_id = %vm_id_str, "process already dead");
        state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
        runtime_dir.cleanup()?;
        return Ok(());
    }

    // Level 1: shutdown_graceful (30s timeout) — skipped when force=true
    if !force {
        info!(vm_id = %vm_id_str, "kill chain level 1: shutdown_graceful");
        if let Err(e) = client.shutdown_graceful().await {
            warn!(vm_id = %vm_id_str, error = %e, "shutdown_graceful failed, continuing kill chain");
        } else if wait_for_pid_exit(pid, Duration::from_secs(30)).await {
            info!(vm_id = %vm_id_str, "process exited after graceful shutdown");
            state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
            runtime_dir.cleanup()?;
            return Ok(());
        }
    }

    // Level 2: shutdown_force (10s timeout)
    if !is_pid_alive(pid) {
        state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
        runtime_dir.cleanup()?;
        return Ok(());
    }
    info!(vm_id = %vm_id_str, "kill chain level 2: shutdown_force");
    if let Err(e) = client.shutdown_force().await {
        warn!(vm_id = %vm_id_str, error = %e, "shutdown_force failed, continuing kill chain");
    } else if wait_for_pid_exit(pid, Duration::from_secs(10)).await {
        info!(vm_id = %vm_id_str, "process exited after forced shutdown");
        state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
        runtime_dir.cleanup()?;
        return Ok(());
    }

    // Level 3: SIGTERM (5s timeout)
    if !is_pid_alive(pid) {
        state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
        runtime_dir.cleanup()?;
        return Ok(());
    }
    info!(vm_id = %vm_id_str, "kill chain level 3: SIGTERM");
    // SAFETY: Sending SIGTERM to the cloud-hypervisor process we spawned.
    let term_result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if term_result != 0 {
        warn!(vm_id = %vm_id_str, "SIGTERM failed (errno), process may already be dead");
    }
    if wait_for_pid_exit(pid, Duration::from_secs(5)).await {
        info!(vm_id = %vm_id_str, "process exited after SIGTERM");
        state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
        runtime_dir.cleanup()?;
        return Ok(());
    }

    // Level 4: SIGKILL (unconditional)
    if !is_pid_alive(pid) {
        state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
        runtime_dir.cleanup()?;
        return Ok(());
    }
    info!(vm_id = %vm_id_str, "kill chain level 4: SIGKILL");
    // SAFETY: Sending SIGKILL to the cloud-hypervisor process we spawned.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }

    // Give a brief moment for the kernel to reap the process
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify process is actually dead
    if is_pid_alive(pid) {
        error!(vm_id = %vm_id_str, pid = pid, "process still alive after SIGKILL");
        state.current_phase = state.current_phase.transition(VmPhase::Failed)?;
        return Err(ProcessError::SignalFailed {
            signal: "SIGKILL".to_string(),
            pid,
        }
        .into());
    }

    info!(vm_id = %vm_id_str, "process confirmed dead");
    state.current_phase = state.current_phase.transition(VmPhase::Stopped)?;
    runtime_dir.cleanup()?;
    Ok(())
}

/// Delete a VM: kill if running, then clean up all artifacts.
///
/// - If Running/Starting: run kill chain first
/// - If Stopped/Failed: skip kill
/// - Call client.delete() to clean CH internal state
/// - Cleanup runtime dir
/// - Transition: -> Deleting -> Deleted
pub async fn delete_vm(
    state: &mut VmRuntimeState,
    client: &ChClient,
    runtime_dir: &RuntimeDir,
) -> Result<(), ComputeError> {
    let vm_id_str = state.vm_id.0.clone();
    info!(vm_id = %vm_id_str, phase = ?state.current_phase, "deleting VM");

    // If the VM is active, kill it first
    match state.current_phase {
        VmPhase::Running | VmPhase::Starting | VmPhase::Provisioning => {
            // Need to transition through Stopping first
            if state.current_phase == VmPhase::Running || state.current_phase == VmPhase::Starting {
                // For Starting, we need to handle the transition: Starting can go to Failed
                if state.current_phase == VmPhase::Starting {
                    state.current_phase = state.current_phase.transition(VmPhase::Failed)?;
                    state.current_phase = state.current_phase.transition(VmPhase::Deleting)?;
                } else {
                    // Running -> Stopping -> Stopped -> Deleting
                    kill_vm(state, client, runtime_dir).await?;
                    // After kill_vm, phase should be Stopped
                    state.current_phase = state.current_phase.transition(VmPhase::Deleting)?;
                }
            } else {
                // Provisioning -> Failed -> Deleting
                state.current_phase = state.current_phase.transition(VmPhase::Failed)?;
                state.current_phase = state.current_phase.transition(VmPhase::Deleting)?;
            }
        }
        VmPhase::Stopping => {
            // Already stopping — wait for it, but we can transition Stopped -> Deleting
            // For simplicity, force kill
            kill_vm(state, client, runtime_dir).await?;
            state.current_phase = state.current_phase.transition(VmPhase::Deleting)?;
        }
        VmPhase::Stopped | VmPhase::Failed => {
            state.current_phase = state.current_phase.transition(VmPhase::Deleting)?;
        }
        VmPhase::Deleting => {
            // Already deleting, continue
        }
        VmPhase::Deleted => {
            // Already deleted, no-op
            return Ok(());
        }
        VmPhase::Pending => {
            // Pending -> can't go to Deleting directly. Go through Failed.
            // The state machine doesn't allow Pending -> Deleting, so:
            // Pending -> Provisioning -> Failed -> Deleting
            // But that doesn't make sense either. For a pending VM that was
            // never spawned, just clean up.
            // Force the phase for cleanup purposes.
            state.current_phase = VmPhase::Deleting;
        }
    }

    // Best-effort: tell CH to delete (may fail if process is already gone)
    if let Err(e) = client.delete().await {
        debug!(vm_id = %vm_id_str, error = %e, "client.delete() failed (process may be gone)");
    }

    // Cleanup runtime dir
    if let Err(e) = runtime_dir.cleanup() {
        warn!(vm_id = %vm_id_str, error = %e, "runtime dir cleanup failed");
    }

    // Transition to Deleted
    if state.current_phase == VmPhase::Deleting {
        state.current_phase = state.current_phase.transition(VmPhase::Deleted)?;
    }

    info!(vm_id = %vm_id_str, "VM deleted");
    Ok(())
}

// ---------------------------------------------------------------------------
// Scan all runtime dirs (including those without meta.json, for orphan detection)
// ---------------------------------------------------------------------------

/// Scan a base directory for all subdirectories, including those without meta.json.
///
/// Returns `(with_meta, without_meta)` — dirs with meta.json and orphan dirs without.
fn scan_all_runtime_dirs(base: &Path) -> (Vec<RuntimeDir>, Vec<RuntimeDir>) {
    let entries = match fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let mut with_meta = Vec::new();
    let mut without_meta = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.join("meta.json").exists() {
                with_meta.push(RuntimeDir::from_existing(path));
            } else {
                without_meta.push(RuntimeDir::from_existing(path));
            }
        }
    }
    (with_meta, without_meta)
}

// ---------------------------------------------------------------------------
// Monitor (#477) — periodic health check and crash detection
// ---------------------------------------------------------------------------

/// Periodic health-check loop for all tracked VMs.
///
/// Runs every `interval` (default 5s). For each VM in Running or Starting phase:
/// 1. Check PID alive via `kill(pid, 0)`
/// 2. Try `ping()` on the CH socket
/// 3. If dead: transition to Failed, emit `VmEvent::Crashed`
/// 4. If alive: update `last_ping_at`
///
/// Uses `try_lock()` on individual VM mutexes to avoid blocking on VMs that are
/// mid-operation (e.g., a long shutdown). Skipped VMs are checked next iteration.
pub async fn monitor_loop(
    vms: Arc<RwLock<HashMap<String, Arc<Mutex<VmRuntimeState>>>>>,
    event_tx: broadcast::Sender<VmEvent>,
    interval: Duration,
) {
    loop {
        tokio::time::sleep(interval).await;

        // Take a snapshot of current VM keys under a brief read lock.
        let snapshot: Vec<(String, Arc<Mutex<VmRuntimeState>>)> = {
            let map = vms.read().await;
            map.iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };

        for (vm_id_str, vm_arc) in snapshot {
            // Use try_lock to avoid blocking on VMs mid-operation.
            let mut guard = match vm_arc.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    debug!(vm_id = %vm_id_str, "monitor: skipping busy VM");
                    continue;
                }
            };

            // Only check VMs in Running or Starting phase.
            if guard.current_phase != VmPhase::Running && guard.current_phase != VmPhase::Starting {
                continue;
            }

            let pid = guard.pid;
            let pid_alive = is_pid_alive(pid);

            if !pid_alive {
                warn!(vm_id = %vm_id_str, pid = pid, "monitor: PID dead, transitioning to Failed");
                guard.current_phase = VmPhase::Failed;
                guard.last_error = Some(format!("process {pid} no longer alive"));
                events::emit(
                    &event_tx,
                    VmEvent::Crashed {
                        vm_id: guard.vm_id.clone(),
                        error: format!("process {pid} exited unexpectedly"),
                    },
                );
                continue;
            }

            // Containers have no API socket — PID check is sufficient.
            let is_container = guard
                .runtime_handle
                .as_ref()
                .map(|h| h.runtime_type == RuntimeType::Container)
                .unwrap_or(false);

            if is_container {
                // PID is alive and that is all we can check for containers.
                guard.last_ping_at = Some(now_unix());
                continue;
            }

            // PID is alive — try ping on the CH API socket.
            let client = ChClient::with_timeout(guard.socket_path.clone(), Duration::from_secs(3));
            // Drop the guard before the async ping to avoid holding the lock across await.
            // We re-acquire after the ping completes.
            let vm_id_clone = guard.vm_id.clone();
            let socket_path = guard.socket_path.clone();
            drop(guard);

            let ping_ok = matches!(client.ping().await, Ok(true));

            // Re-acquire the lock to update state.
            let mut guard = match vm_arc.try_lock() {
                Ok(g) => g,
                Err(_) => continue,
            };

            // Re-check phase — it may have changed while we were pinging.
            if guard.current_phase != VmPhase::Running && guard.current_phase != VmPhase::Starting {
                continue;
            }

            if ping_ok {
                guard.last_ping_at = Some(now_unix());
            } else {
                warn!(
                    vm_id = %vm_id_str,
                    socket = %socket_path.display(),
                    "monitor: ping failed, transitioning to Failed"
                );
                guard.current_phase = VmPhase::Failed;
                guard.last_error = Some("API socket unresponsive".to_string());
                events::emit(
                    &event_tx,
                    VmEvent::Crashed {
                        vm_id: vm_id_clone,
                        error: "API socket unresponsive while PID alive".to_string(),
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reconnect (#482) — recover VMs after daemon restart
// ---------------------------------------------------------------------------

/// Report returned by the reconnect scan.
#[derive(Debug)]
pub struct ReconnectReport {
    /// VMs successfully recovered.
    pub recovered: Vec<VmRuntimeState>,
    /// VMs that failed to reconnect: (vm_id, error description).
    pub failed: Vec<(String, String)>,
    /// VM IDs of orphaned runtime dirs that were cleaned up.
    pub orphans_cleaned: Vec<String>,
}

/// Scan runtime dirs, recover live VMs, report failures, clean orphans.
///
/// Truth model: `meta.json` = intention. PID alive + socket responding = reality.
/// All three must agree for a successful reconnect.
///
/// Orphans (runtime dir with no meta.json, or with dead PID + dead socket)
/// where meta.json is corrupt are cleaned immediately.
pub async fn reconnect(base_dir: &Path, event_tx: broadcast::Sender<VmEvent>) -> ReconnectReport {
    let mut report = ReconnectReport {
        recovered: Vec::new(),
        failed: Vec::new(),
        orphans_cleaned: Vec::new(),
    };

    let (with_meta, without_meta) = scan_all_runtime_dirs(base_dir);

    // Handle dirs without meta.json — completely corrupt orphans.
    for dir in without_meta {
        let dir_name = dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        warn!(
            dir = %dir.path().display(),
            "reconnect: orphan dir without meta.json, cleaning"
        );

        match cleanup_orphan(&dir, "no meta.json found") {
            Ok(vm_id) => {
                events::emit(
                    &event_tx,
                    VmEvent::VmOrphanCleaned {
                        vm_id: VmId(vm_id.clone()),
                        reason: "no meta.json found".to_string(),
                    },
                );
                report.orphans_cleaned.push(vm_id);
            }
            Err(e) => {
                warn!(dir = %dir_name, error = %e, "failed to clean orphan dir");
                report.orphans_cleaned.push(dir_name);
            }
        }
    }

    // Handle dirs with meta.json — attempt reconnect.
    for dir in with_meta {
        let meta = match dir.read_meta() {
            Ok(m) => m,
            Err(e) => {
                // Before treating as orphan, check if this is a container
                // runtime dir. Container VMs write ContainerMeta (different
                // schema) which fails VmMeta deserialization. Skip these —
                // VmManager::reconnect delegates them to the runtime backend.
                if is_container_meta(dir.meta_path()) {
                    debug!(
                        dir = %dir.path().display(),
                        "reconnect: skipping container runtime dir"
                    );
                    continue;
                }

                // meta.json exists but is corrupt — treat as orphan.
                let dir_name = dir
                    .path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                warn!(
                    dir = %dir.path().display(),
                    error = %e,
                    "reconnect: corrupt meta.json, treating as orphan"
                );

                match cleanup_orphan(&dir, "corrupt meta.json") {
                    Ok(vm_id) => {
                        events::emit(
                            &event_tx,
                            VmEvent::VmOrphanCleaned {
                                vm_id: VmId(vm_id.clone()),
                                reason: "corrupt meta.json".to_string(),
                            },
                        );
                        report.orphans_cleaned.push(vm_id);
                    }
                    Err(_) => {
                        report.orphans_cleaned.push(dir_name);
                    }
                }
                continue;
            }
        };

        let vm_id_str = meta.vm_id.clone();

        // Check PID alive.
        if !is_pid_alive(meta.pid) {
            info!(
                vm_id = %vm_id_str,
                pid = meta.pid,
                "reconnect: PID dead"
            );
            events::emit(
                &event_tx,
                VmEvent::ReconnectFailed {
                    vm_id: VmId(vm_id_str.clone()),
                    error: format!("process {} no longer alive", meta.pid),
                },
            );
            report
                .failed
                .push((vm_id_str, format!("PID {} dead", meta.pid)));
            // Don't cleanup — forge decides.
            continue;
        }

        // PID alive — check socket.
        let socket_path = PathBuf::from(&meta.socket_path);
        let client = ChClient::with_timeout(socket_path.clone(), Duration::from_secs(3));

        let ping_ok = matches!(client.ping().await, Ok(true));

        if !ping_ok {
            info!(
                vm_id = %vm_id_str,
                "reconnect: PID alive but socket unresponsive"
            );
            events::emit(
                &event_tx,
                VmEvent::ReconnectFailed {
                    vm_id: VmId(vm_id_str.clone()),
                    error: "PID alive but API socket unresponsive".to_string(),
                },
            );
            report.failed.push((
                vm_id_str,
                "PID alive but API socket unresponsive".to_string(),
            ));
            // Don't cleanup — forge decides.
            continue;
        }

        // All three agree: meta.json + PID alive + socket responds.
        info!(
            vm_id = %vm_id_str,
            pid = meta.pid,
            "reconnect: successfully recovered VM"
        );

        let state = VmRuntimeState {
            vm_id: VmId(vm_id_str.clone()),
            pid: meta.pid,
            socket_path,
            cgroup_path: None,
            ch_binary_path: PathBuf::from(&meta.ch_binary),
            ch_binary_version: meta.ch_version,
            vcpus: meta.vcpus,
            memory_mb: meta.memory_mb,
            launched_at: now_unix(),
            last_ping_at: Some(now_unix()),
            last_error: None,
            current_phase: VmPhase::Running,
            reconnect_source: ReconnectSource::Recovered,
            image_name: meta.image_name.clone(),
            instance_dir_path: None,
            runtime_handle: None,
        };

        events::emit(
            &event_tx,
            VmEvent::ReconnectSucceeded {
                vm_id: VmId(vm_id_str),
            },
        );

        report.recovered.push(state);
    }

    info!(
        recovered = report.recovered.len(),
        failed = report.failed.len(),
        orphans = report.orphans_cleaned.len(),
        "reconnect complete: recovered {} VMs, {} failed, {} orphaned and cleaned",
        report.recovered.len(),
        report.failed.len(),
        report.orphans_cleaned.len(),
    );

    report
}

// ---------------------------------------------------------------------------
// Container meta detection
// ---------------------------------------------------------------------------

/// Check if a `meta.json` file belongs to a container runtime.
///
/// Reads the raw JSON and looks for `"runtime_type": "container"`. This avoids
/// deserializing as `VmMeta` (which would fail for container dirs) and prevents
/// the reconnect loop from treating container dirs as corrupt orphans.
fn is_container_meta(path: PathBuf) -> bool {
    let data = match fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let value: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return false,
    };
    value.get("runtime_type").and_then(|v| v.as_str()) == Some("container")
}

// ---------------------------------------------------------------------------
// Orphan cleanup (#484)
// ---------------------------------------------------------------------------

/// Clean up an orphaned runtime directory.
///
/// An orphan is a runtime dir that exists with no corresponding live process
/// (PID dead, socket dead, no recoverable state).
///
/// Steps:
/// 1. Read vm_id from meta.json (if readable), otherwise use dir name
/// 2. Remove all files in the runtime directory
/// 3. Remove the runtime directory itself
/// 4. Try to remove the cgroup `/sys/fs/cgroup/syfrah/{vm_id}/` (best effort)
/// 5. Return the vm_id for event emission
pub fn cleanup_orphan(dir: &RuntimeDir, reason: &str) -> Result<String, ProcessError> {
    // Try to read vm_id from meta.json, fall back to dir name.
    let vm_id = match dir.read_meta() {
        Ok(meta) => meta.vm_id,
        Err(_) => dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
    };

    info!(
        vm_id = %vm_id,
        reason = %reason,
        dir = %dir.path().display(),
        "cleaning up orphaned runtime dir"
    );

    // Remove the entire runtime directory.
    dir.cleanup()
        .map_err(|e| ProcessError::OrphanCleanupFailed {
            vm_id: vm_id.clone(),
            reason: format!("failed to remove runtime dir: {e}"),
        })?;

    // Best-effort cgroup removal.
    let cgroup_path = PathBuf::from(format!("/sys/fs/cgroup/syfrah/{vm_id}"));
    if cgroup_path.exists() {
        if let Err(e) = fs::remove_dir_all(&cgroup_path) {
            warn!(
                vm_id = %vm_id,
                cgroup = %cgroup_path.display(),
                error = %e,
                "best-effort cgroup removal failed"
            );
        } else {
            debug!(vm_id = %vm_id, "removed cgroup dir");
        }
    }

    Ok(vm_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- RuntimeDir tests -----------------------------------------------------

    #[test]
    fn create_runtime_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-test-1").unwrap();
        assert!(dir.exists());
        assert!(dir.path().ends_with("vm-test-1"));
    }

    #[test]
    fn runtime_dir_paths() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-paths").unwrap();

        assert!(dir.socket_path().ends_with("api.sock"));
        assert!(dir.pid_path().ends_with("pid"));
        assert!(dir.meta_path().ends_with("meta.json"));
        assert!(dir.ch_version_path().ends_with("ch-version"));
        assert!(dir.log_path().ends_with("stdout.log"));
    }

    #[test]
    fn write_and_read_meta() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-meta").unwrap();

        let meta = VmMeta {
            vm_id: "vm-meta".to_string(),
            created_at: "2026-03-27T14:00:00Z".to_string(),
            socket_path: "/run/syfrah/vms/vm-meta/api.sock".to_string(),
            pid: 12345,
            ch_binary: "/usr/local/lib/syfrah/cloud-hypervisor".to_string(),
            ch_version: "v43.0".to_string(),
            spec_hash: "hash:abc123".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };

        dir.write_meta(&meta).unwrap();
        let read_back = dir.read_meta().unwrap();
        assert_eq!(meta, read_back);
    }

    #[test]
    fn atomic_meta_write_no_tmp_leftover() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-atomic").unwrap();

        let meta = VmMeta {
            vm_id: "vm-atomic".to_string(),
            created_at: "2026-03-27T14:00:00Z".to_string(),
            socket_path: "/tmp/api.sock".to_string(),
            pid: 1,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1.0".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };

        dir.write_meta(&meta).unwrap();

        // .tmp file should not exist after successful write
        let tmp_path = dir.path().join(".meta.json.tmp");
        assert!(!tmp_path.exists());
        // meta.json should exist
        assert!(dir.meta_path().exists());
    }

    #[test]
    fn write_and_read_pid() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-pid").unwrap();

        dir.write_pid(42).unwrap();
        assert_eq!(dir.read_pid().unwrap(), 42);
    }

    #[test]
    fn write_ch_version() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-ver").unwrap();

        dir.write_ch_version("v43.0").unwrap();
        let content = fs::read_to_string(dir.ch_version_path()).unwrap();
        assert_eq!(content, "v43.0");
    }

    #[test]
    fn cleanup_removes_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-cleanup").unwrap();
        dir.write_pid(1).unwrap();
        assert!(dir.exists());

        dir.cleanup().unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn cleanup_on_nonexistent_is_ok() {
        let dir = RuntimeDir::from_existing(PathBuf::from("/nonexistent/vm-ghost"));
        // Should not error
        dir.cleanup().unwrap();
    }

    #[test]
    fn from_existing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("vm-existing");
        fs::create_dir_all(&path).unwrap();
        let dir = RuntimeDir::from_existing(path.clone());
        assert!(dir.exists());
        assert_eq!(dir.path(), path);
    }

    // -- VmMeta serde ---------------------------------------------------------

    #[test]
    fn vm_meta_serde_roundtrip() {
        let meta = VmMeta {
            vm_id: "vm-serde".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: "/tmp/api.sock".to_string(),
            pid: 9999,
            ch_binary: "/usr/bin/ch".to_string(),
            ch_version: "v42.0".to_string(),
            spec_hash: "hash:deadbeef".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: VmMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    // -- scan_runtime_dirs ----------------------------------------------------

    #[test]
    fn scan_runtime_dirs_finds_dirs_with_meta() {
        let tmp = TempDir::new().unwrap();

        // Create two valid runtime dirs
        let dir1 = RuntimeDir::create(tmp.path(), "vm-1").unwrap();
        let meta = VmMeta {
            vm_id: "vm-1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: "/tmp/vm-1/api.sock".to_string(),
            pid: 1,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir1.write_meta(&meta).unwrap();

        let dir2 = RuntimeDir::create(tmp.path(), "vm-2").unwrap();
        let meta2 = VmMeta {
            vm_id: "vm-2".to_string(),
            ..meta.clone()
        };
        dir2.write_meta(&meta2).unwrap();

        // Create a dir without meta.json (should be excluded)
        fs::create_dir_all(tmp.path().join("vm-orphan")).unwrap();

        let found = scan_runtime_dirs(tmp.path());
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn scan_runtime_dirs_empty_base() {
        let tmp = TempDir::new().unwrap();
        let found = scan_runtime_dirs(tmp.path());
        assert!(found.is_empty());
    }

    #[test]
    fn scan_runtime_dirs_nonexistent_base() {
        let found = scan_runtime_dirs(Path::new("/nonexistent/base"));
        assert!(found.is_empty());
    }

    // -- is_pid_alive ---------------------------------------------------------

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
    }

    #[test]
    fn nonexistent_pid_is_not_alive() {
        // PID 4_000_000 is extremely unlikely to exist
        assert!(!is_pid_alive(4_000_000));
    }

    // -- days_to_ymd ----------------------------------------------------------

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2026-03-27 is day 20539 since epoch
        // Let's verify a known date: 2000-01-01 = day 10957
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
    }

    // -- compute_spec_hash ----------------------------------------------------

    #[test]
    fn spec_hash_deterministic() {
        use crate::types::{GpuMode, VmId, VmSpec};
        let spec = VmSpec {
            id: VmId("vm-hash".to_string()),
            vcpus: 2,
            memory_mb: 512,
            image: "test".to_string(),
            kernel: None,
            network: None,
            volumes: vec![],
            gpu: GpuMode::None,
            ssh_key: None,
            disk_size_mb: None,
        };
        let h1 = compute_spec_hash(&spec);
        let h2 = compute_spec_hash(&spec);
        assert_eq!(h1, h2);
        assert!(h1.starts_with("hash:"));
    }

    // -- delete_vm state transitions ------------------------------------------

    #[tokio::test]
    async fn delete_already_deleted_is_noop() {
        use crate::types::VmId;

        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-del").unwrap();
        let client = ChClient::new(dir.socket_path());

        let mut state = VmRuntimeState {
            vm_id: VmId("vm-del".to_string()),
            pid: 4_000_000, // nonexistent
            socket_path: dir.socket_path(),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Deleted,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let result = delete_vm(&mut state, &client, &dir).await;
        assert!(result.is_ok());
        assert_eq!(state.current_phase, VmPhase::Deleted);
    }

    #[tokio::test]
    async fn delete_stopped_vm_transitions_to_deleted() {
        use crate::types::VmId;

        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-stop-del").unwrap();
        let client = ChClient::new(dir.socket_path());

        let mut state = VmRuntimeState {
            vm_id: VmId("vm-stop-del".to_string()),
            pid: 4_000_000,
            socket_path: dir.socket_path(),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Stopped,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let result = delete_vm(&mut state, &client, &dir).await;
        assert!(result.is_ok());
        assert_eq!(state.current_phase, VmPhase::Deleted);
    }

    #[tokio::test]
    async fn delete_failed_vm_transitions_to_deleted() {
        use crate::types::VmId;

        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-fail-del").unwrap();
        let client = ChClient::new(dir.socket_path());

        let mut state = VmRuntimeState {
            vm_id: VmId("vm-fail-del".to_string()),
            pid: 4_000_000,
            socket_path: dir.socket_path(),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Failed,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let result = delete_vm(&mut state, &client, &dir).await;
        assert!(result.is_ok());
        assert_eq!(state.current_phase, VmPhase::Deleted);
    }

    // -- kill_vm on already-dead process --------------------------------------

    #[tokio::test]
    async fn kill_vm_already_dead_process() {
        use crate::types::VmId;

        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-kill-dead").unwrap();
        let client = ChClient::new(dir.socket_path());

        let mut state = VmRuntimeState {
            vm_id: VmId("vm-kill-dead".to_string()),
            pid: 4_000_000, // nonexistent PID
            socket_path: dir.socket_path(),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Running,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let result = kill_vm(&mut state, &client, &dir).await;
        assert!(result.is_ok());
        assert_eq!(state.current_phase, VmPhase::Stopped);
        // Runtime dir should be cleaned up
        assert!(!dir.exists());
    }

    // -- scan_all_runtime_dirs ------------------------------------------------

    #[test]
    fn scan_all_runtime_dirs_separates_meta_and_orphan() {
        let tmp = TempDir::new().unwrap();

        // Dir with meta.json
        let dir1 = RuntimeDir::create(tmp.path(), "vm-with-meta").unwrap();
        let meta = VmMeta {
            vm_id: "vm-with-meta".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: "/tmp/api.sock".to_string(),
            pid: 1,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir1.write_meta(&meta).unwrap();

        // Dir without meta.json (orphan)
        fs::create_dir_all(tmp.path().join("vm-orphan")).unwrap();

        let (with, without) = scan_all_runtime_dirs(tmp.path());
        assert_eq!(with.len(), 1);
        assert_eq!(without.len(), 1);
    }

    #[test]
    fn scan_all_runtime_dirs_empty() {
        let tmp = TempDir::new().unwrap();
        let (with, without) = scan_all_runtime_dirs(tmp.path());
        assert!(with.is_empty());
        assert!(without.is_empty());
    }

    // -- cleanup_orphan -------------------------------------------------------

    #[test]
    fn cleanup_orphan_with_meta() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-orphan-meta").unwrap();
        let meta = VmMeta {
            vm_id: "vm-orphan-meta".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: "/tmp/api.sock".to_string(),
            pid: 1,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir.write_meta(&meta).unwrap();

        let result = cleanup_orphan(&dir, "test cleanup");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "vm-orphan-meta");
        assert!(!dir.exists());
    }

    #[test]
    fn cleanup_orphan_without_meta_uses_dir_name() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("vm-no-meta");
        fs::create_dir_all(&path).unwrap();
        let dir = RuntimeDir::from_existing(path);

        let result = cleanup_orphan(&dir, "no meta");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "vm-no-meta");
        assert!(!dir.exists());
    }

    // -- reconnect ------------------------------------------------------------

    #[tokio::test]
    async fn reconnect_with_dead_pid() {
        let tmp = TempDir::new().unwrap();

        // Create a runtime dir with meta pointing to a dead PID.
        let dir = RuntimeDir::create(tmp.path(), "vm-dead").unwrap();
        let meta = VmMeta {
            vm_id: "vm-dead".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: tmp
                .path()
                .join("vm-dead/api.sock")
                .to_string_lossy()
                .into_owned(),
            pid: 4_000_000, // nonexistent
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir.write_meta(&meta).unwrap();

        let (tx, mut rx) = broadcast::channel(16);
        let report = reconnect(tmp.path(), tx).await;

        assert_eq!(report.recovered.len(), 0);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "vm-dead");
        assert_eq!(report.orphans_cleaned.len(), 0);

        // Should have emitted ReconnectFailed event.
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, VmEvent::ReconnectFailed { .. }));
    }

    #[tokio::test]
    async fn reconnect_cleans_orphan_without_meta() {
        let tmp = TempDir::new().unwrap();

        // Create an orphan dir without meta.json.
        fs::create_dir_all(tmp.path().join("vm-orphan")).unwrap();

        let (tx, mut rx) = broadcast::channel(16);
        let report = reconnect(tmp.path(), tx).await;

        assert_eq!(report.recovered.len(), 0);
        assert_eq!(report.failed.len(), 0);
        assert_eq!(report.orphans_cleaned.len(), 1);
        assert_eq!(report.orphans_cleaned[0], "vm-orphan");

        // Orphan dir should be removed.
        assert!(!tmp.path().join("vm-orphan").exists());

        // Should have emitted VmOrphanCleaned event.
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, VmEvent::VmOrphanCleaned { .. }));
    }

    #[tokio::test]
    async fn reconnect_cleans_corrupt_meta() {
        let tmp = TempDir::new().unwrap();

        // Create a dir with corrupt meta.json.
        let dir_path = tmp.path().join("vm-corrupt");
        fs::create_dir_all(&dir_path).unwrap();
        fs::write(dir_path.join("meta.json"), "not valid json!!!").unwrap();

        let (tx, mut rx) = broadcast::channel(16);
        let report = reconnect(tmp.path(), tx).await;

        assert_eq!(report.recovered.len(), 0);
        assert_eq!(report.failed.len(), 0);
        assert_eq!(report.orphans_cleaned.len(), 1);

        // Corrupt dir should be removed.
        assert!(!dir_path.exists());

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, VmEvent::VmOrphanCleaned { .. }));
    }

    #[tokio::test]
    async fn reconnect_empty_dir() {
        let tmp = TempDir::new().unwrap();

        let (tx, _rx) = broadcast::channel(16);
        let report = reconnect(tmp.path(), tx).await;

        assert_eq!(report.recovered.len(), 0);
        assert_eq!(report.failed.len(), 0);
        assert_eq!(report.orphans_cleaned.len(), 0);
    }

    // -- monitor_loop (basic test with dead PID) ------------------------------

    #[tokio::test]
    async fn monitor_detects_dead_pid() {
        let vms: Arc<RwLock<HashMap<String, Arc<Mutex<VmRuntimeState>>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let state = VmRuntimeState {
            vm_id: VmId("vm-monitor-dead".to_string()),
            pid: 4_000_000, // nonexistent
            socket_path: PathBuf::from("/tmp/nonexistent.sock"),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Running,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let vm_arc = Arc::new(Mutex::new(state));
        {
            let mut map = vms.write().await;
            map.insert("vm-monitor-dead".to_string(), Arc::clone(&vm_arc));
        }

        let (tx, mut rx) = broadcast::channel(16);

        // Run monitor for one iteration (very short interval).
        let vms_clone = Arc::clone(&vms);
        let handle = tokio::spawn(async move {
            monitor_loop(vms_clone, tx, Duration::from_millis(10)).await;
        });

        // Wait for the monitor to detect the dead VM.
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("channel error");

        assert!(matches!(event, VmEvent::Crashed { .. }));

        // Verify state transitioned to Failed.
        let guard = vm_arc.lock().await;
        assert_eq!(guard.current_phase, VmPhase::Failed);
        assert!(guard.last_error.is_some());

        handle.abort();
    }

    // -- RuntimeDir error paths -----------------------------------------------

    #[test]
    fn runtime_dir_read_meta_missing_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-no-meta").unwrap();
        // No meta.json written
        let result = dir.read_meta();
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("failed to read"));
    }

    #[test]
    fn runtime_dir_read_pid_missing_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-no-pid").unwrap();
        // No pid file written
        let result = dir.read_pid();
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("failed to read"));
    }

    #[test]
    fn runtime_dir_read_pid_invalid_content_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-bad-pid").unwrap();
        fs::write(dir.pid_path(), "not_a_number").unwrap();
        let result = dir.read_pid();
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("invalid pid"));
    }

    #[test]
    fn runtime_dir_write_and_read_large_pid() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-large-pid").unwrap();
        // Max PID on Linux is typically 4194304 (2^22)
        let large_pid: u32 = 4_194_304;
        dir.write_pid(large_pid).unwrap();
        assert_eq!(dir.read_pid().unwrap(), large_pid);
    }

    #[test]
    fn runtime_dir_overwrite_meta() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-overwrite").unwrap();

        let meta1 = VmMeta {
            vm_id: "vm-overwrite".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: "/tmp/api.sock".to_string(),
            pid: 100,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:aaa".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir.write_meta(&meta1).unwrap();

        let meta2 = VmMeta {
            pid: 200,
            ch_version: "v2".to_string(),
            spec_hash: "hash:bbb".to_string(),
            ..meta1.clone()
        };
        dir.write_meta(&meta2).unwrap();

        let read_back = dir.read_meta().unwrap();
        assert_eq!(read_back.pid, 200);
        assert_eq!(read_back.ch_version, "v2");
        assert_eq!(read_back.spec_hash, "hash:bbb");
    }

    // -- delete_vm additional state transitions -------------------------------

    #[tokio::test]
    async fn delete_pending_vm_transitions_to_deleted() {
        use crate::types::VmId;

        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-pending-del").unwrap();
        let client = ChClient::new(dir.socket_path());

        let mut state = VmRuntimeState {
            vm_id: VmId("vm-pending-del".to_string()),
            pid: 4_000_000,
            socket_path: dir.socket_path(),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Pending,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let result = delete_vm(&mut state, &client, &dir).await;
        assert!(result.is_ok());
        assert_eq!(state.current_phase, VmPhase::Deleted);
    }

    // -- kill_vm cleanup verification -----------------------------------------

    #[tokio::test]
    async fn kill_vm_dead_pid_cleans_runtime_dir_files() {
        use crate::types::VmId;

        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-kill-clean").unwrap();
        // Write some files that should be cleaned
        dir.write_pid(4_000_000).unwrap();
        let meta = VmMeta {
            vm_id: "vm-kill-clean".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: dir.socket_path().to_string_lossy().into_owned(),
            pid: 4_000_000,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir.write_meta(&meta).unwrap();
        assert!(dir.meta_path().exists());
        assert!(dir.pid_path().exists());

        let client = ChClient::new(dir.socket_path());
        let mut state = VmRuntimeState {
            vm_id: VmId("vm-kill-clean".to_string()),
            pid: 4_000_000,
            socket_path: dir.socket_path(),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Running,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let result = kill_vm(&mut state, &client, &dir).await;
        assert!(result.is_ok());
        assert_eq!(state.current_phase, VmPhase::Stopped);
        // Entire runtime dir should be gone
        assert!(!dir.exists());
        assert!(!dir.meta_path().exists());
        assert!(!dir.pid_path().exists());
    }

    // -- reconnect mixed scenario ---------------------------------------------

    #[tokio::test]
    async fn reconnect_multiple_dirs_mixed_dead_and_orphan() {
        let tmp = TempDir::new().unwrap();

        // Dir 1: valid meta.json with dead PID -> failed
        let dir1 = RuntimeDir::create(tmp.path(), "vm-dead-1").unwrap();
        let meta1 = VmMeta {
            vm_id: "vm-dead-1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: tmp
                .path()
                .join("vm-dead-1/api.sock")
                .to_string_lossy()
                .into_owned(),
            pid: 4_000_001,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir1.write_meta(&meta1).unwrap();

        // Dir 2: valid meta.json with another dead PID -> failed
        let dir2 = RuntimeDir::create(tmp.path(), "vm-dead-2").unwrap();
        let meta2 = VmMeta {
            vm_id: "vm-dead-2".to_string(),
            pid: 4_000_002,
            socket_path: tmp
                .path()
                .join("vm-dead-2/api.sock")
                .to_string_lossy()
                .into_owned(),
            ..meta1.clone()
        };
        dir2.write_meta(&meta2).unwrap();

        // Dir 3: orphan (no meta.json) -> cleaned
        fs::create_dir_all(tmp.path().join("vm-orphan-mix")).unwrap();

        let (tx, _rx) = broadcast::channel(16);
        let report = reconnect(tmp.path(), tx).await;

        assert_eq!(report.recovered.len(), 0);
        assert_eq!(report.failed.len(), 2);
        assert_eq!(report.orphans_cleaned.len(), 1);
        assert!(report
            .orphans_cleaned
            .contains(&"vm-orphan-mix".to_string()));
        // Orphan dir should be removed
        assert!(!tmp.path().join("vm-orphan-mix").exists());
        // Dead pid dirs remain (forge decides)
        assert!(tmp.path().join("vm-dead-1").exists());
        assert!(tmp.path().join("vm-dead-2").exists());
    }

    // -- monitor with no VMs --------------------------------------------------

    #[tokio::test]
    async fn monitor_no_vms_no_crash() {
        let vms: Arc<RwLock<HashMap<String, Arc<Mutex<VmRuntimeState>>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let (tx, _rx) = broadcast::channel(16);

        let vms_clone = Arc::clone(&vms);
        let handle = tokio::spawn(async move {
            monitor_loop(vms_clone, tx, Duration::from_millis(10)).await;
        });

        // Let the monitor run a few iterations with zero VMs
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should still be running (not panicked)
        assert!(!handle.is_finished());
        handle.abort();
    }

    // -- monitor skips stopped VMs -------------------------------------------

    #[tokio::test]
    async fn monitor_skips_stopped_vms() {
        let vms: Arc<RwLock<HashMap<String, Arc<Mutex<VmRuntimeState>>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let state = VmRuntimeState {
            vm_id: VmId("vm-stopped".to_string()),
            pid: 4_000_000,
            socket_path: PathBuf::from("/tmp/nonexistent.sock"),
            cgroup_path: None,
            ch_binary_path: PathBuf::from("/bin/true"),
            ch_binary_version: "v1".to_string(),
            vcpus: 2,
            memory_mb: 512,
            launched_at: 0,
            last_ping_at: None,
            last_error: None,
            current_phase: VmPhase::Stopped,
            reconnect_source: ReconnectSource::FreshSpawn,
            image_name: None,
            instance_dir_path: None,
            runtime_handle: None,
        };

        let vm_arc = Arc::new(Mutex::new(state));
        {
            let mut map = vms.write().await;
            map.insert("vm-stopped".to_string(), Arc::clone(&vm_arc));
        }

        let (tx, mut rx) = broadcast::channel(16);
        let vms_clone = Arc::clone(&vms);
        let handle = tokio::spawn(async move {
            monitor_loop(vms_clone, tx, Duration::from_millis(10)).await;
        });

        // Let the monitor iterate a few times
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Should NOT emit any events for stopped VMs
        assert!(rx.try_recv().is_err());
        // Phase should remain Stopped
        let guard = vm_arc.lock().await;
        assert_eq!(guard.current_phase, VmPhase::Stopped);
        drop(guard);

        handle.abort();
    }

    // -- is_container_meta ----------------------------------------------------

    #[test]
    fn is_container_meta_valid_container() {
        let tmp = TempDir::new().unwrap();
        let meta_path = tmp.path().join("meta.json");
        fs::write(
            &meta_path,
            r#"{"container_id":"ctr-1","created_at":"2026-01-01T00:00:00Z","pid":1,"runtime_type":"container","vcpus":2,"memory_mb":512}"#,
        )
        .unwrap();
        assert!(is_container_meta(meta_path));
    }

    #[test]
    fn is_container_meta_vm_meta_returns_false() {
        let tmp = TempDir::new().unwrap();
        let dir = RuntimeDir::create(tmp.path(), "vm-test").unwrap();
        let meta = VmMeta {
            vm_id: "vm-test".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            socket_path: "/tmp/api.sock".to_string(),
            pid: 1,
            ch_binary: "/bin/true".to_string(),
            ch_version: "v1".to_string(),
            spec_hash: "hash:0".to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: None,
            disk_size_mb: None,
        };
        dir.write_meta(&meta).unwrap();
        assert!(!is_container_meta(dir.meta_path()));
    }

    #[test]
    fn is_container_meta_empty_file() {
        let tmp = TempDir::new().unwrap();
        let meta_path = tmp.path().join("meta.json");
        fs::write(&meta_path, "").unwrap();
        assert!(!is_container_meta(meta_path));
    }

    #[test]
    fn is_container_meta_non_json() {
        let tmp = TempDir::new().unwrap();
        let meta_path = tmp.path().join("meta.json");
        fs::write(&meta_path, "not valid json at all").unwrap();
        assert!(!is_container_meta(meta_path));
    }

    #[test]
    fn is_container_meta_missing_file() {
        let path = PathBuf::from("/tmp/nonexistent-test-dir-12345/meta.json");
        assert!(!is_container_meta(path));
    }
}
