use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::capacity::CapacityTracker;
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /v1/node/health
async fn health_handler(State(state): State<Arc<ForgeState>>) -> Json<HealthResponse> {
    let uptime = state.started_at.elapsed().as_secs();
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime,
    })
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
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Admission control: check capacity.
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

    // Create task record.
    let task_id = format!("task-{}", uuid::Uuid::new_v4());
    if let Some(ref task_store) = state.task_store {
        let _ = task_store.create_task(&task_id, &req.name, "create_instance");
        let _ = task_store.start_task(&task_id);
    }

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
        subnet: None,
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

/// DELETE /v1/instances/:id — delete an instance.
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

    // Get VM info first for capacity release.
    let vm_info = vm_manager.info(&id).await.ok();

    match vm_manager.delete_vm(&id).await {
        Ok(()) => {
            // Release capacity.
            if let (Some(ref capacity), Some(ref info)) = (&state.capacity, &vm_info) {
                capacity.release(&id, info.vcpus, info.memory_mb as u64);
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.complete_task(&task_id);
            }
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
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.fail_task(&task_id, &e.to_string());
            }
            warn!("forge: delete instance failed: {e}");
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

/// Build the Forge HTTP router with all routes.
pub fn forge_router(state: Arc<ForgeState>) -> Router {
    Router::new()
        .route("/v1/node/health", get(health_handler))
        .route("/v1/tasks", get(list_tasks_handler))
        .route("/v1/tasks/{id}", get(get_task_handler))
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
}
