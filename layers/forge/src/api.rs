use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use tokio::sync::watch;
use tracing::info;

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

/// Build the Forge HTTP router with all routes.
pub fn forge_router(state: Arc<ForgeState>) -> Router {
    Router::new()
        .route("/v1/node/health", get(health_handler))
        .route("/v1/tasks", get(list_tasks_handler))
        .route("/v1/tasks/{id}", get(get_task_handler))
        .with_state(state)
}

/// The Forge HTTP server. Binds to the fabric IPv6 address on port 7100.
pub struct ForgeServer;

impl ForgeServer {
    /// Start the Forge HTTP server.
    ///
    /// `bind_addr` should be the node's `syfrah0` IPv6 address.
    /// `shutdown_rx` signals the server to shut down gracefully.
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
        })
    }

    fn test_state_with_tasks() -> (tempfile::TempDir, Arc<ForgeState>) {
        let dir = tempfile::tempdir().unwrap();
        let db = syfrah_state::LayerDb::open_at(&dir.path().join("tasks.redb")).unwrap();
        let store = Arc::new(TaskStore::new(db));

        // Create some test tasks.
        store.create_task("t-1", "vm-1", "create").unwrap();
        store.create_task("t-2", "vm-2", "create").unwrap();
        store.create_task("t-3", "vm-1", "delete").unwrap();

        let state = Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: Some(store),
            vm_manager: None,
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
        assert_eq!(task["resource_id"], "vm-1");
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
    async fn list_tasks_filtered_by_resource() {
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
    async fn tasks_unavailable_without_store() {
        let state = test_state(); // No task store
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }
}
