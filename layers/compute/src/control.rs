//! Control socket types for the compute layer.
//!
//! Follows the same pattern as `syfrah_fabric::control`:
//! - `ComputeRequest` / `ComputeResponse` are the typed messages
//! - `ComputeLayerHandler` adapts a `VmManager` to the opaque `LayerHandler` trait

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use syfrah_api::{LayerHandler, LayerRequest, LayerResponse};
use tokio::net::UnixStream;

use crate::manager::VmManager;
use crate::types::{GpuMode, VmId, VmSpec};

use std::path::PathBuf as StdPathBuf;

// ---------------------------------------------------------------------------
// Request / Response enums
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub enum ComputeRequest {
    CreateVm {
        name: String,
        vcpus: u32,
        memory_mb: u32,
        image: String,
        gpu_bdf: Option<String>,
        tap: Option<String>,
        #[serde(default)]
        ssh_key: Option<String>,
        #[serde(default)]
        disk_size_mb: Option<u32>,
    },
    ListVms,
    GetVm {
        id: String,
    },
    StartVm {
        id: String,
    },
    StopVm {
        id: String,
        force: bool,
    },
    DeleteVm {
        id: String,
        #[serde(default)]
        retain_disk: bool,
    },
    RebootVm {
        id: String,
    },
    ResizeVm {
        id: String,
        vcpus: Option<u32>,
        memory_mb: Option<u32>,
    },
    Status,
    ImageList,
    ImageInspect {
        name: String,
    },
    ImagePull {
        name: String,
    },
    ImageImport {
        path: StdPathBuf,
        name: String,
        arch: String,
    },
    ImageDelete {
        name: String,
    },
    ImageCatalog,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ComputeResponse {
    /// Single VM info.
    Vm(serde_json::Value),
    /// List of VMs.
    VmList(Vec<serde_json::Value>),
    /// Compute status.
    Status(serde_json::Value),
    /// Success with no data.
    Ok,
    /// Error message.
    Error(String),
    /// List of image metadata.
    ImageList(Vec<serde_json::Value>),
    /// Single image metadata.
    ImageMeta(serde_json::Value),
    /// Image catalog (remote).
    ImageCatalog(serde_json::Value),
}

// ---------------------------------------------------------------------------
// ComputeLayerHandler — adapts VmManager to LayerHandler
// ---------------------------------------------------------------------------

pub struct ComputeLayerHandler {
    manager: Arc<VmManager>,
}

impl ComputeLayerHandler {
    pub fn new(manager: Arc<VmManager>) -> Self {
        Self { manager }
    }
}

/// Convert a VmStatus to a JSON value suitable for CLI output.
fn vm_status_to_json(s: &crate::types::VmStatus) -> serde_json::Value {
    let runtime = s.runtime.map(|r| r.to_string());
    serde_json::json!({
        "id": s.vm_id.0,
        "phase": format!("{:?}", s.phase),
        "vcpus": s.vcpus,
        "memory_mb": s.memory_mb,
        "image": s.image.as_deref().unwrap_or(""),
        "runtime": runtime,
        "created_at": s.created_at,
        "uptime_secs": s.uptime_secs,
    })
}

#[async_trait::async_trait]
impl LayerHandler for ComputeLayerHandler {
    async fn handle(&self, request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
        let req: ComputeRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(e) => {
                let resp = ComputeResponse::Error(format!("invalid compute request: {e}"));
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
        };

        let resp = handle_compute_request(&self.manager, req).await;
        serde_json::to_vec(&resp).unwrap_or_default()
    }
}

/// Fetch the image catalog using the manager's configured URL, cache path, and pull policy.
///
/// Extracted as a helper to avoid duplicating the fetch_catalog call between
/// ImagePull and ImageCatalog handlers.
async fn fetch_catalog_for(
    mgr: &VmManager,
) -> Result<crate::image::types::ImageCatalog, crate::image::error::ImageError> {
    let catalog_url = mgr.catalog_url().to_string();
    let cache_path = mgr.cache_path().to_path_buf();
    let pull_policy = mgr.pull_policy();
    crate::image::catalog::fetch_catalog(&catalog_url, &cache_path, pull_policy).await
}

async fn handle_compute_request(mgr: &VmManager, req: ComputeRequest) -> ComputeResponse {
    match req {
        ComputeRequest::CreateVm {
            name,
            vcpus,
            memory_mb,
            image,
            gpu_bdf,
            tap,
            ssh_key,
            disk_size_mb,
        } => {
            let gpu = match gpu_bdf {
                Some(bdf) => GpuMode::Passthrough { bdf },
                None => GpuMode::None,
            };
            let network = tap.map(|tap_name| crate::types::NetworkConfig {
                tap_name,
                mac: None,
            });
            let spec = VmSpec {
                id: VmId(name),
                vcpus,
                memory_mb,
                image,
                kernel: None,
                network,
                volumes: vec![],
                gpu,
                ssh_key,
                disk_size_mb,
            };
            match mgr.create_vm(spec).await {
                Ok(status) => ComputeResponse::Vm(vm_status_to_json(&status)),
                Err(e) => ComputeResponse::Error(e.to_string()),
            }
        }
        ComputeRequest::ListVms => {
            let vms = mgr.list().await;
            let json_list: Vec<serde_json::Value> = vms.iter().map(vm_status_to_json).collect();
            ComputeResponse::VmList(json_list)
        }
        ComputeRequest::GetVm { id } => match mgr.info(&id).await {
            Ok(status) => ComputeResponse::Vm(vm_status_to_json(&status)),
            Err(e) => ComputeResponse::Error(e.to_string()),
        },
        ComputeRequest::StartVm { id } => match mgr.start_vm(&id).await {
            Ok(status) => ComputeResponse::Vm(vm_status_to_json(&status)),
            Err(e) => ComputeResponse::Error(e.to_string()),
        },
        ComputeRequest::StopVm { id, force } => {
            let result = if force {
                mgr.shutdown_vm_force(&id).await
            } else {
                mgr.shutdown_vm(&id).await
            };
            match result {
                Ok(()) => match mgr.info(&id).await {
                    Ok(status) => ComputeResponse::Vm(vm_status_to_json(&status)),
                    Err(_) => ComputeResponse::Ok,
                },
                Err(e) => ComputeResponse::Error(e.to_string()),
            }
        }
        ComputeRequest::DeleteVm { id, retain_disk } => {
            match mgr.delete_vm_with_options(&id, retain_disk).await {
                Ok(()) => ComputeResponse::Ok,
                Err(e) => ComputeResponse::Error(e.to_string()),
            }
        }
        ComputeRequest::RebootVm { id } => {
            // Reboot = return current info (MVP — fake CH handles reboot API)
            match mgr.info(&id).await {
                Ok(status) => ComputeResponse::Vm(vm_status_to_json(&status)),
                Err(e) => ComputeResponse::Error(e.to_string()),
            }
        }
        ComputeRequest::ResizeVm { .. } => ComputeResponse::Error(
            "resize is not yet implemented — stop the VM and re-create it with the new specs"
                .to_string(),
        ),
        ComputeRequest::Status => {
            let vms = mgr.list().await;
            let total = vms.len() as u32;
            let running = vms
                .iter()
                .filter(|v| v.phase == crate::phase::VmPhase::Running)
                .count() as u32;
            let (status, warnings) = mgr.health_check();
            ComputeResponse::Status(serde_json::json!({
                "status": status,
                "warnings": warnings,
                "runtime": mgr.runtime_name(),
                "total_vms": total,
                "running_vms": running,
            }))
        }
        ComputeRequest::ImageList => match mgr.image_store().list() {
            Ok(images) => {
                let json_list: Vec<serde_json::Value> = images
                    .iter()
                    .map(|i| serde_json::to_value(i).unwrap_or_default())
                    .collect();
                ComputeResponse::ImageList(json_list)
            }
            Err(e) => ComputeResponse::Error(e.to_string()),
        },
        ComputeRequest::ImageInspect { name } => match mgr.image_store().get(&name) {
            Ok(Some(meta)) => {
                ComputeResponse::ImageMeta(serde_json::to_value(meta).unwrap_or_default())
            }
            Ok(None) => ComputeResponse::Error(format!("image not found: {name}")),
            Err(e) => ComputeResponse::Error(e.to_string()),
        },
        ComputeRequest::ImagePull { name } => match fetch_catalog_for(mgr).await {
            Ok(catalog) => {
                let mode = if mgr.runtime_name().starts_with("container") {
                    crate::image::types::RuntimeMode::Container
                } else {
                    crate::image::types::RuntimeMode::Vm
                };
                match crate::image::pull::pull_for_runtime(
                    mgr.image_store(),
                    &name,
                    &catalog,
                    &mode,
                )
                .await
                {
                    Ok(meta) => {
                        ComputeResponse::ImageMeta(serde_json::to_value(meta).unwrap_or_default())
                    }
                    Err(e) => ComputeResponse::Error(e.to_string()),
                }
            }
            Err(e) => ComputeResponse::Error(e.to_string()),
        },
        ComputeRequest::ImageImport { path, name, arch } => {
            match crate::image::import::import(mgr.image_store(), &path, &name, &arch) {
                Ok(meta) => {
                    ComputeResponse::ImageMeta(serde_json::to_value(meta).unwrap_or_default())
                }
                Err(e) => ComputeResponse::Error(e.to_string()),
            }
        }
        ComputeRequest::ImageDelete { name } => {
            let refcounts = {
                let mut refs = std::collections::HashMap::new();
                let count = mgr.image_refcount(&name).await;
                if count > 0 {
                    refs.insert(name.clone(), count);
                }
                refs
            };
            match crate::image::delete::delete(mgr.image_store(), &name, &refcounts) {
                Ok(()) => ComputeResponse::Ok,
                Err(e) => ComputeResponse::Error(e.to_string()),
            }
        }
        ComputeRequest::ImageCatalog => match fetch_catalog_for(mgr).await {
            Ok(catalog) => {
                ComputeResponse::ImageCatalog(serde_json::to_value(catalog).unwrap_or_default())
            }
            Err(e) => ComputeResponse::Error(e.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Client-side helper — send a compute request to the daemon
// ---------------------------------------------------------------------------

/// Send a compute request to the daemon via the Unix control socket.
pub async fn send_compute_request(
    socket_path: &Path,
    req: &ComputeRequest,
) -> Result<ComputeResponse, Box<dyn std::error::Error>> {
    let payload = serde_json::to_vec(req)?;
    let envelope = LayerRequest::Compute(payload);

    let mut stream = UnixStream::connect(socket_path).await?;
    syfrah_api::transport::write_message(&mut stream, &envelope).await?;
    let resp: LayerResponse = syfrah_api::transport::read_message(&mut stream).await?;

    match resp {
        LayerResponse::Compute(data) => {
            let compute_resp: ComputeResponse = serde_json::from_slice(&data)?;
            Ok(compute_resp)
        }
        LayerResponse::UnknownLayer(name) => Err(format!("unknown layer: {name}").into()),
        other => Err(format!("unexpected response variant: {other:?}").into()),
    }
}
