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
    /// Attach a volume to a VM.
    VolumeAttach {
        name: String,
        vm: String,
        project: Option<String>,
    },
    /// Detach a volume from its VM.
    VolumeDetach {
        name: String,
        project: Option<String>,
        force: bool,
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
    /// Create a snapshot of a volume.
    SnapshotCreate { name: String, volume: String },
    /// List snapshots, optionally filtered by source volume.
    SnapshotList { volume: Option<String> },
    /// Get details of a single snapshot.
    SnapshotGet { name: String },
    /// Restore a snapshot into a new volume.
    SnapshotRestore { snapshot: String, name: String },
    /// Delete a snapshot.
    SnapshotDelete { name: String },
    /// Run storage health check (S3 probe + cache info).
    Health,
    /// Get storage status (connectivity + cache utilization).
    Status,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum StorageResponse {
    /// Single volume info.
    Volume(serde_json::Value),
    /// List of volumes.
    VolumeList(Vec<serde_json::Value>),
    /// Single snapshot info.
    Snapshot(serde_json::Value),
    /// List of snapshots.
    SnapshotList(Vec<serde_json::Value>),
    /// Success with no data.
    Ok,
    /// Storage configuration applied successfully.
    StorageConfigured { region: String },
    /// Storage health check results.
    Health(StorageHealthReport),
    /// Storage status results.
    Status(StorageStatusReport),
    /// Error message.
    Error(String),
}

/// Results of a storage health probe.
#[derive(Debug, Serialize, Deserialize)]
pub struct StorageHealthReport {
    /// S3 endpoint URL (never contains credentials).
    pub s3_endpoint: String,
    /// S3 bucket name.
    pub s3_bucket: String,
    /// Whether the S3 endpoint is reachable.
    pub s3_reachable: bool,
    /// Whether the bucket is accessible (PUT/GET/DELETE succeeded).
    pub bucket_accessible: bool,
    /// PUT latency in milliseconds, if the probe succeeded.
    pub put_latency_ms: Option<u64>,
    /// GET latency in milliseconds, if the probe succeeded.
    pub get_latency_ms: Option<u64>,
    /// DELETE latency in milliseconds, if the probe succeeded.
    pub delete_latency_ms: Option<u64>,
    /// Error message from the S3 probe, if any.
    pub s3_error: Option<String>,
    /// Cache disk path.
    pub cache_disk_path: String,
    /// Cache disk total space in bytes.
    pub cache_disk_total_bytes: u64,
    /// Cache disk available space in bytes.
    pub cache_disk_available_bytes: u64,
    /// Cache memory allocation limit in bytes.
    pub cache_memory_limit_bytes: u64,
}

/// Results of a storage status query.
#[derive(Debug, Serialize, Deserialize)]
pub struct StorageStatusReport {
    /// S3 connectivity: true if the last health check passed.
    pub s3_connected: bool,
    /// S3 endpoint URL (never contains credentials).
    pub s3_endpoint: String,
    /// Per-volume cache utilization (placeholder until ZeroFS metrics in #1187).
    pub volume_cache_stats: Vec<VolumeCacheStat>,
    /// Total dirty bytes across all volumes (placeholder).
    pub total_dirty_bytes: u64,
}

/// Per-volume cache utilization (placeholder structure).
#[derive(Debug, Serialize, Deserialize)]
pub struct VolumeCacheStat {
    /// Volume name.
    pub name: String,
    /// Cached bytes for this volume.
    pub cached_bytes: u64,
    /// Dirty bytes pending writeback for this volume.
    pub dirty_bytes: u64,
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
        StorageRequest::VolumeAttach { name, vm, .. } => {
            // TODO: forward to Raft VolumeAttach
            StorageResponse::Volume(serde_json::json!({
                "name": name,
                "state": "attached",
                "attached_to": vm,
            }))
        }
        StorageRequest::VolumeDetach { name, .. } => {
            // TODO: forward to Raft VolumeDetach
            StorageResponse::Volume(serde_json::json!({
                "name": name,
                "state": "available",
                "attached_to": null,
            }))
        }
        StorageRequest::SnapshotCreate { name, volume } => {
            // TODO: persist snapshot via storage store
            StorageResponse::Snapshot(serde_json::json!({
                "name": name,
                "source_volume": volume,
                "state": "creating",
                "created_at": 0,
            }))
        }
        StorageRequest::SnapshotList { .. } => StorageResponse::SnapshotList(vec![]),
        StorageRequest::SnapshotGet { name } => StorageResponse::Error(format!(
            "snapshot '{name}' not found. List available snapshots with: syfrah volume snapshot list"
        )),
        StorageRequest::SnapshotRestore { snapshot, name } => {
            // TODO: forward to Raft SnapshotRestore
            StorageResponse::Volume(serde_json::json!({
                "name": name,
                "source_snapshot": snapshot,
                "state": "creating",
                "created_at": 0,
            }))
        }
        StorageRequest::SnapshotDelete { name } => {
            // TODO(#1202): forward to Raft DeleteSnapshot once storage store
            // integration is complete. The Raft handler will:
            //   1. Reject if a restore is in progress for this snapshot
            //   2. Decrement SST refcounts
            //   3. Mark SSTs with refcount=0 for GC (pending_gc_ssts)
            //   4. Recalculate minimum WAL retention position
            // For now, return Ok to match the CLI flow.
            let _ = name;
            StorageResponse::Ok
        }
        StorageRequest::Configure { region, .. } => {
            // TODO: forward to Raft SetStorageConfig
            StorageResponse::StorageConfigured { region }
        }
        StorageRequest::ConfigureCache { .. } => {
            // TODO: persist cache overrides locally
            StorageResponse::Ok
        }
        StorageRequest::Health => {
            // Stub: return a placeholder health report.
            // Real implementation reads StorageConfig from Raft and probes S3.
            StorageResponse::Health(StorageHealthReport {
                s3_endpoint: "(not configured)".into(),
                s3_bucket: "(not configured)".into(),
                s3_reachable: false,
                bucket_accessible: false,
                put_latency_ms: None,
                get_latency_ms: None,
                delete_latency_ms: None,
                s3_error: Some("storage config not yet loaded from Raft".into()),
                cache_disk_path: "/var/lib/syfrah/cache".into(),
                cache_disk_total_bytes: 0,
                cache_disk_available_bytes: 0,
                cache_memory_limit_bytes: 0,
            })
        }
        StorageRequest::Status => {
            // Stub: return placeholder status.
            // Real per-volume stats come in #1187 (ZeroFS metrics).
            StorageResponse::Status(StorageStatusReport {
                s3_connected: false,
                s3_endpoint: "(not configured)".into(),
                volume_cache_stats: vec![],
                total_dirty_bytes: 0,
            })
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
    async fn handler_returns_attached_volume() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::VolumeAttach {
            name: "pgdata".into(),
            vm: "web-1".into(),
            project: None,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Volume(v) => {
                assert_eq!(v["name"], "pgdata");
                assert_eq!(v["attached_to"], "web-1");
                assert_eq!(v["state"], "attached");
            }
            other => panic!("expected Volume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_returns_detached_volume() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::VolumeDetach {
            name: "pgdata".into(),
            project: None,
            force: false,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Volume(v) => {
                assert_eq!(v["name"], "pgdata");
                assert!(v["attached_to"].is_null());
                assert_eq!(v["state"], "available");
            }
            other => panic!("expected Volume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_returns_snapshot_on_create() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::SnapshotCreate {
            name: "daily-backup".into(),
            volume: "pgdata".into(),
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Snapshot(s) => {
                assert_eq!(s["name"], "daily-backup");
                assert_eq!(s["source_volume"], "pgdata");
                assert_eq!(s["state"], "creating");
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_returns_empty_snapshot_list() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::SnapshotList { volume: None };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(matches!(resp, StorageResponse::SnapshotList(v) if v.is_empty()));
    }

    #[tokio::test]
    async fn handler_returns_error_on_snapshot_get_not_found() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::SnapshotGet {
            name: "missing".into(),
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Error(msg) => {
                assert!(msg.contains("missing"));
                assert!(msg.contains("syfrah volume snapshot list"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_returns_volume_on_snapshot_restore() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::SnapshotRestore {
            snapshot: "daily-backup".into(),
            name: "pgdata-restored".into(),
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Volume(v) => {
                assert_eq!(v["name"], "pgdata-restored");
                assert_eq!(v["source_snapshot"], "daily-backup");
            }
            other => panic!("expected Volume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_returns_ok_on_snapshot_delete() {
        let handler = StorageLayerHandler;
        let req = StorageRequest::SnapshotDelete {
            name: "daily-backup".into(),
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(
            matches!(resp, StorageResponse::Ok),
            "expected Ok, got {resp:?}"
        );
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
