//! Container runtime backend (runsc / crun).
//!
//! `ContainerRuntime` implements [`ComputeRuntime`] by generating OCI bundles
//! and delegating to `crun` as the primary OCI runtime. `crun` works on all
//! Linux hosts with no special kernel requirements. `runsc` (gVisor) is
//! detected and logged as available but not used by default, since it requires
//! KVM/sandbox support that many VPS environments lack. Both runtimes share
//! the same OCI CLI interface.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::{ComputeError, ProcessError};
use crate::phase::VmPhase;
use crate::runtime_backend::{
    ComputeRuntime, RuntimeHandle, RuntimeInfo, RuntimeSpec, RuntimeType,
};

// ---------------------------------------------------------------------------
// ContainerMeta — persisted metadata for reconnect
// ---------------------------------------------------------------------------

/// Metadata written to `meta.json` inside each container's runtime directory.
///
/// This mirrors the shape of `VmMeta` from `process.rs` but carries
/// container-specific fields. The `runtime_type` field allows `reconnect` to
/// distinguish container dirs from VM dirs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerMeta {
    pub container_id: String,
    pub created_at: String,
    pub pid: u32,
    pub runtime_type: String,
    pub vcpus: u32,
    pub memory_mb: u32,
    /// Image name used to create this container (persisted for reconnect).
    #[serde(default)]
    pub image_name: Option<String>,
}

// ---------------------------------------------------------------------------
// ContainerRuntime
// ---------------------------------------------------------------------------

/// Container runtime backend using crun (primary) with runsc (gVisor) as optional.
///
/// Each container is an OCI bundle under `base_dir/{id}/` containing a
/// `config.json` and a `rootfs/` directory. The lifecycle is managed by
/// calling the OCI runtime binary directly (preferring `crun`; `runsc` is
/// detected but not used by default).
pub struct ContainerRuntime {
    /// Resolved path to the `crun` binary.
    crun_binary: PathBuf,
    /// Resolved path to the `runsc` (gVisor) binary.
    runsc_binary: PathBuf,
    /// Base directory for per-container runtime dirs (e.g., `/run/syfrah/vms`).
    base_dir: PathBuf,
    /// Cached preferred runtime binary path (resolved once in `new()`).
    preferred_runtime: PathBuf,
}

impl ContainerRuntime {
    /// Create a new `ContainerRuntime` by resolving `runsc` and/or `crun` binaries.
    ///
    /// At least one OCI runtime binary must be found. `runsc` is preferred;
    /// `crun` is used as a fallback.
    pub fn new(base_dir: PathBuf) -> Result<Self, ComputeError> {
        let crun = resolve_binary("crun").ok().unwrap_or_default();
        let runsc = resolve_binary("runsc").ok().unwrap_or_default();

        // At least one runtime must be available.
        if runsc.as_os_str().is_empty() && crun.as_os_str().is_empty() {
            return Err(ProcessError::SpawnFailed {
                reason:
                    "neither runsc nor crun binary found in /usr/local/bin/, /usr/bin/, or $PATH"
                        .to_string(),
            }
            .into());
        }

        // Cache the runtime decision once at construction time.
        let preferred = if !crun.as_os_str().is_empty() && crun.exists() {
            if !runsc.as_os_str().is_empty() && runsc.exists() {
                info!(
                    crun = %crun.display(),
                    runsc = %runsc.display(),
                    "runsc (gVisor) is available but not used by default; using crun"
                );
            } else {
                info!(runtime = %crun.display(), "using crun as OCI runtime");
            }
            crun.clone()
        } else {
            info!(runtime = %runsc.display(), "using runsc as OCI runtime (crun not found)");
            runsc.clone()
        };

        Ok(Self {
            crun_binary: crun,
            runsc_binary: runsc,
            base_dir,
            preferred_runtime: preferred,
        })
    }

    /// Get the resolved crun binary path.
    pub fn crun_binary(&self) -> &Path {
        &self.crun_binary
    }

    /// Get the resolved runsc binary path.
    pub fn runsc_binary(&self) -> &Path {
        &self.runsc_binary
    }

    /// Get the base directory for runtime dirs.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Return the preferred OCI runtime binary path (cached at construction time).
    fn runtime_binary(&self) -> &Path {
        &self.preferred_runtime
    }

    /// Execute an OCI runtime command (runsc or crun).
    ///
    /// For `runsc`, `--root /run/syfrah/runsc` is prepended so that runsc
    /// knows where to store container state. Both `runsc` and `crun` share the
    /// same OCI CLI interface (create, start, state, kill, delete).
    ///
    /// Returns stdout on success; maps failures to `ProcessError::SpawnFailed`.
    /// Execute a runtime command and capture its output.
    /// Used for commands like `state`, `kill`, `delete` that return quickly.
    async fn runtime_exec(&self, args: &[&str]) -> Result<String, ComputeError> {
        let binary = self.runtime_binary();
        let binary_name = binary
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let mut cmd = Command::new(binary);

        // runsc needs --root to know where to store container state.
        if binary_name == "runsc" {
            cmd.arg("--root").arg("/run/syfrah/runsc");
        }

        let output = cmd
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("{binary_name}: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProcessError::SpawnFailed {
                reason: format!("{binary_name} failed: {stderr}"),
            }
            .into());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Execute a runtime command without capturing output.
    /// Used for `run -d` where the child process inherits fds and
    /// `output()` would block until the container exits.
    async fn runtime_exec_detached(&self, args: &[&str]) -> Result<(), ComputeError> {
        let binary = self.runtime_binary();
        let binary_name = binary
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let mut cmd = Command::new(binary);

        if binary_name == "runsc" {
            cmd.arg("--root").arg("/run/syfrah/runsc");
        }

        let status = cmd
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("{binary_name}: {e}"),
            })?;

        if !status.success() {
            return Err(ProcessError::SpawnFailed {
                reason: format!("{binary_name} exited with code {:?}", status.code()),
            }
            .into());
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Binary resolution
// ---------------------------------------------------------------------------

/// Resolve a binary by name, checking well-known paths then `$PATH`.
///
/// Resolution order:
/// 1. `/usr/local/bin/{name}`
/// 2. `/usr/bin/{name}`
/// 3. `which {name}` (searches `$PATH`)
fn resolve_binary(name: &str) -> Result<PathBuf, ComputeError> {
    // 1. /usr/local/bin
    let local = PathBuf::from(format!("/usr/local/bin/{name}"));
    if local.exists() {
        return Ok(local);
    }

    // 2. /usr/bin
    let usr = PathBuf::from(format!("/usr/bin/{name}"));
    if usr.exists() {
        return Ok(usr);
    }

    // 3. Search $PATH via `which`
    if let Ok(output) = std::process::Command::new("which").arg(name).output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let path = PathBuf::from(path_str.trim());
            if path.exists() {
                return Ok(path);
            }
        }
    }

    Err(ProcessError::SpawnFailed {
        reason: format!("{name} binary not found in /usr/local/bin/, /usr/bin/, or $PATH"),
    }
    .into())
}

/// Check whether both `crun` and `runsc` binaries are available on this system.
///
/// Used by `select_runtime` to decide whether the container backend can be
/// offered as a fallback when KVM is absent.
pub fn container_binaries_available() -> bool {
    resolve_binary("runsc").is_ok() || resolve_binary("crun").is_ok()
}

// ---------------------------------------------------------------------------
// Auto-download fallback
// ---------------------------------------------------------------------------

/// crun version used for auto-download.
/// Kept in sync with the repo-root `CRUN_VERSION` file.
const CRUN_VERSION: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CRUN_VERSION"));

/// runsc (gVisor) release version used for auto-download.
/// Kept in sync with the repo-root `RUNSC_VERSION` file.
const RUNSC_VERSION: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../RUNSC_VERSION"));

/// URL template for downloading crun static binaries.
/// `{version}` and `{arch}` are substituted at runtime.
fn crun_download_url() -> String {
    let version = CRUN_VERSION.trim();
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    format!(
        "https://github.com/containers/crun/releases/download/{}/crun-{}-linux-{}",
        version, version, arch
    )
}

/// URL for downloading runsc (gVisor) static binary pinned to a specific release.
fn runsc_download_url() -> String {
    let version = RUNSC_VERSION.trim();
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    format!(
        "https://storage.googleapis.com/gvisor/releases/release/{}/{}/runsc",
        version, arch
    )
}

/// Ensure crun and runsc are available, downloading them if necessary.
///
/// This mirrors the `ensure_kernel` pattern in `boot.rs`. If a binary is
/// missing from the standard paths, it is downloaded to `/usr/local/bin/`.
pub async fn ensure_container_binaries() -> Result<(), ComputeError> {
    for (name, url_fn) in [
        ("crun", crun_download_url as fn() -> String),
        ("runsc", runsc_download_url),
    ] {
        if resolve_binary(name).is_ok() {
            continue;
        }

        let url = url_fn();
        let dest = PathBuf::from(format!("/usr/local/bin/{name}"));
        warn!("{name} not found, downloading from {url}...");

        let dest_clone = dest.clone();
        let name_owned = name.to_string();
        tokio::task::spawn_blocking(move || download_binary(&url, &dest_clone, &name_owned))
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("{name} download task panicked: {e}"),
            })??;

        info!("{name} downloaded and installed to {}", dest.display());
    }
    Ok(())
}

/// Download a binary from `url` to `dest` and make it executable.
fn download_binary(url: &str, dest: &Path, name: &str) -> Result<(), ComputeError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to build HTTP client for {name}: {e}"),
        })?;

    let response = client
        .get(url)
        .send()
        .map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to download {name} from {url}: {e}"),
        })?;

    if !response.status().is_success() {
        return Err(ProcessError::SpawnFailed {
            reason: format!(
                "failed to download {name} from {url}: HTTP {}",
                response.status()
            ),
        }
        .into());
    }

    let bytes = response.bytes().map_err(|e| ProcessError::SpawnFailed {
        reason: format!("failed to read {name} response body: {e}"),
    })?;

    // Guard against empty or truncated responses
    if bytes.len() < 1024 {
        return Err(ProcessError::SpawnFailed {
            reason: format!(
                "downloaded {name} binary is too small ({} bytes) — possible empty or truncated response",
                bytes.len()
            ),
        }
        .into());
    }

    // Validate ELF magic bytes (0x7f 'E' 'L' 'F')
    if bytes.len() >= 4 && &bytes[..4] != b"\x7fELF" {
        return Err(ProcessError::SpawnFailed {
            reason: format!(
                "downloaded {name} binary is not a valid ELF executable (bad magic bytes)"
            ),
        }
        .into());
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to create directory {}: {e}", parent.display()),
        })?;
    }

    std::fs::write(dest, &bytes).map_err(|e| ProcessError::SpawnFailed {
        reason: format!("failed to write {name} to {}: {e}", dest.display()),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dest, perms).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to set permissions on {}: {e}", dest.display()),
        })?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// OCI config.json generation
// ---------------------------------------------------------------------------

/// Generate a minimal OCI runtime spec (`config.json`) for the given workload.
fn generate_oci_config(id: &str, spec: &RuntimeSpec) -> serde_json::Value {
    serde_json::json!({
        "ociVersion": "1.0.0",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0 },
            "args": ["/bin/sh", "-c", "exec /sbin/init || exec sleep infinity"],
            "env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            ],
            "cwd": "/"
        },
        "root": {
            "path": "rootfs",
            "readonly": false
        },
        "mounts": [
            {
                "destination": "/proc",
                "type": "proc",
                "source": "proc"
            },
            {
                "destination": "/dev",
                "type": "tmpfs",
                "source": "tmpfs",
                "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
            },
            {
                "destination": "/dev/pts",
                "type": "devpts",
                "source": "devpts",
                "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]
            },
            {
                "destination": "/dev/shm",
                "type": "tmpfs",
                "source": "shm",
                "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
            },
            {
                "destination": "/sys",
                "type": "sysfs",
                "source": "sysfs",
                "options": ["nosuid", "noexec", "nodev", "ro"]
            }
        ],
        "hostname": id,
        "linux": {
            "resources": {
                "memory": { "limit": (spec.memory_mb as u64) * 1024 * 1024 },
                "cpu": { "shares": spec.vcpus * 1024 }
            },
            "namespaces": [
                { "type": "pid" },
                { "type": "mount" },
                { "type": "ipc" },
                { "type": "uts" },
                { "type": "network" }
            ]
        }
    })
}

// ---------------------------------------------------------------------------
// Helper: extract rootfs from archive
// ---------------------------------------------------------------------------

/// Prepare the rootfs directory for the container.
///
/// - If `rootfs_path` is a directory, use it directly (symlink/bind).
/// - If it is a `.tar.gz` / `.tgz`, extract into `{runtime_dir}/rootfs/`.
/// - Otherwise, return an error (raw disk images are not supported for
///   container mode).
async fn prepare_rootfs(rootfs_path: &Path, runtime_dir: &Path) -> Result<PathBuf, ComputeError> {
    let rootfs_dest = runtime_dir.join("rootfs");

    if rootfs_path.is_dir() {
        // Use the directory directly — symlink so the runtime sees "rootfs" in the bundle.
        #[cfg(unix)]
        tokio::fs::symlink(rootfs_path, &rootfs_dest)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!(
                    "failed to symlink rootfs {} -> {}: {e}",
                    rootfs_path.display(),
                    rootfs_dest.display()
                ),
            })?;
        return Ok(rootfs_dest);
    }

    let name = rootfs_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let staging_dir = runtime_dir.join("_oci_staging");
        tokio::fs::create_dir_all(&staging_dir)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to create staging dir: {e}"),
            })?;

        // Extract the archive into a staging area first.
        let output = Command::new("tar")
            .args(["xzf", &rootfs_path.to_string_lossy(), "-C"])
            .arg(&staging_dir)
            .output()
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to extract rootfs tar: {e}"),
            })?;

        if !output.status.success() {
            let image_name = rootfs_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .trim_end_matches("-oci.tar")
                .trim_end_matches("-oci")
                .to_string();
            return Err(ProcessError::SpawnFailed {
                reason: format!(
                    "Container image format not available for '{image_name}'. \
                     The OCI archive may be corrupt or missing. \
                     Try re-pulling with: syfrah compute image pull {image_name}"
                ),
            }
            .into());
        }

        // Explicit format detection — try each format in order with clear
        // diagnostics. No silent fallback: OCI layout -> Docker save -> flat
        // rootfs -> error.
        let is_oci_layout = staging_dir.join("oci-layout").exists();
        let is_docker_save = !is_oci_layout && staging_dir.join("manifest.json").exists();

        if is_oci_layout {
            debug!("detected OCI image layout (oci-layout file present)");
        } else if is_docker_save {
            debug!("detected Docker save archive (manifest.json present)");
        } else {
            debug!("no oci-layout or manifest.json — treating as flat rootfs tarball");
        }

        if is_oci_layout || is_docker_save {
            // OCI/Docker image archive — extract layer tarballs into rootfs.
            tokio::fs::create_dir_all(&rootfs_dest).await.map_err(|e| {
                ProcessError::SpawnFailed {
                    reason: format!("failed to create rootfs dir: {e}"),
                }
            })?;

            let layer_tars = find_layer_tars(&staging_dir, is_oci_layout).await?;
            if layer_tars.is_empty() {
                return Err(ProcessError::SpawnFailed {
                    reason: "OCI/Docker image archive contains no layer tarballs".to_string(),
                }
                .into());
            }

            for layer_tar in &layer_tars {
                debug!(layer = %layer_tar.display(), "extracting layer into rootfs");
                let out = Command::new("tar")
                    .args(["xf", &layer_tar.to_string_lossy(), "-C"])
                    .arg(&rootfs_dest)
                    .output()
                    .await
                    .map_err(|e| ProcessError::SpawnFailed {
                        reason: format!("failed to extract layer {}: {e}", layer_tar.display()),
                    })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(ProcessError::SpawnFailed {
                        reason: format!(
                            "tar failed extracting layer {}: {stderr}",
                            layer_tar.display()
                        ),
                    }
                    .into());
                }
            }
        } else {
            // Flat rootfs tarball — staging IS the rootfs.
            tokio::fs::rename(&staging_dir, &rootfs_dest)
                .await
                .map_err(|e| ProcessError::SpawnFailed {
                    reason: format!("failed to rename staging dir to rootfs: {e}"),
                })?;
            return Ok(rootfs_dest);
        }

        // Clean up staging directory.
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;

        return Ok(rootfs_dest);
    }

    // .raw or unsupported format — show image name instead of full path.
    let image_name = rootfs_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .trim_end_matches(".tar")
        .trim_end_matches("-oci")
        .to_string();
    Err(ProcessError::SpawnFailed {
        reason: format!(
            "Container image format not available for '{image_name}'. \
             Raw disk images are not supported in container mode — use a .tar.gz OCI archive."
        ),
    }
    .into())
}

/// Find layer tarballs inside an OCI image layout or Docker save archive.
///
/// Detection is explicit with no silent fallback chains:
/// 1. If `oci-layout` exists -> parse OCI layout (error on failure)
/// 2. Else if `manifest.json` exists -> parse Docker save format (error on failure)
/// 3. Else if subdirectories contain `layer.tar` -> flat rootfs format
/// 4. Otherwise -> return error "unsupported archive format"
async fn find_layer_tars(
    staging_dir: &Path,
    is_oci_layout: bool,
) -> Result<Vec<PathBuf>, ComputeError> {
    // 1. OCI layout: parse index.json -> manifest -> layers (no silent fallthrough).
    if is_oci_layout {
        return find_oci_layout_layers(staging_dir).await;
    }

    // 2. Docker save: parse manifest.json for layer paths.
    let manifest_path = staging_dir.join("manifest.json");
    if manifest_path.exists() {
        let data = tokio::fs::read_to_string(&manifest_path)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to read Docker manifest.json: {e}"),
            })?;
        let manifests: Vec<serde_json::Value> =
            serde_json::from_str(&data).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to parse Docker manifest.json: {e}"),
            })?;
        let manifest = manifests.first().ok_or_else(|| ProcessError::SpawnFailed {
            reason: "Docker manifest.json is an empty array".to_string(),
        })?;
        let layers = manifest["Layers"]
            .as_array()
            .ok_or_else(|| ProcessError::SpawnFailed {
                reason: "Docker manifest.json has no Layers array".to_string(),
            })?;
        let mut paths = Vec::new();
        for l in layers {
            let layer_str = l.as_str().ok_or_else(|| ProcessError::SpawnFailed {
                reason: "Docker manifest.json Layers entry is not a string".to_string(),
            })?;
            let path = safe_join(staging_dir, layer_str)?;
            if !path.exists() {
                return Err(ProcessError::SpawnFailed {
                    reason: format!("referenced layer blob does not exist: {}", path.display()),
                }
                .into());
            }
            paths.push(path);
        }
        return Ok(paths);
    }

    // 3. Flat rootfs: glob for layer.tar files in subdirectories.
    let mut layer_tars = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(staging_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                let layer_tar = path.join("layer.tar");
                if layer_tar.exists() {
                    layer_tars.push(layer_tar);
                }
            }
        }
    }
    if !layer_tars.is_empty() {
        layer_tars.sort();
        return Ok(layer_tars);
    }

    // 4. No recognized format.
    Err(ProcessError::SpawnFailed {
        reason:
            "unsupported archive format: no oci-layout, manifest.json, or layer.tar files found"
                .to_string(),
    }
    .into())
}

/// Expected OCI manifest media types.
const OCI_MANIFEST_MEDIA_TYPES: &[&str] = &[
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Parse OCI image layout: index.json -> manifest blob -> layer blobs.
async fn find_oci_layout_layers(staging_dir: &Path) -> Result<Vec<PathBuf>, ComputeError> {
    let index_path = staging_dir.join("index.json");
    let index_data =
        tokio::fs::read_to_string(&index_path)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to read OCI index.json: {e}"),
            })?;
    let index: serde_json::Value =
        serde_json::from_str(&index_data).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to parse OCI index.json: {e}"),
        })?;

    // Get the first manifest descriptor.
    let manifest_descriptor = index["manifests"]
        .as_array()
        .and_then(|m| m.first())
        .ok_or_else(|| ProcessError::SpawnFailed {
            reason: "OCI index.json has no manifest entries".to_string(),
        })?;

    // Validate mediaType on the manifest descriptor.
    if let Some(media_type) = manifest_descriptor["mediaType"].as_str() {
        if !OCI_MANIFEST_MEDIA_TYPES.contains(&media_type) {
            warn!(
                media_type = media_type,
                "OCI manifest descriptor has unexpected mediaType"
            );
        }
    } else {
        warn!("OCI manifest descriptor is missing mediaType field");
    }

    let manifest_digest =
        manifest_descriptor["digest"]
            .as_str()
            .ok_or_else(|| ProcessError::SpawnFailed {
                reason: "OCI manifest descriptor has no digest".to_string(),
            })?;

    // digest is "sha256:<hex>" — resolve to blobs/sha256/<hex>
    let blob_path = digest_to_blob_path(staging_dir, manifest_digest)?;
    if !blob_path.exists() {
        return Err(ProcessError::SpawnFailed {
            reason: format!(
                "referenced manifest blob does not exist: {}",
                blob_path.display()
            ),
        }
        .into());
    }
    let manifest_data =
        tokio::fs::read_to_string(&blob_path)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to read OCI manifest blob: {e}"),
            })?;
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_data).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to parse OCI manifest: {e}"),
        })?;

    let layers = manifest["layers"]
        .as_array()
        .ok_or_else(|| ProcessError::SpawnFailed {
            reason: "OCI manifest has no layers array".to_string(),
        })?;

    let mut paths = Vec::new();
    for layer in layers {
        let digest = layer["digest"]
            .as_str()
            .ok_or_else(|| ProcessError::SpawnFailed {
                reason: "OCI manifest layer entry missing digest field".to_string(),
            })?;
        let path = digest_to_blob_path(staging_dir, digest)?;
        if !path.exists() {
            return Err(ProcessError::SpawnFailed {
                reason: format!("referenced layer blob does not exist: {}", path.display()),
            }
            .into());
        }
        paths.push(path);
    }

    Ok(paths)
}

/// Convert an OCI digest like `sha256:abc123` to `{staging}/blobs/sha256/abc123`.
///
/// The resolved path is validated to stay within `staging_dir` to prevent
/// path traversal via crafted digest strings.
fn digest_to_blob_path(staging_dir: &Path, digest: &str) -> Result<PathBuf, ComputeError> {
    let parts: Vec<&str> = digest.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(ProcessError::SpawnFailed {
            reason: format!("invalid OCI digest format: {digest}"),
        }
        .into());
    }
    let joined = staging_dir.join("blobs").join(parts[0]).join(parts[1]);
    safe_join_check(staging_dir, &joined, digest)?;
    Ok(joined)
}

/// Validate that `resolved` stays within `base_dir`, preventing path traversal.
///
/// Uses canonicalize when the path exists on disk; otherwise validates the
/// joined path components do not escape via `..`.
fn safe_join_check(base_dir: &Path, resolved: &Path, untrusted: &str) -> Result<(), ComputeError> {
    let canonical = resolved
        .canonicalize()
        .unwrap_or_else(|_| resolved.to_path_buf());
    let base_canonical = base_dir
        .canonicalize()
        .unwrap_or_else(|_| base_dir.to_path_buf());
    if !canonical.starts_with(&base_canonical) {
        return Err(ProcessError::SpawnFailed {
            reason: format!("path traversal detected: {untrusted}"),
        }
        .into());
    }
    Ok(())
}

/// Join a base directory with an untrusted relative path, returning an error
/// if the result would escape `base_dir`.
fn safe_join(base_dir: &Path, untrusted: &str) -> Result<PathBuf, ComputeError> {
    let joined = base_dir.join(untrusted);
    safe_join_check(base_dir, &joined, untrusted)?;
    Ok(joined)
}

// ---------------------------------------------------------------------------
// ComputeRuntime implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl ComputeRuntime for ContainerRuntime {
    async fn create(&self, id: &str, spec: &RuntimeSpec) -> Result<RuntimeHandle, ComputeError> {
        let runtime_dir = self.base_dir.join(id);
        info!(
            container_id = %id,
            runtime_dir = %runtime_dir.display(),
            "ContainerRuntime::create: setting up OCI bundle"
        );

        // 1. Create runtime directory.
        tokio::fs::create_dir_all(&runtime_dir)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!(
                    "failed to create runtime dir {}: {e}",
                    runtime_dir.display()
                ),
            })?;

        // 2. Prepare rootfs.
        prepare_rootfs(&spec.rootfs_path, &runtime_dir).await?;

        // 3. Write config.json.
        let config = generate_oci_config(id, spec);
        let config_path = runtime_dir.join("config.json");
        let config_json =
            serde_json::to_string_pretty(&config).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to serialize OCI config: {e}"),
            })?;
        tokio::fs::write(&config_path, config_json)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to write config.json: {e}"),
            })?;

        // 4. runtime run -d --bundle {runtime_dir} {id}
        //    `run -d` does create+start in one call and detaches immediately,
        //    avoiding the deadlock where `create` blocks waiting for `start`.
        let runtime_dir_str = runtime_dir.to_string_lossy().to_string();
        self.runtime_exec_detached(&["run", "-d", "--bundle", &runtime_dir_str, id])
            .await?;

        // 5. Get PID from runtime state.
        let state_json = self.runtime_exec(&["state", id]).await?;
        let state: serde_json::Value =
            serde_json::from_str(&state_json).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to parse runtime state JSON: {e}"),
            })?;
        let pid =
            state["pid"]
                .as_u64()
                .filter(|&p| p > 0)
                .ok_or_else(|| ProcessError::SpawnFailed {
                    reason: "runtime state missing valid pid".to_string(),
                })? as u32;

        // 7. Write meta.json for reconnect.
        let now = chrono_now_iso8601();
        let meta = ContainerMeta {
            container_id: id.to_string(),
            created_at: now,
            pid,
            runtime_type: RuntimeType::Container.to_string(),
            vcpus: spec.vcpus,
            memory_mb: spec.memory_mb,
            image_name: spec.image_name.clone(),
        };
        let meta_json =
            serde_json::to_string_pretty(&meta).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to serialize meta.json: {e}"),
            })?;
        let meta_tmp = runtime_dir.join(".meta.json.tmp");
        let meta_path = runtime_dir.join("meta.json");
        tokio::fs::write(&meta_tmp, meta_json)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to write meta.json tmp: {e}"),
            })?;
        {
            let f = std::fs::File::open(&meta_tmp).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to open meta.json tmp for sync: {e}"),
            })?;
            f.sync_all().map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to sync meta.json tmp: {e}"),
            })?;
        }
        tokio::fs::rename(&meta_tmp, &meta_path)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to rename meta.json tmp: {e}"),
            })?;

        info!(
            container_id = %id,
            pid = pid,
            "ContainerRuntime::create: container started"
        );

        // 8. Return handle.
        Ok(RuntimeHandle {
            id: id.to_string(),
            pid,
            runtime_type: RuntimeType::Container,
            runtime_dir,
            vcpus: Some(spec.vcpus),
            memory_mb: Some(spec.memory_mb),
            launched_at: None,
            image_name: spec.image_name.clone(),
        })
    }

    async fn start(&self, handle: &RuntimeHandle) -> Result<RuntimeHandle, ComputeError> {
        info!(
            container_id = %handle.id,
            "ContainerRuntime::start: restarting stopped container"
        );

        // The OCI lifecycle doesn't allow restarting a stopped container directly.
        // Delete the stopped container state, then re-run it using the existing
        // bundle (rootfs + config.json are still in the runtime dir).
        let _ = self.runtime_exec(&["delete", "--force", &handle.id]).await;

        // Re-run the container from the existing bundle directory.
        let runtime_dir_str = handle.runtime_dir.to_string_lossy().to_string();
        self.runtime_exec_detached(&["run", "-d", "--bundle", &runtime_dir_str, &handle.id])
            .await?;

        // Get the new PID from runtime state.
        let state_json = self.runtime_exec(&["state", &handle.id]).await?;
        let state: serde_json::Value =
            serde_json::from_str(&state_json).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to parse runtime state JSON: {e}"),
            })?;
        let pid =
            state["pid"]
                .as_u64()
                .filter(|&p| p > 0)
                .ok_or_else(|| ProcessError::SpawnFailed {
                    reason: "runtime state missing valid pid".to_string(),
                })? as u32;

        // Update meta.json with new timestamp and PID (atomic: tmp + sync + rename).
        let now = chrono_now_iso8601();
        let meta_tmp = handle.runtime_dir.join(".meta.json.tmp");
        let meta_path = handle.runtime_dir.join("meta.json");
        let old_meta = read_container_meta(&handle.runtime_dir)?;
        let meta = ContainerMeta {
            pid,
            created_at: now,
            ..old_meta
        };
        let meta_json =
            serde_json::to_string_pretty(&meta).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to serialize meta.json: {e}"),
            })?;
        tokio::fs::write(&meta_tmp, meta_json)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to write meta.json tmp: {e}"),
            })?;
        {
            let f = std::fs::File::open(&meta_tmp).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to open meta.json tmp for sync: {e}"),
            })?;
            f.sync_all().map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to sync meta.json tmp: {e}"),
            })?;
        }
        tokio::fs::rename(&meta_tmp, &meta_path)
            .await
            .map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to rename meta.json tmp: {e}"),
            })?;

        info!(
            container_id = %handle.id,
            pid = pid,
            "ContainerRuntime::start: container restarted"
        );

        Ok(RuntimeHandle {
            id: handle.id.clone(),
            pid,
            runtime_type: RuntimeType::Container,
            runtime_dir: handle.runtime_dir.clone(),
            vcpus: handle.vcpus,
            memory_mb: handle.memory_mb,
            launched_at: None,
            image_name: handle.image_name.clone(),
        })
    }

    async fn stop(&self, handle: &RuntimeHandle, force: bool) -> Result<(), ComputeError> {
        let signal = if force { "SIGKILL" } else { "SIGTERM" };
        debug!(
            container_id = %handle.id,
            signal = signal,
            "ContainerRuntime::stop"
        );

        self.runtime_exec(&["kill", &handle.id, signal]).await?;

        if !force {
            // Poll for the container to exit (up to 30 seconds).
            for _ in 0..30 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if !self.is_alive(handle).await {
                    return Ok(());
                }
            }
            // Escalate to SIGKILL after timeout.
            warn!(
                container_id = %handle.id,
                "ContainerRuntime::stop: graceful shutdown timed out, sending SIGKILL"
            );
            self.runtime_exec(&["kill", &handle.id, "SIGKILL"]).await?;
        }

        Ok(())
    }

    async fn delete(&self, handle: &RuntimeHandle) -> Result<(), ComputeError> {
        debug!(
            container_id = %handle.id,
            "ContainerRuntime::delete"
        );

        // OCI runtime delete (force to handle stopped containers).
        // Ignore errors from delete — the container may already be gone.
        let _ = self.runtime_exec(&["delete", "--force", &handle.id]).await;

        // Clean up the runtime directory.
        if handle.runtime_dir.exists() {
            tokio::fs::remove_dir_all(&handle.runtime_dir)
                .await
                .map_err(|e| ProcessError::SpawnFailed {
                    reason: format!(
                        "failed to remove runtime dir {}: {e}",
                        handle.runtime_dir.display()
                    ),
                })?;
        }

        info!(
            container_id = %handle.id,
            "ContainerRuntime::delete: container deleted and cleaned up"
        );
        Ok(())
    }

    async fn info(&self, handle: &RuntimeHandle) -> Result<RuntimeInfo, ComputeError> {
        let state_json = self.runtime_exec(&["state", &handle.id]).await?;
        let state: serde_json::Value =
            serde_json::from_str(&state_json).map_err(|e| ProcessError::SpawnFailed {
                reason: format!("failed to parse runtime state JSON: {e}"),
            })?;

        let status = state["status"].as_str().unwrap_or("unknown");
        let pid =
            state["pid"]
                .as_u64()
                .filter(|&p| p > 0)
                .ok_or_else(|| ProcessError::SpawnFailed {
                    reason: "runtime state missing valid pid".to_string(),
                })? as u32;

        let phase = match status {
            "creating" => VmPhase::Pending,
            "created" => VmPhase::Pending,
            "running" => VmPhase::Running,
            "stopped" => VmPhase::Stopped,
            _ => VmPhase::Failed,
        };

        // Best-effort uptime from meta.json created_at timestamp.
        let uptime_secs = if phase == VmPhase::Running {
            read_container_meta(&handle.runtime_dir).ok().and_then(|m| {
                parse_iso8601_to_unix(&m.created_at).map(|created| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    now.saturating_sub(created)
                })
            })
        } else {
            None
        };

        Ok(RuntimeInfo {
            phase,
            pid,
            uptime_secs,
            runtime_type: RuntimeType::Container,
        })
    }

    async fn is_alive(&self, handle: &RuntimeHandle) -> bool {
        // Try runtime state first; fall back to kill(pid, 0).
        if let Ok(state_json) = self.runtime_exec(&["state", &handle.id]).await {
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&state_json) {
                return state["status"].as_str() == Some("running");
            }
        }
        // Fallback: signal check.
        unsafe { libc::kill(handle.pid as i32, 0) == 0 }
    }

    async fn reconnect(&self, runtime_dir_base: &Path) -> Vec<RuntimeHandle> {
        let entries = match std::fs::read_dir(runtime_dir_base) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut handles = Vec::new();

        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }

            let meta = match read_container_meta(&dir) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Only recover containers (skip VM meta.json files).
            if meta.runtime_type != "container" {
                continue;
            }

            // Verify the container is still running.
            let id = &meta.container_id;
            let alive = if let Ok(state_json) = self.runtime_exec(&["state", id]).await {
                serde_json::from_str::<serde_json::Value>(&state_json)
                    .map(|s| s["status"].as_str() == Some("running"))
                    .unwrap_or(false)
            } else {
                false
            };

            if alive {
                info!(
                    container_id = %id,
                    pid = meta.pid,
                    "ContainerRuntime::reconnect: found live container"
                );
                handles.push(RuntimeHandle {
                    id: id.clone(),
                    pid: meta.pid,
                    runtime_type: RuntimeType::Container,
                    runtime_dir: dir,
                    vcpus: Some(meta.vcpus),
                    memory_mb: Some(meta.memory_mb),
                    launched_at: parse_iso8601_to_unix(&meta.created_at),
                    image_name: meta.image_name.clone(),
                });
            } else {
                warn!(
                    container_id = %id,
                    pid = meta.pid,
                    dir = %dir.display(),
                    "ContainerRuntime::reconnect: dead container, cleaning up runtime dir"
                );
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    warn!(
                        container_id = %id,
                        dir = %dir.display(),
                        error = %e,
                        "ContainerRuntime::reconnect: failed to remove dead container dir"
                    );
                }
            }
        }

        handles
    }

    fn name(&self) -> &str {
        "container (gVisor)"
    }

    fn health_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.crun_binary.exists() {
            warnings.push("crun binary not found".to_string());
        }
        if !self.runsc_binary.exists() {
            warnings.push("runsc (gVisor) binary not found".to_string());
        }
        warnings
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `meta.json` from a container runtime directory.
fn read_container_meta(runtime_dir: &Path) -> Result<ContainerMeta, ComputeError> {
    let path = runtime_dir.join("meta.json");
    let data = std::fs::read_to_string(&path).map_err(|e| ProcessError::SpawnFailed {
        reason: format!("failed to read {}: {e}", path.display()),
    })?;
    Ok(
        serde_json::from_str(&data).map_err(|e| ProcessError::SpawnFailed {
            reason: format!("failed to parse {}: {e}", path.display()),
        })?,
    )
}

/// Return the current time as an ISO 8601 string (UTC).
fn chrono_now_iso8601() -> String {
    // Use std time to avoid adding chrono dependency.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO 8601-ish format: "1970-01-01T00:00:00Z"
    let secs_per_day: u64 = 86400;
    let days = now / secs_per_day;
    let rem = now % secs_per_day;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    // Approximate date from days since epoch (good enough for metadata).
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

/// Parse a simple ISO 8601 timestamp to Unix epoch seconds (best-effort).
fn parse_iso8601_to_unix(s: &str) -> Option<u64> {
    // Expected: "YYYY-MM-DDThh:mm:ssZ"
    let parts: Vec<&str> = s.split('T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let time_str = parts[1].trim_end_matches('Z');
    let time_parts: Vec<u64> = time_str.split(':').filter_map(|p| p.parse().ok()).collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }
    // Rough conversion — good enough for uptime calculation.
    let (y, m, d) = (date_parts[0], date_parts[1], date_parts[2]);
    let days = ymd_to_days(y, m, d);
    Some(days * 86400 + time_parts[0] * 3600 + time_parts[1] * 60 + time_parts[2])
}

/// Convert (year, month, day) to days since Unix epoch.
fn ymd_to_days(year: u64, month: u64, day: u64) -> u64 {
    // Inverse of days_to_ymd (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    days as u64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GpuMode;

    #[test]
    fn generate_oci_config_sets_hostname() {
        let spec = RuntimeSpec {
            vcpus: 2,
            memory_mb: 512,
            rootfs_path: PathBuf::from("/tmp/rootfs"),
            cloud_init_path: None,
            network: None,
            gpu: GpuMode::None,
            image_name: None,
        };
        let config = generate_oci_config("test-vm-1", &spec);
        assert_eq!(config["hostname"], "test-vm-1");
    }

    #[test]
    fn generate_oci_config_sets_memory_limit() {
        let spec = RuntimeSpec {
            vcpus: 4,
            memory_mb: 2048,
            rootfs_path: PathBuf::from("/tmp/rootfs"),
            cloud_init_path: None,
            network: None,
            gpu: GpuMode::None,
            image_name: None,
        };
        let config = generate_oci_config("mem-test", &spec);
        let limit = config["linux"]["resources"]["memory"]["limit"]
            .as_u64()
            .unwrap();
        assert_eq!(limit, 2048 * 1024 * 1024);
    }

    #[test]
    fn generate_oci_config_sets_cpu_shares() {
        let spec = RuntimeSpec {
            vcpus: 4,
            memory_mb: 1024,
            rootfs_path: PathBuf::from("/tmp/rootfs"),
            cloud_init_path: None,
            network: None,
            gpu: GpuMode::None,
            image_name: None,
        };
        let config = generate_oci_config("cpu-test", &spec);
        let shares = config["linux"]["resources"]["cpu"]["shares"]
            .as_u64()
            .unwrap();
        assert_eq!(shares, 4 * 1024);
    }

    #[test]
    fn generate_oci_config_has_namespaces() {
        let spec = RuntimeSpec {
            vcpus: 1,
            memory_mb: 256,
            rootfs_path: PathBuf::from("/tmp/rootfs"),
            cloud_init_path: None,
            network: None,
            gpu: GpuMode::None,
            image_name: None,
        };
        let config = generate_oci_config("ns-test", &spec);
        let namespaces = config["linux"]["namespaces"].as_array().unwrap();
        assert_eq!(namespaces.len(), 5);
        let ns_types: Vec<&str> = namespaces
            .iter()
            .filter_map(|n| n["type"].as_str())
            .collect();
        assert!(ns_types.contains(&"pid"));
        assert!(ns_types.contains(&"mount"));
        assert!(ns_types.contains(&"network"));
    }

    #[test]
    fn generate_oci_config_oci_version() {
        let spec = RuntimeSpec {
            vcpus: 1,
            memory_mb: 256,
            rootfs_path: PathBuf::from("/tmp/rootfs"),
            cloud_init_path: None,
            network: None,
            gpu: GpuMode::None,
            image_name: None,
        };
        let config = generate_oci_config("ver-test", &spec);
        assert_eq!(config["ociVersion"], "1.0.0");
    }

    #[test]
    fn container_meta_serde_roundtrip() {
        let meta = ContainerMeta {
            container_id: "ctr-1".to_string(),
            created_at: "2026-01-15T10:30:00Z".to_string(),
            pid: 42,
            runtime_type: RuntimeType::Container.to_string(),
            vcpus: 2,
            memory_mb: 512,
            image_name: Some("ubuntu-24.04".to_string()),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: ContainerMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.container_id, "ctr-1");
        assert_eq!(back.pid, 42);
        assert_eq!(back.runtime_type, RuntimeType::Container.to_string());
    }

    #[test]
    fn container_runtime_name() {
        // We can't construct ContainerRuntime without binaries on the host,
        // but we can test the name method via a direct struct construction.
        let rt = ContainerRuntime {
            crun_binary: PathBuf::from("/bin/true"),
            runsc_binary: PathBuf::from("/bin/true"),
            base_dir: PathBuf::from("/tmp/vms"),
            preferred_runtime: PathBuf::from("/bin/true"),
        };
        assert_eq!(rt.name(), "container (gVisor)");
    }

    #[test]
    fn container_runtime_accessors() {
        let rt = ContainerRuntime {
            crun_binary: PathBuf::from("/usr/local/bin/crun"),
            runsc_binary: PathBuf::from("/usr/local/bin/runsc"),
            base_dir: PathBuf::from("/run/syfrah/vms"),
            preferred_runtime: PathBuf::from("/usr/local/bin/crun"),
        };
        assert_eq!(rt.crun_binary(), Path::new("/usr/local/bin/crun"));
        assert_eq!(rt.runsc_binary(), Path::new("/usr/local/bin/runsc"));
        assert_eq!(rt.base_dir(), Path::new("/run/syfrah/vms"));
    }

    #[test]
    fn days_to_ymd_epoch() {
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2024-01-01 is day 19723
        let (y, m, d) = days_to_ymd(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }

    #[test]
    fn chrono_now_iso8601_format() {
        let ts = chrono_now_iso8601();
        // Should be roughly "YYYY-MM-DDThh:mm:ssZ"
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
        assert_eq!(ts.len(), 20);
    }

    #[test]
    fn parse_iso8601_roundtrip() {
        let ts = "2026-03-28T12:00:00Z";
        let secs = parse_iso8601_to_unix(ts).unwrap();
        assert!(secs > 0);
    }

    #[test]
    fn parse_iso8601_invalid() {
        assert!(parse_iso8601_to_unix("not-a-date").is_none());
        assert!(parse_iso8601_to_unix("").is_none());
    }

    #[tokio::test]
    async fn container_runtime_reconnect_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rt = ContainerRuntime {
            crun_binary: PathBuf::from("/bin/true"),
            runsc_binary: PathBuf::from("/bin/true"),
            base_dir: tmp.path().to_path_buf(),
            preferred_runtime: PathBuf::from("/bin/true"),
        };
        let handles = rt.reconnect(tmp.path()).await;
        assert!(handles.is_empty());
    }

    #[tokio::test]
    async fn container_runtime_is_alive_dead_pid() {
        let rt = ContainerRuntime {
            crun_binary: PathBuf::from("/bin/false"),
            runsc_binary: PathBuf::from("/bin/false"),
            base_dir: PathBuf::from("/tmp/vms"),
            preferred_runtime: PathBuf::from("/bin/false"),
        };
        let handle = RuntimeHandle {
            id: "ctr-dead".to_string(),
            pid: 4_000_000, // nonexistent PID
            runtime_type: RuntimeType::Container,
            runtime_dir: PathBuf::from("/tmp/nonexistent"),
            vcpus: None,
            memory_mb: None,
            launched_at: None,
            image_name: None,
        };
        assert!(!rt.is_alive(&handle).await);
    }

    #[test]
    fn container_binaries_not_available_on_ci() {
        // On most CI hosts, crun and runsc are not installed.
        // This test verifies the function doesn't panic.
        let _ = container_binaries_available();
    }

    #[test]
    fn read_container_meta_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = read_container_meta(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn read_container_meta_valid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let meta = ContainerMeta {
            container_id: "ctr-2".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            pid: 100,
            runtime_type: RuntimeType::Container.to_string(),
            vcpus: 1,
            memory_mb: 256,
            image_name: None,
        };
        let json = serde_json::to_string_pretty(&meta).unwrap();
        std::fs::write(tmp.path().join("meta.json"), json).unwrap();

        let loaded = read_container_meta(tmp.path()).unwrap();
        assert_eq!(loaded.container_id, "ctr-2");
        assert_eq!(loaded.pid, 100);
    }
}
