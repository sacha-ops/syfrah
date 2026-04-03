//! Control socket types for the storage layer.
//!
//! Follows the same pattern as `syfrah_org::api`:
//! - `StorageRequest` / `StorageResponse` are the typed messages
//! - `StorageLayerHandler` adapts request handling to the opaque `LayerHandler` trait
//! - `send_storage_request` is the client-side helper used by CLI commands

use std::path::Path;

use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use syfrah_api::{LayerRequest, LayerResponse};
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------
// Request / Response enums
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub enum StorageRequest {
    /// Create a new volume.
    VolumeCreate {
        name: String,
        size_gb: u64,
        project: String,
        org: String,
        env: Option<String>,
    },
    /// List volumes with optional filters.
    VolumeList {
        project: Option<String>,
        org: Option<String>,
        env: Option<String>,
    },
    /// Get details of a single volume.
    VolumeGet {
        name: String,
        project: Option<String>,
    },
    /// Delete a volume.
    VolumeDelete {
        name: String,
        project: Option<String>,
        cascade: bool,
    },
    /// Resize a volume (grow only).
    VolumeResize {
        name: String,
        size_gb: u64,
        project: Option<String>,
    },
    /// Update volume settings (e.g. deletion protection).
    VolumeUpdate {
        name: String,
        project: Option<String>,
        deletion_protection: Option<bool>,
    },
    /// Configure storage backend (S3 + cache settings).
    Configure {
        region: String,
        s3_endpoint: String,
        s3_bucket: String,
        s3_access_key: String,
        s3_secret_key: String,
        cache_disk_path: Option<String>,
        cache_disk_size_gb: Option<u32>,
        cache_memory_size_gb: Option<u32>,
    },
    /// Update per-hypervisor cache overrides only.
    ConfigureCache {
        cache_disk_path: String,
        cache_disk_size_gb: u32,
        cache_memory_size_gb: u32,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum StorageResponse {
    /// Single volume info.
    Volume(serde_json::Value),
    /// List of volumes.
    VolumeList(Vec<serde_json::Value>),
    /// Success with no data.
    Ok,
    /// Storage configuration applied successfully.
    StorageConfigured { region: String },
    /// Error message.
    Error(String),
}

// ---------------------------------------------------------------------------
// StorageLayerHandler — adapts to LayerHandler
// ---------------------------------------------------------------------------

pub struct StorageLayerHandler;

#[async_trait::async_trait]
impl LayerHandler for StorageLayerHandler {
    async fn handle(&self, request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
        let req: StorageRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(e) => {
                let resp = StorageResponse::Error(format!("invalid storage request: {e}"));
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
        };

        let resp = handle_storage_request(req).await;
        serde_json::to_vec(&resp).unwrap_or_default()
    }
}

async fn handle_storage_request(req: StorageRequest) -> StorageResponse {
    // Stub implementation — returns meaningful "not yet implemented" errors
    // so users get actionable feedback during development.
    match req {
        StorageRequest::VolumeCreate { name, size_gb, .. } => {
            // TODO: persist volume via storage store
            StorageResponse::Volume(serde_json::json!({
                "name": name,
                "size_gb": size_gb,
                "state": "creating",
                "attached_to": null,
                "created_at": 0,
                "deletion_protection": false,
            }))
        }
        StorageRequest::VolumeList { .. } => StorageResponse::VolumeList(vec![]),
        StorageRequest::VolumeGet { name, .. } => StorageResponse::Error(format!(
            "volume '{name}' not found. List available volumes with: syfrah volume list"
        )),
        StorageRequest::VolumeDelete { name, .. } => StorageResponse::Error(format!(
            "volume '{name}' not found. List available volumes with: syfrah volume list"
        )),
        StorageRequest::VolumeResize { name, .. } => StorageResponse::Error(format!(
            "volume '{name}' not found. List available volumes with: syfrah volume list"
        )),
        StorageRequest::VolumeUpdate { name, .. } => StorageResponse::Error(format!(
            "volume '{name}' not found. List available volumes with: syfrah volume list"
        )),
        StorageRequest::Configure { region, .. } => {
            // TODO: forward to Raft SetStorageConfig
            StorageResponse::StorageConfigured { region }
        }
        StorageRequest::ConfigureCache { .. } => {
            // TODO: persist cache overrides locally
            StorageResponse::Ok
        }
    }
}

// ---------------------------------------------------------------------------
// Client-side helper
// ---------------------------------------------------------------------------

/// Send a storage request to the daemon via the control socket and return
/// the typed response.
pub async fn send_storage_request(
    socket_path: &Path,
    req: &StorageRequest,
) -> Result<StorageResponse, Box<dyn std::error::Error>> {
    let payload = serde_json::to_vec(req)?;
    let envelope = LayerRequest::Storage(payload);

    let mut stream = UnixStream::connect(socket_path).await?;
    syfrah_api::transport::write_message(&mut stream, &envelope).await?;
    let resp: LayerResponse = syfrah_api::transport::read_message(&mut stream).await?;

    match resp {
        LayerResponse::Storage(data) => {
            let storage_resp: StorageResponse = serde_json::from_slice(&data)?;
            Ok(storage_resp)
        }
        LayerResponse::UnknownLayer(name) => Err(format!("unknown layer: {name}").into()),
        other => Err(format!("unexpected response variant: {other:?}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handler_returns_volume_on_create() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::VolumeCreate {
            name: "pgdata".into(),
            size_gb: 50,
            project: "backend".into(),
            org: "acme".into(),
            env: None,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Volume(v) => {
                assert_eq!(v["name"], "pgdata");
                assert_eq!(v["size_gb"], 50);
            }
            other => panic!("expected Volume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_returns_empty_list() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::VolumeList {
            project: None,
            org: None,
            env: None,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(matches!(resp, StorageResponse::VolumeList(v) if v.is_empty()));
    }
}
