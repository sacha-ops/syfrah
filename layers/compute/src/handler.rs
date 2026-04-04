//! REST gateway for the compute layer.
//!
//! `ComputeHandler` wraps a `VmManager` and exposes 9 endpoints mirroring
//! the `ComputeService` RPCs defined in `compute.proto`:
//!
//! - `POST   /v1/compute/vms`              — CreateVm
//! - `GET    /v1/compute/vms`              — ListVms
//! - `GET    /v1/compute/vms/:id`          — GetVm
//! - `DELETE /v1/compute/vms/:id`          — DeleteVm
//! - `POST   /v1/compute/vms/:id/start`   — StartVm
//! - `POST   /v1/compute/vms/:id/stop`    — StopVm
//! - `POST   /v1/compute/vms/:id/reboot`  — RebootVm
//! - `POST   /v1/compute/vms/:id/resize`  — ResizeVm
//! - `GET    /v1/compute/status`           — GetStatus

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::config::validate_name;
use crate::error::ComputeError;
use crate::manager::VmManager;
use crate::types::{GpuMode, NetworkConfig, VmId, VmSpec, VmStatus, VolumeAttachment};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Shared `VmManager` reference for axum handlers.
type SharedManager = Arc<VmManager>;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateVmRequest {
    pub name: String,
    pub vcpus: u32,
    pub memory_mb: u32,
    pub image: String,
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub gpu: Option<GpuModeRequest>,
    #[serde(default)]
    pub volumes: Vec<VolumeAttachmentRequest>,
    #[serde(default)]
    pub network: Option<NetworkConfigRequest>,
}

#[derive(Debug, Deserialize)]
pub struct GpuModeRequest {
    #[serde(default)]
    pub none: bool,
    #[serde(default)]
    pub passthrough: Option<GpuPassthroughRequest>,
}

#[derive(Debug, Deserialize)]
pub struct GpuPassthroughRequest {
    pub bdf: String,
}

#[derive(Debug, Deserialize)]
pub struct VolumeAttachmentRequest {
    pub path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Deserialize)]
pub struct NetworkConfigRequest {
    pub tap_name: String,
    #[serde(default)]
    pub mac: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StopVmRequest {
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ResizeVmRequest {
    pub vcpus: Option<u32>,
    pub memory_mb: Option<u32>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct VmResponse {
    pub id: String,
    pub phase: String,
    pub vcpus: u32,
    pub memory_mb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vpc: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security_groups: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypervisor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListVmsResponse {
    pub vms: Vec<VmResponse>,
}

#[derive(Debug, Serialize)]
pub struct DeleteVmResponse {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct GetStatusResponse {
    pub status: String,
    pub total_vms: u32,
    pub running_vms: u32,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn vm_status_to_response(s: &VmStatus) -> VmResponse {
    VmResponse {
        id: s.vm_id.0.clone(),
        phase: format!("{:?}", s.phase),
        vcpus: s.vcpus,
        memory_mb: s.memory_mb,
        image: s.image.clone(),
        runtime: s.runtime.map(|r| r.to_string()),
        created_at: s.created_at,
        uptime_secs: s.uptime_secs,
        ip: s.ip.clone(),
        subnet: s.subnet.clone(),
        vpc: s.vpc.clone(),
        security_groups: s.security_groups.clone(),
        hypervisor_id: s.hypervisor_id.clone(),
        region: s.region.clone(),
        zone: s.zone.clone(),
    }
}

/// Map a `ComputeError` to an HTTP status code.
fn error_to_status(err: &ComputeError) -> StatusCode {
    match err {
        ComputeError::Config(_) => StatusCode::BAD_REQUEST,
        ComputeError::Preflight(_) => StatusCode::BAD_REQUEST,
        ComputeError::Transition(_) => StatusCode::CONFLICT,
        ComputeError::Concurrency(_) => StatusCode::CONFLICT,
        ComputeError::VmNotFound { .. } => StatusCode::NOT_FOUND,
        ComputeError::Process(_) => StatusCode::INTERNAL_SERVER_ERROR,
        ComputeError::Client(_) => StatusCode::INTERNAL_SERVER_ERROR,
        ComputeError::Image(_) => StatusCode::UNPROCESSABLE_ENTITY,
        ComputeError::NetworkSetup(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn error_response(err: ComputeError) -> (StatusCode, Json<ErrorResponse>) {
    let status = error_to_status(&err);
    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

/// Validate a path-parameter ID using the shared name rules.
fn validate_path_id(id: &str) -> Result<(), ComputeError> {
    validate_name(id, "VM").map_err(ComputeError::Config)?;
    Ok(())
}

fn parse_gpu_mode(req: Option<GpuModeRequest>) -> GpuMode {
    match req {
        None => GpuMode::None,
        Some(gm) => match gm.passthrough {
            Some(pt) => GpuMode::Passthrough { bdf: pt.bdf },
            None => GpuMode::None,
        },
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the axum [`Router`] for the compute REST API.
pub fn router(manager: Arc<VmManager>) -> Router {
    Router::new()
        .route("/v1/compute/vms", post(create_vm))
        .route("/v1/compute/vms", get(list_vms))
        .route("/v1/compute/vms/{id}", get(get_vm))
        .route("/v1/compute/vms/{id}", delete(delete_vm))
        .route("/v1/compute/vms/{id}/start", post(start_vm))
        .route("/v1/compute/vms/{id}/stop", post(stop_vm))
        .route("/v1/compute/vms/{id}/reboot", post(reboot_vm))
        .route("/v1/compute/vms/{id}/resize", post(resize_vm))
        .route("/v1/compute/status", get(get_status))
        .with_state(manager)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn create_vm(
    State(mgr): State<SharedManager>,
    Json(body): Json<CreateVmRequest>,
) -> impl IntoResponse {
    let spec = VmSpec {
        id: VmId(body.name.clone()),
        vcpus: body.vcpus,
        memory_mb: body.memory_mb,
        image: body.image,
        kernel: body.kernel,
        network: body.network.map(|n| NetworkConfig {
            tap_name: n.tap_name,
            mac: n.mac,
        }),
        volumes: body
            .volumes
            .into_iter()
            .map(|v| VolumeAttachment {
                path: v.path,
                read_only: v.read_only,
                volume_id: None,
                is_root: false,
            })
            .collect(),
        gpu: parse_gpu_mode(body.gpu),
        ssh_key: None,
        disk_size_mb: None,
        subnet: None,
        security_groups: vec![],
        pre_allocated_ip: None,
        pre_allocated_mac: None,
    };

    match mgr.create_vm(spec).await {
        Ok(status) => (StatusCode::CREATED, Json(vm_status_to_response(&status))).into_response(),
        Err(e) => {
            let (status, json) = error_response(e);
            (status, json).into_response()
        }
    }
}

async fn list_vms(State(mgr): State<SharedManager>) -> impl IntoResponse {
    let vms = mgr.list().await;
    let response = ListVmsResponse {
        vms: vms.iter().map(vm_status_to_response).collect(),
    };
    (StatusCode::OK, Json(response))
}

async fn get_vm(State(mgr): State<SharedManager>, Path(id): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_path_id(&id) {
        return error_response(e).into_response();
    }
    match mgr.info(&id).await {
        Ok(status) => (StatusCode::OK, Json(vm_status_to_response(&status))).into_response(),
        Err(e) => {
            let (status, json) = error_response(e);
            (status, json).into_response()
        }
    }
}

async fn delete_vm(State(mgr): State<SharedManager>, Path(id): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_path_id(&id) {
        return error_response(e).into_response();
    }
    match mgr.delete_vm(&id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(DeleteVmResponse {
                id,
                status: "deleted".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            let (status, json) = error_response(e);
            (status, json).into_response()
        }
    }
}

async fn start_vm(State(_mgr): State<SharedManager>, Path(id): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_path_id(&id) {
        return error_response(e).into_response();
    }
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "start is not yet implemented"})),
    )
        .into_response()
}

async fn stop_vm(State(mgr): State<SharedManager>, Path(id): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_path_id(&id) {
        return error_response(e).into_response();
    }
    match mgr.shutdown_vm(&id).await {
        Ok(()) => match mgr.info(&id).await {
            Ok(status) => (StatusCode::OK, Json(vm_status_to_response(&status))).into_response(),
            Err(_) => (
                StatusCode::OK,
                Json(VmResponse {
                    id,
                    phase: "Stopped".to_string(),
                    vcpus: 0,
                    memory_mb: 0,
                    image: None,
                    runtime: None,
                    created_at: None,
                    uptime_secs: None,
                    ip: None,
                    subnet: None,
                    vpc: None,
                    security_groups: vec![],
                    hypervisor_id: None,
                    region: None,
                    zone: None,
                }),
            )
                .into_response(),
        },
        Err(e) => {
            let (status, json) = error_response(e);
            (status, json).into_response()
        }
    }
}

async fn reboot_vm(State(_mgr): State<SharedManager>, Path(id): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_path_id(&id) {
        return error_response(e).into_response();
    }
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "reboot is not yet implemented"})),
    )
        .into_response()
}

async fn resize_vm(State(_mgr): State<SharedManager>, Path(id): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_path_id(&id) {
        return error_response(e).into_response();
    }
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "resize is not yet implemented"})),
    )
        .into_response()
}

async fn get_status(State(mgr): State<SharedManager>) -> impl IntoResponse {
    let vms = mgr.list().await;
    let total = vms.len() as u32;
    let running = vms
        .iter()
        .filter(|v| v.phase == crate::phase::VmPhase::Running)
        .count() as u32;

    (
        StatusCode::OK,
        Json(GetStatusResponse {
            status: "healthy".to_string(),
            total_vms: total,
            running_vms: running,
        }),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    use crate::manager::ComputeConfig;

    /// Build a test VmManager backed by a temp directory.
    fn make_test_manager(tmp: &std::path::Path) -> Arc<VmManager> {
        let config = ComputeConfig {
            base_dir: tmp.join("vms"),
            image_dir: tmp.join("images"),
            kernel_path: tmp.join("vmlinux"),
            ch_binary: Some(PathBuf::from("/bin/true")),
            monitor_interval_secs: 1,
            shutdown_timeout_secs: 5,
            instance_base: tmp.join("instances"),
            image_management: false,
            pull_policy: crate::image::types::PullPolicy::default(),
            catalog_url: crate::manager::DEFAULT_CATALOG_URL.to_string(),
            cache_path: tmp.join("cache").join("catalog.json"),
        };
        std::fs::create_dir_all(&config.base_dir).unwrap();
        std::fs::create_dir_all(&config.image_dir).unwrap();
        std::fs::create_dir_all(&config.instance_base).unwrap();
        Arc::new(VmManager::new(config).unwrap())
    }

    fn test_router(mgr: Arc<VmManager>) -> Router {
        router(mgr)
    }

    async fn send_request(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let body = match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        };
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body)
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    // -- GET /v1/compute/status -------------------------------------------------

    #[tokio::test]
    async fn status_returns_200_with_healthy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, body) = send_request(app, "GET", "/v1/compute/status", None).await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "healthy");
        assert_eq!(v["total_vms"], 0);
        assert_eq!(v["running_vms"], 0);
    }

    // -- GET /v1/compute/vms ----------------------------------------------------

    #[tokio::test]
    async fn list_vms_returns_200_with_empty_list() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, body) = send_request(app, "GET", "/v1/compute/vms", None).await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["vms"].as_array().unwrap().is_empty());
    }

    // -- GET /v1/compute/vms/:id -----------------------------------------------

    #[tokio::test]
    async fn get_vm_not_found_returns_404() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, body) = send_request(app, "GET", "/v1/compute/vms/nonexistent", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("not found"));
    }

    // -- DELETE /v1/compute/vms/:id --------------------------------------------

    #[tokio::test]
    async fn delete_vm_not_found_returns_404() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, body) = send_request(app, "DELETE", "/v1/compute/vms/nonexistent", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("not found"));
    }

    // -- POST /v1/compute/vms/:id/start ----------------------------------------

    #[tokio::test]
    async fn start_vm_not_implemented_returns_501() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, _) =
            send_request(app, "POST", "/v1/compute/vms/nonexistent/start", None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    }

    // -- POST /v1/compute/vms/:id/stop -----------------------------------------

    #[tokio::test]
    async fn stop_vm_not_found_returns_404() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, _) = send_request(app, "POST", "/v1/compute/vms/nonexistent/stop", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // -- POST /v1/compute/vms/:id/reboot ---------------------------------------

    #[tokio::test]
    async fn reboot_vm_not_implemented_returns_501() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, _) =
            send_request(app, "POST", "/v1/compute/vms/nonexistent/reboot", None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    }

    // -- POST /v1/compute/vms/:id/resize ---------------------------------------

    #[tokio::test]
    async fn resize_vm_not_implemented_returns_501() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, _) = send_request(
            app,
            "POST",
            "/v1/compute/vms/nonexistent/resize",
            Some(r#"{"vcpus": 4}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    }

    // -- POST /v1/compute/vms (create) — bad request ---------------------------

    #[tokio::test]
    async fn create_vm_missing_fields_returns_422() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        // Missing required fields
        let (status, _) =
            send_request(app, "POST", "/v1/compute/vms", Some(r#"{"name":"test"}"#)).await;
        // axum returns 422 for JSON deserialization failures
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn create_vm_invalid_json_returns_4xx() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (status, _) = send_request(app, "POST", "/v1/compute/vms", Some("not json")).await;
        // axum returns 400 for unparseable JSON bodies
        assert!(status.is_client_error());
    }

    // -- Response format validation -------------------------------------------

    #[tokio::test]
    async fn status_response_has_required_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (_, body) = send_request(app, "GET", "/v1/compute/status", None).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("status").is_some());
        assert!(v.get("total_vms").is_some());
        assert!(v.get("running_vms").is_some());
    }

    #[tokio::test]
    async fn list_response_has_vms_array() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (_, body) = send_request(app, "GET", "/v1/compute/vms", None).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["vms"].is_array());
    }

    // -- Error response format ------------------------------------------------

    #[tokio::test]
    async fn error_response_has_error_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = make_test_manager(tmp.path());
        let app = test_router(mgr);
        let (_, body) = send_request(app, "GET", "/v1/compute/vms/nonexistent", None).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("error").is_some());
        assert!(!v["error"].as_str().unwrap().is_empty());
    }

    // -- error_to_status unit tests -------------------------------------------

    #[test]
    fn error_to_status_config_is_bad_request() {
        let err = ComputeError::Config(crate::error::ConfigError::InvalidVcpuCount { value: 0 });
        assert_eq!(error_to_status(&err), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn error_to_status_preflight_is_bad_request() {
        let err = ComputeError::Preflight(crate::error::PreflightError::KvmNotAvailable);
        assert_eq!(error_to_status(&err), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn error_to_status_vm_not_found() {
        let err = ComputeError::VmNotFound {
            id: "vm-123".to_string(),
        };
        assert_eq!(error_to_status(&err), StatusCode::NOT_FOUND);
    }

    #[test]
    fn error_to_status_process_is_500() {
        let err = ComputeError::Process(crate::error::ProcessError::SpawnFailed {
            reason: "permission denied".to_string(),
        });
        assert_eq!(error_to_status(&err), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn error_to_status_client_is_500() {
        let err = ComputeError::Client(crate::error::ClientError::ConnectionRefused);
        assert_eq!(error_to_status(&err), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // -- vm_status_to_response ------------------------------------------------

    #[test]
    fn vm_status_to_response_maps_all_fields() {
        let status = VmStatus {
            vm_id: VmId("test-vm".to_string()),
            phase: crate::phase::VmPhase::Running,
            vcpus: 4,
            memory_mb: 8192,
            image: Some("ubuntu-24.04".to_string()),
            runtime: Some(crate::runtime_backend::RuntimeType::Vm),
            created_at: Some(1700000000),
            uptime_secs: Some(3600),
            ip: Some("10.1.1.3".to_string()),
            subnet: Some("frontend".to_string()),
            vpc: Some("default".to_string()),
            security_groups: vec!["web-sg".to_string()],
            hypervisor_id: Some("hv-1".to_string()),
            region: Some("eu-west".to_string()),
            zone: Some("az-1".to_string()),
        };
        let resp = vm_status_to_response(&status);
        assert_eq!(resp.id, "test-vm");
        assert_eq!(resp.phase, "Running");
        assert_eq!(resp.vcpus, 4);
        assert_eq!(resp.memory_mb, 8192);
        assert_eq!(resp.created_at, Some(1700000000));
        assert_eq!(resp.uptime_secs, Some(3600));
        assert_eq!(resp.ip.as_deref(), Some("10.1.1.3"));
        assert_eq!(resp.subnet.as_deref(), Some("frontend"));
        assert_eq!(resp.vpc.as_deref(), Some("default"));
    }

    #[test]
    fn vm_status_to_response_omits_none_fields() {
        let status = VmStatus {
            vm_id: VmId("test-vm".to_string()),
            phase: crate::phase::VmPhase::Pending,
            vcpus: 1,
            memory_mb: 512,
            image: None,
            runtime: None,
            created_at: None,
            uptime_secs: None,
            ip: None,
            subnet: None,
            vpc: None,
            security_groups: vec![],
            hypervisor_id: None,
            region: None,
            zone: None,
        };
        let resp = vm_status_to_response(&status);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("created_at"));
        assert!(!json.contains("uptime_secs"));
    }

    // -- parse_gpu_mode -------------------------------------------------------

    #[test]
    fn parse_gpu_mode_none_when_absent() {
        assert_eq!(parse_gpu_mode(None), GpuMode::None);
    }

    #[test]
    fn parse_gpu_mode_passthrough() {
        let req = Some(GpuModeRequest {
            none: false,
            passthrough: Some(GpuPassthroughRequest {
                bdf: "0000:01:00.0".to_string(),
            }),
        });
        let mode = parse_gpu_mode(req);
        assert!(matches!(mode, GpuMode::Passthrough { .. }));
    }
}
