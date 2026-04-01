use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::capacity::CapacityTracker;
use crate::generation::GenerationTracker;
use crate::task::TaskStore;

/// Shared application state for all Forge HTTP handlers.
#[derive(Clone)]
pub struct ForgeState {
    /// When the Forge server started (for uptime calculation).
    pub started_at: Instant,
    /// Task store for operation tracking.
    pub task_store: Option<Arc<TaskStore>>,
    /// VmManager for compute operations.
    pub vm_manager: Option<Arc<syfrah_compute::VmManager>>,
    /// Capacity tracker for admission control.
    pub capacity: Option<Arc<CapacityTracker>>,
    /// Org store for subnet/VPC resolution.
    pub org_store: Option<Arc<syfrah_org::OrgStore>>,
    /// Network backend for bridge/VXLAN/TAP/nftables operations.
    pub network_backend: Option<Arc<dyn syfrah_overlay::NetworkBackend>>,
    /// Generation tracker for resource drift detection.
    pub generation_tracker: Option<Arc<GenerationTracker>>,
}

/// Standard error response with FORGE_ prefix codes.
#[derive(Serialize, Deserialize, Debug)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}

/// Health check response.
#[derive(Serialize, Deserialize, Debug)]
pub struct HealthResponse {
    pub status: String,
    pub uptime: u64,
}

/// Query parameters for listing tasks.
#[derive(Deserialize, Debug)]
pub struct TaskListQuery {
    pub resource_id: Option<String>,
}

/// Request body for creating an instance.
#[derive(Deserialize, Debug)]
pub struct CreateInstanceRequest {
    pub name: String,
    pub image: String,
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default)]
    pub subnet: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub ssh_key: Option<String>,
    #[serde(default)]
    pub disk_size_mb: Option<u32>,
    #[serde(default)]
    pub security_groups: Vec<String>,
}

fn default_vcpus() -> u32 {
    1
}
fn default_memory() -> u32 {
    512
}

/// Instance action for start/stop/reboot.
#[derive(Deserialize, Debug)]
pub struct InstanceActionRequest {
    pub action: Option<String>,
}

/// Request body for creating/ensuring a bridge.
#[derive(Deserialize, Debug)]
pub struct CreateBridgeRequest {
    /// VPC identifier — used to derive the bridge name via naming conventions.
    pub vpc_id: String,
}

/// Response for bridge operations.
#[derive(Serialize, Deserialize, Debug)]
pub struct BridgeResponse {
    pub id: String,
    pub bridge_name: String,
    pub vpc_id: String,
    pub generation: Option<crate::generation::ResourceGeneration>,
}

/// Request body for creating/ensuring a VXLAN.
#[derive(Deserialize, Debug)]
pub struct CreateVxlanRequest {
    /// VPC identifier — used to derive the VXLAN name via naming conventions.
    pub vpc_id: String,
    /// VXLAN Network Identifier.
    pub vni: u32,
    /// Local VTEP IP address.
    pub local_ip: String,
    /// VXLAN UDP port (default 4789).
    #[serde(default = "default_vxlan_port")]
    pub port: u16,
}

fn default_vxlan_port() -> u16 {
    4789
}

/// Response for VXLAN operations.
#[derive(Serialize, Deserialize, Debug)]
pub struct VxlanResponse {
    pub id: String,
    pub vxlan_name: String,
    pub vpc_id: String,
    pub vni: u32,
    pub generation: Option<crate::generation::ResourceGeneration>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /v1/hypervisor/health (alias: /v1/node/health)
async fn health_handler(State(state): State<Arc<ForgeState>>) -> Json<HealthResponse> {
    let uptime = state.started_at.elapsed().as_secs();
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime,
    })
}

/// GET /v1/hypervisor/status (alias: /v1/node/status)
async fn status_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let vm_count = match state.vm_manager.as_ref() {
        Some(m) => m.list().await.len(),
        None => 0,
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "healthy",
            "uptime": uptime,
            "vm_count": vm_count,
        })),
    )
}

/// GET /v1/hypervisor/capacity (alias: /v1/node/capacity)
async fn capacity_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    if let Some(ref cap) = state.capacity {
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "allocatable_vcpus": cap.allocatable_vcpus(),
                "allocatable_memory_mb": cap.allocatable_memory_mb(),
                "available_vcpus": cap.available_vcpus(),
                "available_memory_mb": cap.available_memory_mb(),
                "used_vcpus": cap.allocatable_vcpus().saturating_sub(cap.available_vcpus()),
                "used_memory_mb": cap.allocatable_memory_mb().saturating_sub(cap.available_memory_mb()),
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_CAPACITY_UNAVAILABLE", "message": "capacity tracker not initialized"}),
            ),
        )
    }
}

/// GET /v1/hypervisor/metrics (alias: /v1/node/metrics)
async fn metrics_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let vm_count = match state.vm_manager.as_ref() {
        Some(m) => m.list().await.len(),
        None => 0,
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "uptime_seconds": uptime,
            "vm_count": vm_count,
        })),
    )
}

/// GET /v1/hypervisor/resources (alias: /v1/node/resources)
async fn resources_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let vms = match state.vm_manager.as_ref() {
        Some(m) => m.list().await,
        None => vec![],
    };

    let vm_names: Vec<String> = vms.iter().map(|v| v.vm_id.0.clone()).collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "vm_count": vms.len(),
            "vms": vm_names,
        })),
    )
}

/// GET /v1/tasks/:id
async fn get_task_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref task_store) = state.task_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_TASK_STORE_UNAVAILABLE", "message": "task store not initialized"}),
            ),
        );
    };

    match task_store.get_task(&id) {
        Ok(Some(task)) => (StatusCode::OK, Json(serde_json::to_value(task).unwrap())),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::json!({"code": "FORGE_TASK_NOT_FOUND", "message": format!("task {} not found", id)}),
            ),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"code": "FORGE_INTERNAL_ERROR", "message": e.to_string()})),
        ),
    }
}

/// GET /v1/tasks?resource_id=X
async fn list_tasks_handler(
    State(state): State<Arc<ForgeState>>,
    Query(query): Query<TaskListQuery>,
) -> impl IntoResponse {
    let Some(ref task_store) = state.task_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_TASK_STORE_UNAVAILABLE", "message": "task store not initialized"}),
            ),
        );
    };

    match task_store.list_tasks(query.resource_id.as_deref()) {
        Ok(tasks) => (StatusCode::OK, Json(serde_json::to_value(tasks).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"code": "FORGE_INTERNAL_ERROR", "message": e.to_string()})),
        ),
    }
}

/// POST /v1/instances — create a new VM instance.
async fn create_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateInstanceRequest>,
) -> impl IntoResponse {
    // Admission control: check capacity before anything else.
    if let Some(ref capacity) = state.capacity {
        if !capacity.can_admit(req.vcpus, req.memory_mb as u64) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "code": "FORGE_INSUFFICIENT_CAPACITY",
                    "message": format!(
                        "insufficient capacity: requested {} vCPUs, {} MB memory",
                        req.vcpus, req.memory_mb
                    )
                })),
            );
        }
        capacity.reserve(&req.name, req.vcpus, req.memory_mb as u64);
    }

    let Some(ref vm_manager) = state.vm_manager else {
        // Release any reservation made above.
        if let Some(ref capacity) = state.capacity {
            capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
        }
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Create task record.
    let task_id = format!("task-{}", uuid::Uuid::new_v4());
    if let Some(ref task_store) = state.task_store {
        let _ = task_store.create_task(&task_id, &req.name, "create_instance");
        let _ = task_store.start_task(&task_id);
    }

    // Resolve subnet if specified.
    let subnet_info = if let Some(ref subnet_name) = req.subnet {
        if let Some(ref org_store) = state.org_store {
            match org_store.find_subnets_by_name(subnet_name) {
                Ok(matches) if !matches.is_empty() => {
                    let (_vpc_name, subnet) = &matches[0];
                    Some(syfrah_compute::types::SubnetInfo {
                        name: subnet.name.clone(),
                        cidr: subnet.cidr.clone(),
                        gateway: subnet.gateway.clone(),
                        vpc_id: subnet.vpc_id.0.clone(),
                        env_id: subnet.env_id.0.clone(),
                    })
                }
                Ok(_) => {
                    // Release capacity reservation on error.
                    if let Some(ref capacity) = state.capacity {
                        capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
                    }
                    if let Some(ref task_store) = state.task_store {
                        let _ = task_store.fail_task(&task_id, "subnet not found");
                    }
                    return (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "code": "FORGE_SUBNET_NOT_FOUND",
                            "message": format!("subnet '{}' not found", subnet_name),
                            "task_id": task_id,
                        })),
                    );
                }
                Err(e) => {
                    if let Some(ref capacity) = state.capacity {
                        capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
                    }
                    if let Some(ref task_store) = state.task_store {
                        let _ = task_store.fail_task(&task_id, &e.to_string());
                    }
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "code": "FORGE_SUBNET_RESOLVE_FAILED",
                            "message": e.to_string(),
                            "task_id": task_id,
                        })),
                    );
                }
            }
        } else {
            warn!("forge: subnet requested but org store not available");
            None
        }
    } else {
        None
    };

    let spec = syfrah_compute::VmSpec {
        id: syfrah_compute::VmId(req.name.clone()),
        vcpus: req.vcpus,
        memory_mb: req.memory_mb,
        image: req.image,
        kernel: None,
        network: None,
        volumes: vec![],
        gpu: syfrah_compute::GpuMode::None,
        ssh_key: req.ssh_key,
        disk_size_mb: req.disk_size_mb,
        subnet: subnet_info,
        security_groups: if req.security_groups.is_empty() {
            vec!["default".to_string()]
        } else {
            req.security_groups
        },
    };

    match vm_manager.create_vm(spec).await {
        Ok(status) => {
            if let Some(ref capacity) = state.capacity {
                capacity.commit(&req.name, req.vcpus, req.memory_mb as u64);
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.complete_task(&task_id);
            }
            (
                StatusCode::CREATED,
                Json(serde_json::to_value(status).unwrap()),
            )
        }
        Err(e) => {
            if let Some(ref capacity) = state.capacity {
                capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.fail_task(&task_id, &e.to_string());
            }
            warn!("forge: create instance failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_CREATE_FAILED",
                    "message": e.to_string(),
                    "task_id": task_id,
                })),
            )
        }
    }
}

/// GET /v1/instances — list all instances.
async fn list_instances_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    let vms = vm_manager.list().await;
    (StatusCode::OK, Json(serde_json::to_value(vms).unwrap()))
}

/// GET /v1/instances/:id — get a single instance.
async fn get_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    match vm_manager.info(&id).await {
        Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "FORGE_INSTANCE_NOT_FOUND",
                "message": format!("instance '{}' not found: {}", id, e),
            })),
        ),
    }
}

/// DELETE /v1/instances/:id — delete an instance with reverse dependency cleanup.
///
/// Orchestration order (reverse of create):
///   stop VM -> announce FDB remove -> release IP (IPAM) -> delete NIC record
///   -> delete TAP/veth -> remove nftables rules -> remove FDB entries
///   -> if bridge empty: remove gateway IP, NAT, VXLAN, bridge
///
/// Best-effort: errors in cleanup steps are logged but do not fail the delete.
/// The VmManager handles the full reverse-dependency flow internally.
async fn delete_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Create task record.
    let task_id = format!("task-{}", uuid::Uuid::new_v4());
    if let Some(ref task_store) = state.task_store {
        let _ = task_store.create_task(&task_id, &id, "delete_instance");
        let _ = task_store.start_task(&task_id);
    }

    info!(instance = %id, task = %task_id, "starting delete orchestration");

    // Get VM info first for capacity release.
    let vm_info = vm_manager.info(&id).await.ok();

    // VmManager::delete_vm handles the full reverse-dependency cleanup:
    // stop -> FDB remove -> IPAM release -> NIC delete -> TAP delete
    // -> nftables remove -> bridge cleanup (if empty)
    // All cleanup steps are best-effort — errors are logged, not propagated.
    match vm_manager.delete_vm(&id).await {
        Ok(()) => {
            // Release capacity tracking.
            if let (Some(ref capacity), Some(ref info)) = (&state.capacity, &vm_info) {
                capacity.release(&id, info.vcpus, info.memory_mb as u64);
                info!(
                    instance = %id,
                    vcpus = info.vcpus,
                    memory_mb = info.memory_mb,
                    "capacity released"
                );
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.complete_task(&task_id);
            }
            info!(instance = %id, task = %task_id, "delete orchestration complete");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "code": "FORGE_INSTANCE_DELETED",
                    "message": format!("instance '{}' deleted", id),
                    "task_id": task_id,
                })),
            )
        }
        Err(e) => {
            // Even on error, attempt capacity release (best-effort).
            if let (Some(ref capacity), Some(ref info)) = (&state.capacity, &vm_info) {
                capacity.release(&id, info.vcpus, info.memory_mb as u64);
                warn!(
                    instance = %id,
                    "released capacity despite delete error (best-effort cleanup)"
                );
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.fail_task(&task_id, &e.to_string());
            }
            warn!(instance = %id, error = %e, "delete orchestration failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_DELETE_FAILED",
                    "message": e.to_string(),
                    "task_id": task_id,
                })),
            )
        }
    }
}

/// POST /v1/instances/:id/start
async fn start_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    match vm_manager.start_vm(&id).await {
        Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_START_FAILED",
                "message": e.to_string(),
            })),
        ),
    }
}

/// POST /v1/instances/:id/stop
async fn stop_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    match vm_manager.shutdown_vm(&id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "code": "FORGE_INSTANCE_STOPPED",
                "message": format!("instance '{}' stop initiated", id),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_STOP_FAILED",
                "message": e.to_string(),
            })),
        ),
    }
}

/// POST /v1/instances/:id/reboot
async fn reboot_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Reboot = shutdown then start.
    if let Err(e) = vm_manager.shutdown_vm(&id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_REBOOT_FAILED",
                "message": format!("shutdown phase failed: {}", e),
            })),
        );
    }

    // Wait briefly for shutdown.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    match vm_manager.start_vm(&id).await {
        Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_REBOOT_FAILED",
                "message": format!("start phase failed: {}", e),
            })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Bridge / VXLAN handlers
// ---------------------------------------------------------------------------

/// POST /v1/networks/bridges — ensure a bridge exists for a VPC.
async fn create_bridge_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateBridgeRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let bridge_name = syfrah_overlay::naming::bridge_name(&req.vpc_id);
    let resource_id = format!("br-{}", req.vpc_id);

    if let Err(e) = backend.create_bridge(&bridge_name).await {
        warn!(vpc_id = %req.vpc_id, error = %e, "failed to create bridge");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_BRIDGE_CREATE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    let gen = state
        .generation_tracker
        .as_ref()
        .map(|gt| gt.register(&resource_id));

    info!(vpc_id = %req.vpc_id, bridge = %bridge_name, "bridge ensured");
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(BridgeResponse {
                id: resource_id,
                bridge_name,
                vpc_id: req.vpc_id,
                generation: gen,
            })
            .unwrap(),
        ),
    )
}

/// DELETE /v1/networks/bridges/:id — remove a bridge.
async fn delete_bridge_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    // The id is the bridge kernel name (e.g. syfb-XXXXXXXX).
    if let Err(e) = backend.delete_bridge(&id).await {
        warn!(bridge = %id, error = %e, "failed to delete bridge");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_BRIDGE_DELETE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    if let Some(ref gt) = state.generation_tracker {
        gt.remove(&id);
    }

    info!(bridge = %id, "bridge deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_BRIDGE_DELETED",
            "message": format!("bridge '{}' deleted", id)
        })),
    )
}

/// POST /v1/networks/vxlans — ensure a VXLAN exists for a VPC.
async fn create_vxlan_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateVxlanRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let vxlan_name = syfrah_overlay::naming::vxlan_name(&req.vpc_id);
    let resource_id = format!("vx-{}", req.vpc_id);

    if let Err(e) = backend
        .create_vxlan(&vxlan_name, req.vni, &req.local_ip, req.port)
        .await
    {
        warn!(vpc_id = %req.vpc_id, error = %e, "failed to create VXLAN");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_VXLAN_CREATE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    // Attach VXLAN to its bridge.
    let bridge_name = syfrah_overlay::naming::bridge_name(&req.vpc_id);
    if let Err(e) = backend.attach_to_bridge(&vxlan_name, &bridge_name).await {
        warn!(
            vpc_id = %req.vpc_id,
            vxlan = %vxlan_name,
            bridge = %bridge_name,
            error = %e,
            "failed to attach VXLAN to bridge (bridge may not exist yet)"
        );
        // Non-fatal: the bridge may be created separately.
    }

    let gen = state
        .generation_tracker
        .as_ref()
        .map(|gt| gt.register(&resource_id));

    info!(vpc_id = %req.vpc_id, vxlan = %vxlan_name, vni = req.vni, "VXLAN ensured");
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(VxlanResponse {
                id: resource_id,
                vxlan_name,
                vpc_id: req.vpc_id,
                vni: req.vni,
                generation: gen,
            })
            .unwrap(),
        ),
    )
}

/// DELETE /v1/networks/vxlans/:id — remove a VXLAN.
async fn delete_vxlan_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    if let Err(e) = backend.delete_vxlan(&id).await {
        warn!(vxlan = %id, error = %e, "failed to delete VXLAN");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_VXLAN_DELETE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    if let Some(ref gt) = state.generation_tracker {
        gt.remove(&id);
    }

    info!(vxlan = %id, "VXLAN deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_VXLAN_DELETED",
            "message": format!("VXLAN '{}' deleted", id)
        })),
    )
}

/// Build the Forge HTTP router with all routes.
pub fn forge_router(state: Arc<ForgeState>) -> Router {
    Router::new()
        // -- Hypervisor endpoints (canonical) --
        .route("/v1/hypervisor/health", get(health_handler))
        .route("/v1/hypervisor/status", get(status_handler))
        .route("/v1/hypervisor/capacity", get(capacity_handler))
        .route("/v1/hypervisor/metrics", get(metrics_handler))
        .route("/v1/hypervisor/resources", get(resources_handler))
        // -- Deprecated /v1/node/* aliases --
        .route("/v1/node/health", get(health_handler))
        .route("/v1/node/status", get(status_handler))
        .route("/v1/node/capacity", get(capacity_handler))
        .route("/v1/node/metrics", get(metrics_handler))
        .route("/v1/node/resources", get(resources_handler))
        // -- Networks: bridges --
        .route("/v1/networks/bridges", post(create_bridge_handler))
        .route("/v1/networks/bridges/{id}", delete(delete_bridge_handler))
        // -- Networks: VXLANs --
        .route("/v1/networks/vxlans", post(create_vxlan_handler))
        .route("/v1/networks/vxlans/{id}", delete(delete_vxlan_handler))
        // -- Tasks --
        .route("/v1/tasks", get(list_tasks_handler))
        .route("/v1/tasks/{id}", get(get_task_handler))
        // -- Instances --
        .route(
            "/v1/instances",
            get(list_instances_handler).post(create_instance_handler),
        )
        .route(
            "/v1/instances/{id}",
            get(get_instance_handler).delete(delete_instance_handler),
        )
        .route("/v1/instances/{id}/start", post(start_instance_handler))
        .route("/v1/instances/{id}/stop", post(stop_instance_handler))
        .route("/v1/instances/{id}/reboot", post(reboot_instance_handler))
        .with_state(state)
}

/// The Forge HTTP server. Binds to the fabric IPv6 address on port 7100.
pub struct ForgeServer;

impl ForgeServer {
    /// Start the Forge HTTP server.
    pub async fn serve(
        bind_addr: SocketAddr,
        state: Arc<ForgeState>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let app = forge_router(state);
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        info!("Forge HTTP API listening on {}", bind_addr);

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                loop {
                    if shutdown_rx.changed().await.is_err() {
                        break;
                    }
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
            })
            .await?;

        info!("Forge HTTP API shut down");
        Ok(())
    }
}

/// Empty handler for the control socket — keeps backward compatibility.
pub struct ForgeHandler;

#[async_trait::async_trait]
impl LayerHandler for ForgeHandler {
    async fn handle(&self, _request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "error": "not implemented",
            "layer": "forge"
        }))
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state() -> Arc<ForgeState> {
        Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
            capacity: None,
            org_store: None,
            network_backend: None,
            generation_tracker: None,
        })
    }

    fn test_state_with_tasks() -> (tempfile::TempDir, Arc<ForgeState>) {
        let dir = tempfile::tempdir().unwrap();
        let db = syfrah_state::LayerDb::open_at(&dir.path().join("tasks.redb")).unwrap();
        let store = Arc::new(TaskStore::new(db));
        store.create_task("t-1", "vm-1", "create").unwrap();
        store.create_task("t-2", "vm-2", "create").unwrap();
        store.create_task("t-3", "vm-1", "delete").unwrap();

        let state = Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: Some(store),
            vm_manager: None,
            capacity: None,
            org_store: None,
            network_backend: None,
            generation_tracker: None,
        });
        (dir, state)
    }

    #[tokio::test]
    async fn handler_returns_not_implemented() {
        let handler = ForgeHandler;
        let resp = handler.handle(vec![], None).await;
        let body: serde_json::Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(body["error"], "not implemented");
        assert_eq!(body["layer"], "forge");
    }

    #[tokio::test]
    async fn health_endpoint_returns_healthy() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/node/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.status, "healthy");
    }

    #[tokio::test]
    async fn get_task_returns_task() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks/t-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let task: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(task["id"], "t-1");
    }

    #[tokio::test]
    async fn get_task_not_found() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn list_all_tasks() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tasks: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[tokio::test]
    async fn list_tasks_filtered() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks?resource_id=vm-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tasks: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn instances_unavailable_without_compute() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/instances")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err["code"], "FORGE_COMPUTE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn create_instance_requires_compute() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"test","image":"alpine","vcpus":1,"memory_mb":512}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn delete_instance_requires_compute() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/instances/test-vm")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn tasks_unavailable_without_store() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_with_subnet_requires_org_store() {
        // When subnet is specified but org_store is None, the handler should
        // still attempt creation (subnet_info will be None, network setup
        // will be skipped by VmManager).
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"test","image":"alpine","subnet":"frontend"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // 503 because vm_manager is None, not because of subnet
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_orchestration_task_tracking() {
        // Verify that create sets up task tracking even when compute is unavailable
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"orch-test","image":"alpine","vcpus":1,"memory_mb":512}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // 503 because vm_manager is None
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_with_capacity_admission() {
        // Verify admission control rejects when capacity is insufficient
        let cap = Arc::new(CapacityTracker::with_capacity(1, 256));
        let state = Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
            capacity: Some(cap),
            org_store: None,
            network_backend: None,
            generation_tracker: None,
        });
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"big-vm","image":"alpine","vcpus":4,"memory_mb":8192}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 409);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err["code"], "FORGE_INSUFFICIENT_CAPACITY");
    }

    fn test_state_with_network() -> Arc<ForgeState> {
        let backend: Arc<dyn syfrah_overlay::NetworkBackend> =
            Arc::new(syfrah_overlay::MockBackend::new());
        let gen_tracker = Arc::new(crate::generation::GenerationTracker::new());
        Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
            capacity: None,
            org_store: None,
            network_backend: Some(backend),
            generation_tracker: Some(gen_tracker),
        })
    }

    #[tokio::test]
    async fn create_bridge_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-prod"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["vpc_id"], "vpc-prod");
        assert!(result["bridge_name"].as_str().unwrap().starts_with("syfb-"));
        assert!(result["generation"].is_object());
    }

    #[tokio::test]
    async fn delete_bridge_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/networks/bridges/syfb-12345678")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["code"], "FORGE_BRIDGE_DELETED");
    }

    #[tokio::test]
    async fn create_vxlan_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/vxlans")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vpc_id":"vpc-prod","vni":100,"local_ip":"10.0.0.1"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["vpc_id"], "vpc-prod");
        assert_eq!(result["vni"], 100);
        assert!(result["vxlan_name"].as_str().unwrap().starts_with("syfx-"));
    }

    #[tokio::test]
    async fn idempotent_create() {
        // Creating the same bridge twice should succeed (idempotent).
        let state = test_state_with_network();

        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-test"}"#))
            .unwrap();
        let resp1 = app1.oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), 200);

        let app2 = forge_router(state);
        let req2 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-test"}"#))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), 200);
    }

    #[tokio::test]
    async fn bridge_unavailable_without_network() {
        let state = test_state(); // no network backend
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-test"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }
}
