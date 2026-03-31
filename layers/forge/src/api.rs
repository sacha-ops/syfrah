use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
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

/// Health check response.
#[derive(Serialize, Deserialize, Debug)]
pub struct HealthResponse {
    pub status: String,
    pub uptime: u64,
}

/// GET /v1/node/health
async fn health_handler(State(state): State<Arc<ForgeState>>) -> Json<HealthResponse> {
    let uptime = state.started_at.elapsed().as_secs();
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime,
    })
}

/// Build the Forge HTTP router with all routes.
pub fn forge_router(state: Arc<ForgeState>) -> Router {
    Router::new()
        .route("/v1/node/health", get(health_handler))
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
        let state = Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
        });
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
}
