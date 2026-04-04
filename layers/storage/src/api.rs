//! Control socket types for the storage layer.
//!
//! Follows the same pattern as `syfrah_org::api`:
//! - `StorageRequest` / `StorageResponse` are the typed messages
//! - `StorageLayerHandler` adapts request handling to the opaque `LayerHandler` trait
//! - `send_storage_request` is the client-side helper used by CLI commands
//!
//! Mutations (VolumeCreate, VolumeDelete, Configure, etc.) are routed through
//! Raft when the control plane is active. Reads (VolumeList, VolumeGet) are
//! served from the local redb materialized view.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use syfrah_api::{LayerRequest, LayerResponse};
use syfrah_controlplane::commands::{StateMachineCommand, StateMachineResponse};
use syfrah_controlplane::RaftClient;
use syfrah_org::StorageStore;
use tokio::net::UnixStream;
use tokio::sync::RwLock;
use tracing::debug;

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
        /// Availability zone. When empty, `region` is used as fallback (#1281).
        #[serde(default)]
        zone: String,
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
    /// S3 PUT latency from the latest health probe (ms).
    #[serde(default)]
    pub s3_put_latency_ms: Option<u64>,
    /// S3 GET latency from the latest health probe (ms).
    #[serde(default)]
    pub s3_get_latency_ms: Option<u64>,
    /// Current S3 degradation level (Healthy, FsyncBlocking, EIO, Degraded, Error).
    #[serde(default)]
    pub s3_degradation_level: Option<String>,
    /// Duration of current S3 outage in seconds (0 if healthy).
    #[serde(default)]
    pub s3_outage_duration_secs: Option<u64>,
    /// Per-volume S3 health state (ADR-006 S25).
    pub volume_health: Vec<crate::volume_mgr::VolumeHealthReport>,
    /// Aggregated cache metrics for this node.
    #[serde(default)]
    pub cache_metrics: Option<crate::cache::CacheMetrics>,
    /// Active cache alerts (empty when healthy).
    #[serde(default)]
    pub cache_alerts: Vec<String>,
    /// Cache pre-warming progress for migrated volumes.
    #[serde(default)]
    pub warmup_progress: Vec<crate::cache::CachePrewarmProgress>,
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
// StorageLayerHandler -- adapts to LayerHandler
// ---------------------------------------------------------------------------

/// Storage layer handler that routes mutations through Raft when available
/// and reads from the local redb materialized view.
///
/// Architecture:
/// - Mutation requests (VolumeCreate, VolumeDelete, Configure, etc.) are
///   converted to `StateMachineCommand` and submitted to Raft.
/// - Read requests (VolumeList, VolumeGet) are served directly from local redb.
/// - Fallback: if Raft/store are not initialized, returns appropriate errors.
pub struct StorageLayerHandler {
    /// Optional Raft client -- set when controlplane is initialized.
    raft_client: RwLock<Option<RaftClient>>,
    /// Direct store access for reads and read-after-write.
    store: Option<Arc<StorageStore>>,
    /// Region name for this node (used in SetStorageConfig).
    region: String,
    /// Local node/hypervisor name (used as hypervisor_id in VolumeAttach).
    local_node_name: String,
}

impl StorageLayerHandler {
    /// Create a handler without backing store (stub mode for tests / early boot).
    pub fn new_stub() -> Self {
        Self {
            raft_client: RwLock::new(None),
            store: None,
            region: String::new(),
            local_node_name: String::new(),
        }
    }

    /// Create a handler backed by a redb storage store.
    pub fn new(store: Arc<StorageStore>, region: String, local_node_name: String) -> Self {
        Self {
            raft_client: RwLock::new(None),
            store: Some(store),
            region,
            local_node_name,
        }
    }

    /// Set the Raft client (called when controlplane is initialized).
    pub async fn set_raft_client(&self, client: RaftClient) {
        let mut guard = self.raft_client.write().await;
        *guard = Some(client);
    }
}

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

        let resp = self.handle_storage_request(req).await;
        serde_json::to_vec(&resp).unwrap_or_default()
    }
}

impl StorageLayerHandler {
    async fn handle_storage_request(&self, req: StorageRequest) -> StorageResponse {
        match req {
            // ----- Mutations: route through Raft when available -----
            StorageRequest::VolumeCreate {
                name,
                size_gb,
                project,
                org,
                env,
            } => {
                let env_id = match &env {
                    Some(e) => format!("{org}/{project}/{e}"),
                    None => format!("{org}/{project}/default"),
                };
                let volume_id = format!("vol-{}", short_id());
                // Auto-assign volume to the local hypervisor so the storage
                // reconciler starts ZeroFS immediately (single-node flow).
                let hypervisor_id = if self.local_node_name.is_empty() {
                    None
                } else {
                    Some(self.local_node_name.clone())
                };
                let cmd = StateMachineCommand::CreateVolume {
                    id: volume_id.clone(),
                    name: name.clone(),
                    size_gb: size_gb as u32,
                    org_id: org,
                    project_id: project,
                    env_id,
                    volume_type: syfrah_controlplane::commands::VolumeType::Data,
                    hypervisor_id,
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => {
                        // Read back from local store after Raft apply.
                        if let Some(ref store) = self.store {
                            if let Ok(Some(vol)) = store.get_volume(&volume_id) {
                                return StorageResponse::Volume(
                                    serde_json::to_value(&vol).unwrap_or_default(),
                                );
                            }
                        }
                        // Fallback: return minimal response.
                        StorageResponse::Volume(serde_json::json!({
                            "id": volume_id,
                            "name": name,
                            "size_gb": size_gb,
                            "state": "Creating",
                        }))
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::VolumeDelete {
                name,
                project,
                cascade,
            } => {
                let volume_id = match self.resolve_volume_id(&name, project.as_deref()) {
                    Ok(id) => id,
                    Err(e) => return StorageResponse::Error(e),
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let cmd = StateMachineCommand::DeleteVolume {
                    volume_id,
                    cascade,
                    deleted_at: now,
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => StorageResponse::Ok,
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::VolumeResize {
                name,
                size_gb,
                project,
            } => {
                let volume_id = match self.resolve_volume_id(&name, project.as_deref()) {
                    Ok(id) => id,
                    Err(e) => return StorageResponse::Error(e),
                };
                let cmd = StateMachineCommand::ResizeVolume {
                    volume_id: volume_id.clone(),
                    new_size_gb: size_gb as u32,
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => {
                        if let Some(ref store) = self.store {
                            if let Ok(Some(vol)) = store.get_volume(&volume_id) {
                                return StorageResponse::Volume(
                                    serde_json::to_value(&vol).unwrap_or_default(),
                                );
                            }
                        }
                        StorageResponse::Ok
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::VolumeUpdate { name, project, .. } => {
                // VolumeUpdate (deletion_protection, etc.) is not yet a Raft
                // command. Return an error until the state machine supports it.
                let _ = (name, project);
                StorageResponse::Error("volume update not yet supported via Raft".to_string())
            }

            StorageRequest::VolumeAttach { name, vm, project } => {
                let volume_id = match self.resolve_volume_id(&name, project.as_deref()) {
                    Ok(id) => id,
                    Err(e) => return StorageResponse::Error(e),
                };
                let cmd = StateMachineCommand::VolumeAttach {
                    volume_id: volume_id.clone(),
                    vm_id: vm,
                    hypervisor_id: self.local_node_name.clone(),
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => {
                        if let Some(ref store) = self.store {
                            if let Ok(Some(vol)) = store.get_volume(&volume_id) {
                                return StorageResponse::Volume(
                                    serde_json::to_value(&vol).unwrap_or_default(),
                                );
                            }
                        }
                        StorageResponse::Ok
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::VolumeDetach { name, project, .. } => {
                let volume_id = match self.resolve_volume_id(&name, project.as_deref()) {
                    Ok(id) => id,
                    Err(e) => return StorageResponse::Error(e),
                };
                let cmd = StateMachineCommand::VolumeDetach {
                    volume_id: volume_id.clone(),
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => {
                        if let Some(ref store) = self.store {
                            if let Ok(Some(vol)) = store.get_volume(&volume_id) {
                                return StorageResponse::Volume(
                                    serde_json::to_value(&vol).unwrap_or_default(),
                                );
                            }
                        }
                        StorageResponse::Ok
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::Configure {
                region,
                zone,
                s3_endpoint,
                s3_bucket,
                s3_access_key,
                s3_secret_key,
                cache_disk_path,
                cache_disk_size_gb,
                cache_memory_size_gb,
            } => {
                let config = syfrah_controlplane::commands::StorageConfig {
                    s3_endpoint,
                    s3_bucket,
                    s3_access_key,
                    s3_secret_key,
                    cache_disk_path: cache_disk_path
                        .unwrap_or_else(|| "/var/lib/syfrah/cache".to_string()),
                    cache_disk_size_gb: cache_disk_size_gb.unwrap_or(100),
                    cache_memory_size_gb: cache_memory_size_gb.unwrap_or(4),
                };
                let cmd = StateMachineCommand::SetStorageConfig {
                    region: region.clone(),
                    zone: zone.clone(),
                    config: Box::new(config),
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => StorageResponse::StorageConfigured { region },
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::ConfigureCache { .. } => {
                // Cache overrides are node-local, not replicated via Raft.
                StorageResponse::Ok
            }

            // Snapshot mutations -- forward through Raft.
            StorageRequest::SnapshotCreate { name, volume } => {
                let snapshot_id = format!("snap-{}", short_id());
                let volume_id = match self.resolve_volume_id(&volume, None) {
                    Ok(id) => id,
                    Err(e) => return StorageResponse::Error(e),
                };
                // Snapshot data is created by ZeroFS's native checkpoint:
                //   VolumeMgr::capture_manifest(volume_id, snapshot_name)
                // which runs `zerofs checkpoint create -c {config} {name}`.
                // The Raft command records the snapshot metadata; the actual
                // data lives in the ZeroFS checkpoint on S3.
                let cmd = StateMachineCommand::CreateSnapshot {
                    id: snapshot_id.clone(),
                    source_volume_id: volume_id,
                    sst_files: vec![],
                    wal_position: 0,
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => {
                        if let Some(ref store) = self.store {
                            if let Ok(Some(snap)) = store.get_snapshot(&snapshot_id) {
                                return StorageResponse::Snapshot(
                                    serde_json::to_value(&snap).unwrap_or_default(),
                                );
                            }
                        }
                        StorageResponse::Snapshot(serde_json::json!({
                            "id": snapshot_id,
                            "name": name,
                            "source_volume": volume,
                            "state": "creating",
                        }))
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::SnapshotDelete { name } => {
                let snapshot_id = match self.resolve_snapshot_id(&name) {
                    Ok(id) => id,
                    Err(e) => return StorageResponse::Error(e),
                };
                let cmd = StateMachineCommand::DeleteSnapshot { snapshot_id };
                match self.submit_raft(cmd).await {
                    Ok(_) => StorageResponse::Ok,
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::SnapshotRestore { snapshot, name } => {
                let new_volume_id = format!("vol-{}", short_id());
                let cmd = StateMachineCommand::RestoreSnapshot {
                    snapshot_id: snapshot,
                    new_volume_id: new_volume_id.clone(),
                    new_volume_name: name.clone(),
                };
                match self.submit_raft(cmd).await {
                    Ok(_) => {
                        if let Some(ref store) = self.store {
                            if let Ok(Some(vol)) = store.get_volume(&new_volume_id) {
                                return StorageResponse::Volume(
                                    serde_json::to_value(&vol).unwrap_or_default(),
                                );
                            }
                        }
                        StorageResponse::Volume(serde_json::json!({
                            "id": new_volume_id,
                            "name": name,
                            "state": "Creating",
                        }))
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            // ----- Reads: served from local redb store -----
            StorageRequest::VolumeList { project, org, env } => {
                if let Some(ref store) = self.store {
                    let volumes = if let (Some(o), Some(p), Some(e)) =
                        (org.as_deref(), project.as_deref(), env.as_deref())
                    {
                        let env_id = format!("{o}/{p}/{e}");
                        store.list_volumes_by_env(&env_id).unwrap_or_default()
                    } else {
                        store.list_volumes().unwrap_or_default()
                    };
                    let values: Vec<serde_json::Value> = volumes
                        .iter()
                        .filter(|v| {
                            if let Some(ref p) = project {
                                if v.project_id != *p {
                                    return false;
                                }
                            }
                            if let Some(ref o) = org {
                                if v.org_id != *o {
                                    return false;
                                }
                            }
                            true
                        })
                        .map(|v| serde_json::to_value(v).unwrap_or_default())
                        .collect();
                    StorageResponse::VolumeList(values)
                } else {
                    StorageResponse::VolumeList(vec![])
                }
            }

            StorageRequest::VolumeGet { name, project } => {
                match self.resolve_volume(&name, project.as_deref()) {
                    Ok(vol) => {
                        StorageResponse::Volume(serde_json::to_value(&vol).unwrap_or_default())
                    }
                    Err(e) => StorageResponse::Error(e),
                }
            }

            StorageRequest::SnapshotList { volume } => {
                if let Some(ref store) = self.store {
                    let snapshots = if let Some(ref vol_name) = volume {
                        match self.resolve_volume_id(vol_name, None) {
                            Ok(vol_id) => {
                                store.list_snapshots_by_volume(&vol_id).unwrap_or_default()
                            }
                            Err(_) => vec![],
                        }
                    } else {
                        store.list_snapshots().unwrap_or_default()
                    };
                    let values: Vec<serde_json::Value> = snapshots
                        .iter()
                        .map(|s| serde_json::to_value(s).unwrap_or_default())
                        .collect();
                    StorageResponse::SnapshotList(values)
                } else {
                    StorageResponse::SnapshotList(vec![])
                }
            }

            StorageRequest::SnapshotGet { name } => {
                if let Some(ref store) = self.store {
                    match store.get_snapshot(&name) {
                        Ok(Some(snap)) => StorageResponse::Snapshot(
                            serde_json::to_value(&snap).unwrap_or_default(),
                        ),
                        _ => StorageResponse::Error(format!(
                            "snapshot '{name}' not found. \
                             List available snapshots with: syfrah volume snapshot list"
                        )),
                    }
                } else {
                    StorageResponse::Error(format!(
                        "snapshot '{name}' not found. \
                         List available snapshots with: syfrah volume snapshot list"
                    ))
                }
            }

            // ----- Health: reads StorageConfig from store -----
            StorageRequest::Health => {
                let config = self.get_storage_config();
                match config {
                    Some(cfg) => {
                        // Return config-based report. The actual S3 probe runs in
                        // the background (start_s3_health_probe); here we surface
                        // configured values so the CLI can verify configuration.
                        StorageResponse::Health(StorageHealthReport {
                            s3_endpoint: cfg.s3_endpoint,
                            s3_bucket: cfg.s3_bucket,
                            s3_reachable: false,
                            bucket_accessible: false,
                            put_latency_ms: None,
                            get_latency_ms: None,
                            delete_latency_ms: None,
                            s3_error: None,
                            cache_disk_path: cfg.cache_disk_path,
                            cache_disk_total_bytes: 0,
                            cache_disk_available_bytes: 0,
                            cache_memory_limit_bytes: (cfg.cache_memory_size_gb as u64)
                                * 1024
                                * 1024
                                * 1024,
                        })
                    }
                    None => StorageResponse::Health(StorageHealthReport {
                        s3_endpoint: "(not configured)".into(),
                        s3_bucket: "(not configured)".into(),
                        s3_reachable: false,
                        bucket_accessible: false,
                        put_latency_ms: None,
                        get_latency_ms: None,
                        delete_latency_ms: None,
                        s3_error: Some(
                            "storage not configured -- run: syfrah storage configure".into(),
                        ),
                        cache_disk_path: "/var/lib/syfrah/cache".into(),
                        cache_disk_total_bytes: 0,
                        cache_disk_available_bytes: 0,
                        cache_memory_limit_bytes: 0,
                    }),
                }
            }

            StorageRequest::Status => {
                // Status still uses placeholders for per-volume stats (#1187).
                StorageResponse::Status(StorageStatusReport {
                    s3_connected: false,
                    s3_endpoint: self
                        .get_storage_config()
                        .map(|c| c.s3_endpoint)
                        .unwrap_or_else(|| "(not configured)".into()),
                    volume_cache_stats: vec![],
                    total_dirty_bytes: 0,
                    s3_put_latency_ms: None,
                    s3_get_latency_ms: None,
                    s3_degradation_level: None,
                    s3_outage_duration_secs: None,
                    volume_health: vec![],
                    cache_metrics: Some(crate::cache::CacheMetrics::default()),
                    cache_alerts: vec![],
                    warmup_progress: vec![],
                })
            }
        }
    }

    /// Submit a command to Raft. Returns an error if Raft is not available.
    async fn submit_raft(&self, cmd: StateMachineCommand) -> Result<StateMachineResponse, String> {
        let guard = self.raft_client.read().await;
        match guard.as_ref() {
            Some(client) => {
                debug!("storage: submitting {cmd} to raft");
                client
                    .write(cmd)
                    .await
                    .map_err(|e| format!("raft error: {e}"))
                    .and_then(|resp| match resp {
                        StateMachineResponse::Error(msg) => Err(msg),
                        other => Ok(other),
                    })
            }
            None => Err("raft not available -- cluster may not be initialized yet".to_string()),
        }
    }

    /// Resolve a volume name to its ID by scanning the store.
    ///
    /// When `project` is `Some`, only volumes in that project are considered.
    /// This prevents ambiguous resolution when multiple projects contain a
    /// volume with the same name.
    fn resolve_volume_id(&self, name: &str, project: Option<&str>) -> Result<String, String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| format!("volume '{name}' not found -- store not initialized"))?;
        let volumes = store
            .list_volumes()
            .map_err(|e| format!("failed to list volumes: {e}"))?;
        for vol in &volumes {
            if vol.name == name || vol.id == name {
                if let Some(p) = project {
                    if vol.project_id != p {
                        continue;
                    }
                }
                return Ok(vol.id.clone());
            }
        }
        Err(format!(
            "volume '{name}' not found. List available volumes with: syfrah volume list"
        ))
    }

    /// Resolve a volume by name, returning the full Volume record.
    ///
    /// When `project` is `Some`, only volumes in that project are considered.
    fn resolve_volume(
        &self,
        name: &str,
        project: Option<&str>,
    ) -> Result<syfrah_org::Volume, String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| format!("volume '{name}' not found -- store not initialized"))?;
        // Direct ID lookup first (project filter applied below if needed).
        if let Ok(Some(vol)) = store.get_volume(name) {
            if project.is_none() || project == Some(vol.project_id.as_str()) {
                return Ok(vol);
            }
        }
        let volumes = store
            .list_volumes()
            .map_err(|e| format!("failed to list volumes: {e}"))?;
        for vol in volumes {
            if vol.name == name {
                if let Some(p) = project {
                    if vol.project_id != p {
                        continue;
                    }
                }
                return Ok(vol);
            }
        }
        Err(format!(
            "volume '{name}' not found. List available volumes with: syfrah volume list"
        ))
    }

    /// Resolve a snapshot name/ID to its canonical ID by scanning the store.
    ///
    /// Snapshots in the data model only have an `id` field (no separate `name`),
    /// so we accept either a direct ID match or a prefix match for convenience.
    fn resolve_snapshot_id(&self, name: &str) -> Result<String, String> {
        // If it looks like a snapshot ID and the store has it, use it directly.
        if let Some(ref store) = self.store {
            if let Ok(Some(snap)) = store.get_snapshot(name) {
                return Ok(snap.id);
            }
            // Fallback: scan all snapshots for a prefix/exact match.
            let snapshots = store
                .list_snapshots()
                .map_err(|e| format!("failed to list snapshots: {e}"))?;
            for snap in &snapshots {
                if snap.id == name {
                    return Ok(snap.id.clone());
                }
            }
        }
        Err(format!(
            "snapshot '{name}' not found. List available snapshots with: syfrah volume snapshot list"
        ))
    }

    /// Read StorageConfig from the local store for this node's region.
    fn get_storage_config(&self) -> Option<syfrah_org::StorageConfig> {
        let store = self.store.as_ref()?;
        if !self.region.is_empty() {
            if let Ok(Some(cfg)) = store.get_storage_config(&self.region) {
                return Some(cfg);
            }
        }
        // Fallback: return the first available config.
        if let Ok(configs) = store.list_storage_configs() {
            if let Some((_region, cfg)) = configs.into_iter().next() {
                return Some(cfg);
            }
        }
        None
    }
}

/// Generate a short hex ID (12 chars) from OS-seeded random entropy.
///
/// Uses `RandomState` (seeded from OS randomness on construction) to produce
/// two independent 64-bit hashes, then takes 12 hex chars from the XOR.
/// This avoids adding external crates while providing collision resistance
/// far beyond the old deterministic LCG approach.
fn short_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    // Each RandomState is seeded from OS entropy (getrandom/urandom).
    let s1 = std::collections::hash_map::RandomState::new();
    let s2 = std::collections::hash_map::RandomState::new();
    let mut h1 = s1.build_hasher();
    let mut h2 = s2.build_hasher();
    // Feed unique per-call material to further separate calls within the same
    // thread (timestamp + stack address).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    h1.write_u128(now);
    h2.write_usize(&now as *const _ as usize);
    let combined = h1.finish() ^ h2.finish();
    format!("{:016x}", combined)[..12].to_string()
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

    /// In stub mode (no Raft, no store), mutations that require Raft return errors,
    /// and reads return empty results. This validates the fallback paths.

    #[tokio::test]
    async fn stub_handler_volume_create_returns_raft_error() {
        let handler = StorageLayerHandler::new_stub();
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
        assert!(
            matches!(resp, StorageResponse::Error(ref msg) if msg.contains("raft")),
            "expected raft error, got {resp:?}"
        );
    }

    #[tokio::test]
    async fn stub_handler_returns_empty_volume_list() {
        let handler = StorageLayerHandler::new_stub();
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

    #[tokio::test]
    async fn stub_handler_returns_empty_snapshot_list() {
        let handler = StorageLayerHandler::new_stub();
        let req = StorageRequest::SnapshotList { volume: None };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(matches!(resp, StorageResponse::SnapshotList(v) if v.is_empty()));
    }

    #[tokio::test]
    async fn stub_handler_returns_error_on_snapshot_get_not_found() {
        let handler = StorageLayerHandler::new_stub();
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
    async fn stub_handler_returns_status_with_cache_metrics() {
        let handler = StorageLayerHandler::new_stub();
        let req = StorageRequest::Status;
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Status(s) => {
                assert!(s.cache_metrics.is_some());
                let cm = s.cache_metrics.unwrap();
                assert_eq!(cm.cache_hit_rate, 100.0);
                assert!(s.cache_alerts.is_empty());
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stub_handler_health_returns_not_configured() {
        let handler = StorageLayerHandler::new_stub();
        let req = StorageRequest::Health;
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            StorageResponse::Health(h) => {
                assert_eq!(h.s3_endpoint, "(not configured)");
                assert!(h.s3_error.is_some());
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stub_handler_configure_returns_raft_error() {
        let handler = StorageLayerHandler::new_stub();
        let req = StorageRequest::Configure {
            region: "par1".into(),
            zone: "par1-a".into(),
            s3_endpoint: "https://s3.par.io.cloud.ovh.net".into(),
            s3_bucket: "syfrah-volumes".into(),
            s3_access_key: "key".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: None,
            cache_disk_size_gb: None,
            cache_memory_size_gb: None,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(
            matches!(resp, StorageResponse::Error(ref msg) if msg.contains("raft")),
            "expected raft error, got {resp:?}"
        );
    }

    #[tokio::test]
    async fn stub_handler_volume_get_returns_not_found() {
        let handler = StorageLayerHandler::new_stub();
        let req = StorageRequest::VolumeGet {
            name: "pgdata".into(),
            project: None,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(
            matches!(resp, StorageResponse::Error(ref msg) if msg.contains("not found")),
            "expected not found error, got {resp:?}"
        );
    }

    #[tokio::test]
    async fn stub_handler_cache_configure_returns_ok() {
        let handler = StorageLayerHandler::new_stub();
        let req = StorageRequest::ConfigureCache {
            cache_disk_path: "/mnt/ssd/cache".into(),
            cache_disk_size_gb: 200,
            cache_memory_size_gb: 8,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: StorageResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(
            matches!(resp, StorageResponse::Ok),
            "expected Ok, got {resp:?}"
        );
    }
}
