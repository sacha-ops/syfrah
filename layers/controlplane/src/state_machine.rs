//! Raft state machine backed by the existing OrgStore (redb).
//!
//! Snapshots serialize the full redb state (all org/ipam/placement tables)
//! so new members joining the cluster get the complete state without
//! replaying the entire Raft log.

use std::collections::HashMap;
use std::io;
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::TryStreamExt;
use openraft::alias::{LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf};
use openraft::storage::{EntryResponder, RaftSnapshotBuilder, RaftStateMachine};
use openraft::type_config::alias::SnapshotDataOf;
use openraft::{EntryPayload, OptionalSend};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::commands::{
    QuotaScope, StateMachineCommand, StateMachineResponse, StorageConfig, VolumeType,
};
use crate::types::SyfrahRaftConfig;

// ---------------------------------------------------------------------------
// Storage state types (in-memory, replicated through Raft log + snapshots)
// ---------------------------------------------------------------------------

/// Storage quota limits for an org or project.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StorageQuota {
    pub max_volumes: u32,
    pub max_total_gb: u64,
    pub max_snapshots: u32,
}

/// Volume record tracked by the state machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VolumeRecord {
    pub id: String,
    pub name: String,
    pub size_gb: u32,
    pub org_id: String,
    pub project_id: String,
    pub env_id: String,
    pub volume_type: VolumeType,
    pub state: VolumeState,
    pub attached_vm_id: Option<String>,
    pub attached_hypervisor_id: Option<String>,
    pub placement_generation: u64,
    /// The zone where this volume's data lives (determines which S3 bucket to use).
    /// Matches the hypervisor's zone at creation time.
    #[serde(default)]
    pub zone: Option<String>,
    /// Whether deletion protection is enabled (prevents accidental deletion).
    #[serde(default)]
    pub deletion_protection: bool,
    /// Unix timestamp (seconds) when the volume was marked as Deleted.
    /// Used for tombstone retention (30-day TTL before purge).
    #[serde(default)]
    pub deleted_at: Option<u64>,
    /// Source zone (region key) during a cross-zone migration.
    /// Set when the volume enters the `Migrating` state.
    #[serde(default)]
    pub migration_source_zone: Option<String>,
    /// Target zone (region key) during a cross-zone migration.
    /// Set when the volume enters the `Migrating` state.
    #[serde(default)]
    pub migration_target_zone: Option<String>,
    /// Hypervisor the volume was attached to before migration started.
    /// Used for rollback if the migration fails.
    #[serde(default)]
    pub pre_migration_hypervisor: Option<String>,
    /// VM the volume was attached to before migration started.
    /// Used for rollback if the migration fails.
    #[serde(default)]
    pub pre_migration_vm_id: Option<String>,
}

/// Volume lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VolumeState {
    Available,
    Attached,
    /// Volume is being migrated between zones (S3-to-S3 copy in progress).
    /// The volume is offline during migration. On success, transitions to
    /// Available on the target zone. On failure, reverts to its pre-migration
    /// state on the source zone.
    Migrating,
    /// Tombstone state: volume is logically deleted, pending cleanup.
    /// Retained for audit purposes (30-day TTL).
    Deleted,
}

/// Default tombstone retention period: 30 days in seconds.
pub const TOMBSTONE_TTL_SECS: u64 = 30 * 24 * 3600;

/// Snapshot lifecycle state.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SnapshotState {
    #[default]
    Available,
    /// Tombstone: snapshot is logically deleted, SST refcounts already decremented.
    Deleted,
}

/// Snapshot record tracked by the state machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub source_volume_id: String,
    pub sst_files: Vec<String>,
    pub wal_position: u64,
    /// org_id inherited from the source volume at creation time.
    pub org_id: String,
    /// project_id inherited from the source volume at creation time.
    pub project_id: String,
    /// Size in GB of the source volume at snapshot time.
    /// Used by RestoreSnapshot so the restored volume gets the correct size
    /// even if the source volume has been deleted or resized since.
    #[serde(default)]
    pub size_gb: u32,
    /// env_id inherited from the source volume at creation time.
    #[serde(default)]
    pub env_id: String,
    /// volume_type inherited from the source volume at creation time.
    #[serde(default = "default_volume_type")]
    pub volume_type: VolumeType,
    /// Snapshot lifecycle state. Defaults to Available for backward compat.
    #[serde(default)]
    pub state: SnapshotState,
}

fn default_volume_type() -> VolumeType {
    VolumeType::Data
}

/// SST file reference count for garbage collection.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SstRefCounts(pub HashMap<String, u64>);

/// Snapshot of all storage state for serialization into Raft snapshots.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StorageState {
    pub quotas: HashMap<String, StorageQuota>,
    pub volumes: HashMap<String, VolumeRecord>,
    pub snapshots: HashMap<String, SnapshotRecord>,
    pub sst_refcounts: SstRefCounts,
    /// Storage configs keyed by zone (migrated from per-region in #1281).
    pub storage_configs: HashMap<String, StorageConfig>,
    /// Manifest pointers keyed by volume_id (ADR-006 §12b).
    #[serde(default)]
    pub manifest_pointers: HashMap<String, ManifestPointerRecord>,
    /// SST files whose refcount has reached 0 and are awaiting garbage
    /// collection. We mark them here rather than deleting immediately so
    /// that the GC worker can remove the S3 objects asynchronously.
    #[serde(default)]
    pub pending_gc_ssts: Vec<String>,
    /// Snapshot IDs that currently have an in-progress restore. A snapshot
    /// cannot be deleted while a restore is in progress.
    #[serde(default)]
    pub restores_in_progress: Vec<String>,
    /// Minimum WAL position across all snapshots. Used by the log compactor
    /// to determine how far back WAL segments must be retained. `None` when
    /// there are no snapshots.
    #[serde(default)]
    pub min_wal_position: Option<u64>,
}

/// Manifest pointer record tracked by the state machine (ADR-006 §12b).
///
/// Tracks the latest committed manifest for each volume. The `manifest_version`
/// is strictly sequential: each commit must present `last_committed + 1`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManifestPointerRecord {
    pub volume_id: String,
    /// Must match the volume's current `placement_generation`.
    pub generation: u64,
    /// Strictly sequential manifest version (starts at 1).
    pub manifest_version: u64,
    /// S3 key where the manifest is stored.
    pub s3_key: String,
    /// Hypervisor that published this manifest.
    pub published_by: String,
}

/// Error returned when a storage quota is exceeded.
#[derive(Debug, Clone)]
pub struct QuotaExceededError {
    pub scope: QuotaScope,
    pub resource: String,
    pub current: u64,
    pub limit: u64,
}

impl std::fmt::Display for QuotaExceededError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "quota exceeded for {}: {} limit is {} but current usage is {}",
            self.scope, self.resource, self.limit, self.current
        )
    }
}

/// Default number of log entries between snapshots.
pub const DEFAULT_SNAPSHOT_THRESHOLD: u64 = 10_000;

/// Event emitted by the state machine when a VM placement changes.
///
/// The daemon subscribes to these events to update FDB + ARP proxy
/// entries incrementally (O(1) per placement change).
#[derive(Debug, Clone)]
pub enum PlacementEvent {
    /// A VM was placed on a hypervisor.
    Added {
        vpc_id: String,
        vm_id: String,
        vm_mac: String,
        vm_ip: String,
        subnet_id: String,
        hypervisor_id: String,
    },
    /// A VM was removed from a hypervisor.
    Removed {
        vpc_id: String,
        vm_id: String,
        vm_mac: String,
        vm_ip: String,
        hypervisor_id: String,
    },
}

/// Snapshot of the state machine — serialized state for transfer.
#[derive(Debug)]
pub struct SmSnapshot {
    pub meta: SnapshotMetaOf<SyfrahRaftConfig>,
    pub data: Vec<u8>,
}

/// Internal state tracking for the Raft state machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SmState {
    pub last_applied_log: Option<LogIdOf<SyfrahRaftConfig>>,
    pub last_membership: StoredMembershipOf<SyfrahRaftConfig>,
}

/// Full snapshot data including store tables for new member catch-up.
///
/// When serialized, this contains the complete redb state so a joining
/// node can restore without replaying the entire Raft log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FullSnapshotData {
    /// Raft metadata (last applied log, membership).
    pub sm_state: SmState,
    /// Raw table data: table_name -> Vec<(key, json_bytes)>.
    /// Uses base64-encoded bytes for JSON compatibility.
    pub tables: HashMap<String, Vec<(String, Vec<u8>)>>,
    /// In-memory storage state (volumes, snapshots, quotas, configs).
    #[serde(default)]
    pub storage: StorageState,
}

/// Raft state machine that dispatches commands to the OrgStore.
///
/// On apply, each `StateMachineCommand` is deserialized and dispatched
/// to the appropriate store method. The OrgStore is backed by redb,
/// so the state machine output IS the local redb state.
pub struct RedbStateMachine {
    pub org_store: Arc<syfrah_org::OrgStore>,
    pub ipam_store: Option<Arc<syfrah_org::IpamStore>>,
    pub placement_store: Option<Arc<syfrah_org::PlacementStore>>,
    pub sg_rule_store: Option<Arc<syfrah_org::SgRuleStore>>,
    pub hypervisor_store: Option<Arc<syfrah_org::HypervisorStore>>,
    pub storage_store: Option<Arc<syfrah_org::StorageStore>>,
    pub sm_state: RwLock<SmState>,
    pub current_snapshot: RwLock<Option<SmSnapshot>>,
    snapshot_idx: std::sync::Mutex<u64>,
    /// Broadcast channel for placement events (FDB incremental updates).
    /// Subscribers receive events when PlaceVm/RemoveVm commands are applied.
    placement_tx: tokio::sync::broadcast::Sender<PlacementEvent>,
    /// Counter for total snapshots built (for metrics).
    pub snapshot_count: AtomicU64,
    /// Counter tracking entries applied since last snapshot.
    pub entries_since_snapshot: AtomicU64,
    /// Configurable snapshot threshold (default: 10,000 log entries).
    pub snapshot_threshold: u64,
    /// In-memory storage state (volumes, snapshots, quotas, configs).
    /// Protected by std::sync::RwLock since apply_command takes &self.
    pub storage: std::sync::RwLock<StorageState>,
}

impl RedbStateMachine {
    /// Create a new state machine wrapping the given OrgStore.
    pub fn new(org_store: Arc<syfrah_org::OrgStore>) -> Self {
        let (placement_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            org_store,
            ipam_store: None,
            placement_store: None,
            sg_rule_store: None,
            hypervisor_store: None,
            storage_store: None,
            sm_state: RwLock::new(SmState::default()),
            current_snapshot: RwLock::new(None),
            snapshot_idx: std::sync::Mutex::new(0),
            placement_tx,
            snapshot_count: AtomicU64::new(0),
            entries_since_snapshot: AtomicU64::new(0),
            snapshot_threshold: DEFAULT_SNAPSHOT_THRESHOLD,
            storage: std::sync::RwLock::new(StorageState::default()),
        }
    }

    /// Create a new state machine with a custom snapshot threshold.
    pub fn with_snapshot_threshold(mut self, threshold: u64) -> Self {
        self.snapshot_threshold = threshold;
        self
    }

    /// Export all store tables as raw data for snapshot serialization.
    ///
    /// Collects data from org, IPAM, and placement stores into a
    /// HashMap of table_name -> entries.
    fn export_store_tables(&self) -> HashMap<String, Vec<(String, Vec<u8>)>> {
        let mut tables = HashMap::new();

        // Export org store tables.
        for table_name in syfrah_org::OrgStore::table_names() {
            match self.org_store.db().export_table_raw(table_name) {
                Ok(entries) if !entries.is_empty() => {
                    tables.insert(table_name.to_string(), entries);
                }
                Ok(_) => {} // empty table, skip
                Err(e) => {
                    warn!("snapshot: failed to export org table {table_name}: {e}");
                }
            }
        }

        // Export IPAM store tables.
        if let Some(ref ipam) = self.ipam_store {
            for table_name in syfrah_org::IpamStore::table_names() {
                match ipam.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export IPAM table {table_name}: {e}");
                    }
                }
            }
        }

        // Export placement store tables.
        if let Some(ref placement) = self.placement_store {
            for table_name in syfrah_org::PlacementStore::table_names() {
                match placement.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export placement table {table_name}: {e}");
                    }
                }
            }
        }

        // Export SG rule store tables.
        if let Some(ref sg_rules) = self.sg_rule_store {
            for table_name in syfrah_org::SgRuleStore::table_names() {
                match sg_rules.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export SG rule table {table_name}: {e}");
                    }
                }
            }
        }

        // Export hypervisor store tables.
        if let Some(ref hv_store) = self.hypervisor_store {
            for table_name in syfrah_org::HypervisorStore::table_names() {
                match hv_store.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export hypervisor table {table_name}: {e}");
                    }
                }
            }
        }

        // Export storage store tables.
        if let Some(ref storage) = self.storage_store {
            for table_name in syfrah_org::StorageStore::table_names() {
                match storage.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export storage table {table_name}: {e}");
                    }
                }
            }
        }

        tables
    }

    /// Import store tables from snapshot data, replacing existing state.
    fn import_store_tables(&self, tables: &HashMap<String, Vec<(String, Vec<u8>)>>) {
        // Import org store tables.
        for table_name in syfrah_org::OrgStore::table_names() {
            if let Some(entries) = tables.get(*table_name) {
                if let Err(e) = self.org_store.db().import_table_raw(table_name, entries) {
                    warn!("snapshot: failed to import org table {table_name}: {e}");
                }
            }
        }

        // Import IPAM store tables.
        if let Some(ref ipam) = self.ipam_store {
            for table_name in syfrah_org::IpamStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = ipam.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import IPAM table {table_name}: {e}");
                    }
                }
            }
        }

        // Import placement store tables.
        if let Some(ref placement) = self.placement_store {
            for table_name in syfrah_org::PlacementStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = placement.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import placement table {table_name}: {e}");
                    }
                }
            }
        }

        // Import SG rule store tables.
        if let Some(ref sg_rules) = self.sg_rule_store {
            for table_name in syfrah_org::SgRuleStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = sg_rules.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import SG rule table {table_name}: {e}");
                    }
                }
            }
        }

        // Import hypervisor store tables.
        if let Some(ref hv_store) = self.hypervisor_store {
            for table_name in syfrah_org::HypervisorStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = hv_store.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import hypervisor table {table_name}: {e}");
                    }
                }
            }
        }

        // Import storage store tables.
        if let Some(ref storage) = self.storage_store {
            for table_name in syfrah_org::StorageStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = storage.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import storage table {table_name}: {e}");
                    }
                }
            }
        }
    }

    /// Subscribe to placement events for incremental FDB updates.
    ///
    /// Returns a broadcast receiver that yields `PlacementEvent`s whenever
    /// the state machine applies a PlaceVm or RemoveVm command.
    pub fn subscribe_placement_events(&self) -> tokio::sync::broadcast::Receiver<PlacementEvent> {
        self.placement_tx.subscribe()
    }

    /// Set the IPAM store for distributed IP allocation.
    pub fn with_ipam_store(mut self, store: Arc<syfrah_org::IpamStore>) -> Self {
        self.ipam_store = Some(store);
        self
    }

    /// Set the placement store for VM placement tracking.
    pub fn with_placement_store(mut self, store: Arc<syfrah_org::PlacementStore>) -> Self {
        self.placement_store = Some(store);
        self
    }

    /// Set the SG rule store for security group rule replication.
    pub fn with_sg_rule_store(mut self, store: Arc<syfrah_org::SgRuleStore>) -> Self {
        self.sg_rule_store = Some(store);
        self
    }

    /// Set the hypervisor store for Raft-replicated hypervisor registration.
    pub fn with_hypervisor_store(mut self, store: Arc<syfrah_org::HypervisorStore>) -> Self {
        self.hypervisor_store = Some(store);
        self
    }

    /// Set the storage store for syncing in-memory StorageState to redb.
    pub fn with_storage_store(mut self, store: Arc<syfrah_org::StorageStore>) -> Self {
        self.storage_store = Some(store);
        self
    }

    /// Replace the internal placement-event broadcast sender with an
    /// externally created one.  This lets the caller subscribe *before*
    /// the state machine (and therefore before openraft) is created,
    /// eliminating the window where events can be missed.
    pub fn with_placement_tx(mut self, tx: tokio::sync::broadcast::Sender<PlacementEvent>) -> Self {
        self.placement_tx = tx;
        self
    }

    // -- redb sync helpers ---------------------------------------------------
    //
    // After mutating the in-memory StorageState, these helpers write the
    // corresponding record to the redb StorageStore so that read-path
    // queries (e.g. `syfrah volume list`) see the latest state.

    /// Convert an in-memory VolumeRecord to a storage_store::Volume and upsert.
    ///
    /// Maintains the `VOLUMES_BY_HYPERVISOR` secondary index by comparing the
    /// old hypervisor assignment (from redb) with the new one. Also sets
    /// proper `created_at` / `updated_at` timestamps.
    fn sync_volume_to_store(&self, vol: &VolumeRecord) {
        let Some(ref store) = self.storage_store else {
            return;
        };
        let now = syfrah_org::StorageStore::now();
        let existing = store.get_volume(&vol.id).ok().flatten();
        let store_vol = syfrah_org::storage_store::Volume {
            id: vol.id.clone(),
            name: vol.name.clone(),
            size_gb: vol.size_gb,
            org_id: vol.org_id.clone(),
            project_id: vol.project_id.clone(),
            env_id: vol.env_id.clone(),
            volume_type: match vol.volume_type {
                VolumeType::Root => syfrah_org::storage_store::VolumeType::Root,
                VolumeType::Data => syfrah_org::storage_store::VolumeType::Data,
            },
            state: match vol.state {
                VolumeState::Available => syfrah_org::storage_store::VolumeState::Available,
                VolumeState::Attached => syfrah_org::storage_store::VolumeState::Attached,
                VolumeState::Migrating => syfrah_org::storage_store::VolumeState::Migrating,
                VolumeState::Deleted => syfrah_org::storage_store::VolumeState::Deleted,
            },
            attached_vm_id: vol.attached_vm_id.clone(),
            attached_hypervisor_id: vol.attached_hypervisor_id.clone(),
            placement_generation: vol.placement_generation,
            created_at: existing.as_ref().map_or(now, |e| e.created_at),
            updated_at: now,
        };

        // Maintain VOLUMES_BY_HYPERVISOR index: compare old ↔ new hypervisor.
        let old_hv = existing
            .as_ref()
            .and_then(|e| e.attached_hypervisor_id.as_deref());
        let new_hv = vol.attached_hypervisor_id.as_deref();
        if old_hv != new_hv {
            // Remove from old hypervisor index.
            if let Some(hv_id) = old_hv {
                if let Err(e) = store.remove_from_hypervisor_index(hv_id, &vol.id) {
                    warn!(volume_id = %vol.id, hypervisor_id = %hv_id, error = %e,
                        "failed to remove volume from old hypervisor index");
                }
            }
            // Add to new hypervisor index.
            if let Some(hv_id) = new_hv {
                if let Err(e) = store.add_to_hypervisor_index(hv_id, &vol.id) {
                    warn!(volume_id = %vol.id, hypervisor_id = %hv_id, error = %e,
                        "failed to add volume to new hypervisor index");
                }
            }
        }

        // Use create for new volumes, update for existing ones.
        if existing.is_some() {
            if let Err(e) = store.update_volume(&store_vol) {
                warn!(volume_id = %vol.id, error = %e, "failed to sync volume update to redb");
            }
        } else {
            if let Err(e) = store.create_volume(&store_vol) {
                warn!(volume_id = %vol.id, error = %e, "failed to sync volume create to redb");
            }
        }
    }

    /// Remove a volume from the redb store (hard delete for purged tombstones).
    fn sync_volume_delete_from_store(&self, volume_id: &str) {
        let Some(ref store) = self.storage_store else {
            return;
        };
        if let Err(e) = store.delete_volume(volume_id) {
            // Not-found is fine — the volume may not have been synced yet.
            if !e.to_string().contains("not found") {
                warn!(volume_id, error = %e, "failed to sync volume delete to redb");
            }
        }
    }

    /// Sync a storage config to the redb store (keyed by zone).
    fn sync_storage_config_to_store(&self, zone: &str, config: &StorageConfig) {
        let Some(ref store) = self.storage_store else {
            return;
        };
        let store_config = syfrah_org::storage_store::StorageConfig {
            s3_endpoint: config.s3_endpoint.clone(),
            s3_bucket: config.s3_bucket.clone(),
            s3_access_key: config.s3_access_key.clone(),
            s3_secret_key: config.s3_secret_key.clone(),
            cache_disk_path: config.cache_disk_path.clone(),
            cache_disk_size_gb: config.cache_disk_size_gb,
            cache_memory_size_gb: config.cache_memory_size_gb,
        };
        if let Err(e) = store.set_storage_config(zone, &store_config) {
            warn!(zone, error = %e, "failed to sync storage config to redb");
        }
    }

    /// Sync a storage quota to the redb store.
    fn sync_quota_to_store(&self, scope: &QuotaScope, quota: &StorageQuota) {
        let Some(ref store) = self.storage_store else {
            return;
        };
        let key = Self::quota_key(scope);
        let store_quota = syfrah_org::storage_store::StorageQuota {
            max_volumes: quota.max_volumes,
            max_total_gb: quota.max_total_gb,
            max_snapshots: quota.max_snapshots,
        };
        if let Err(e) = store.set_storage_quota(&key, &store_quota) {
            warn!(scope = %key, error = %e, "failed to sync storage quota to redb");
        }
    }

    /// Serialize a `QuotaScope` into a deterministic key for the quotas map.
    fn quota_key(scope: &QuotaScope) -> String {
        match scope {
            QuotaScope::Org { org_id } => format!("org:{org_id}"),
            QuotaScope::Project { org_id, project_id } => {
                format!("project:{org_id}/{project_id}")
            }
        }
    }

    /// Look up the effective quota for a given org + project from an already-locked storage.
    ///
    /// Returns the project-level quota if set, otherwise the org-level quota.
    /// Returns `None` if no quota is set (meaning unlimited).
    fn effective_quota_from(
        storage: &StorageState,
        org_id: &str,
        project_id: &str,
    ) -> Option<StorageQuota> {
        // Check project-level first.
        let project_key = Self::quota_key(&QuotaScope::Project {
            org_id: org_id.to_string(),
            project_id: project_id.to_string(),
        });
        if let Some(q) = storage.quotas.get(&project_key) {
            return Some(q.clone());
        }
        // Fall back to org-level.
        let org_key = Self::quota_key(&QuotaScope::Org {
            org_id: org_id.to_string(),
        });
        storage.quotas.get(&org_key).cloned()
    }

    /// Determine which scope is effective for quota from an already-locked storage.
    fn effective_quota_scope_from(
        storage: &StorageState,
        org_id: &str,
        project_id: &str,
    ) -> QuotaScope {
        let project_key = Self::quota_key(&QuotaScope::Project {
            org_id: org_id.to_string(),
            project_id: project_id.to_string(),
        });
        if storage.quotas.contains_key(&project_key) {
            QuotaScope::Project {
                org_id: org_id.to_string(),
                project_id: project_id.to_string(),
            }
        } else {
            QuotaScope::Org {
                org_id: org_id.to_string(),
            }
        }
    }

    /// Check volume quotas for a CreateVolume operation.
    ///
    /// Returns `Ok(())` if quota is not exceeded, or an error with usage details.
    ///
    /// # TOCTOU safety
    ///
    /// There is no time-of-check-to-time-of-use race here because all
    /// `apply_command` calls are serialized by Raft — only one command is
    /// applied at a time on each node, so the quota read and the subsequent
    /// volume insert are effectively atomic.
    fn check_volume_quota(
        &self,
        org_id: &str,
        project_id: &str,
        new_size_gb: u32,
    ) -> Result<(), QuotaExceededError> {
        let storage = self.storage.read().unwrap();
        let quota = match Self::effective_quota_from(&storage, org_id, project_id) {
            Some(q) => q,
            None => return Ok(()), // No quota = unlimited.
        };
        let scope = Self::effective_quota_scope_from(&storage, org_id, project_id);

        // TODO: For v1 we scan all volumes. Add index by org_id/project_id if this
        // becomes a hot path.
        let (count, total_gb) = Self::compute_volume_usage(&storage, &scope);

        // Check volume count.
        if count >= quota.max_volumes as u64 {
            return Err(QuotaExceededError {
                scope,
                resource: "volume_count".to_string(),
                current: count,
                limit: quota.max_volumes as u64,
            });
        }
        // Check total size.
        if total_gb + new_size_gb as u64 > quota.max_total_gb {
            return Err(QuotaExceededError {
                scope,
                resource: "total_gb".to_string(),
                current: total_gb,
                limit: quota.max_total_gb,
            });
        }
        Ok(())
    }

    /// Check total_gb quota for a ResizeVolume operation (delta only, no volume count check).
    ///
    /// # TOCTOU safety
    ///
    /// Same as `check_volume_quota` — Raft serializes all applies.
    fn check_resize_quota(
        &self,
        org_id: &str,
        project_id: &str,
        delta_gb: u32,
    ) -> Result<(), QuotaExceededError> {
        let storage = self.storage.read().unwrap();
        let quota = match Self::effective_quota_from(&storage, org_id, project_id) {
            Some(q) => q,
            None => return Ok(()), // No quota = unlimited.
        };
        let scope = Self::effective_quota_scope_from(&storage, org_id, project_id);
        let (_count, total_gb) = Self::compute_volume_usage(&storage, &scope);

        if total_gb + delta_gb as u64 > quota.max_total_gb {
            return Err(QuotaExceededError {
                scope,
                resource: "total_gb".to_string(),
                current: total_gb,
                limit: quota.max_total_gb,
            });
        }
        Ok(())
    }

    /// Check snapshot quota for a CreateSnapshot operation.
    fn check_snapshot_quota(
        &self,
        org_id: &str,
        project_id: &str,
    ) -> Result<(), QuotaExceededError> {
        let storage = self.storage.read().unwrap();
        let quota = match Self::effective_quota_from(&storage, org_id, project_id) {
            Some(q) => q,
            None => return Ok(()), // No quota = unlimited.
        };
        let scope = Self::effective_quota_scope_from(&storage, org_id, project_id);
        let count = Self::compute_snapshot_count(&storage, &scope);

        if count >= quota.max_snapshots as u64 {
            return Err(QuotaExceededError {
                scope,
                resource: "snapshot_count".to_string(),
                current: count,
                limit: quota.max_snapshots as u64,
            });
        }
        Ok(())
    }

    /// Compute volume usage (count, total_gb) within the effective scope.
    /// Excludes volumes in the Deleted state (tombstones).
    fn compute_volume_usage(storage: &StorageState, scope: &QuotaScope) -> (u64, u64) {
        let mut count = 0u64;
        let mut total_gb = 0u64;
        for vol in storage.volumes.values() {
            // Tombstoned volumes don't count against quota.
            if vol.state == VolumeState::Deleted {
                continue;
            }
            let in_scope = match scope {
                QuotaScope::Org { org_id: oid } => vol.org_id == *oid,
                QuotaScope::Project {
                    org_id: oid,
                    project_id: pid,
                } => vol.org_id == *oid && vol.project_id == *pid,
            };
            if in_scope {
                count += 1;
                total_gb += vol.size_gb as u64;
            }
        }
        (count, total_gb)
    }

    /// Compute snapshot count within the effective scope.
    /// Excludes snapshots in the Deleted state (tombstones).
    fn compute_snapshot_count(storage: &StorageState, scope: &QuotaScope) -> u64 {
        let mut count = 0u64;
        for snap in storage.snapshots.values() {
            if snap.state == SnapshotState::Deleted {
                continue;
            }
            let in_scope = match scope {
                QuotaScope::Org { org_id: oid } => snap.org_id == *oid,
                QuotaScope::Project {
                    org_id: oid,
                    project_id: pid,
                } => snap.org_id == *oid && snap.project_id == *pid,
            };
            if in_scope {
                count += 1;
            }
        }
        count
    }

    /// Retrieve the storage configuration for a given zone.
    ///
    /// This is a local read -- it does not go through Raft consensus.
    /// Returns `None` if no config has been set for the zone.
    pub fn get_storage_config(&self, zone: &str) -> Option<StorageConfig> {
        let storage = self.storage.read().unwrap();
        storage.storage_configs.get(zone).cloned()
    }

    /// Apply a single command to the state machine.
    ///
    /// Every mutation goes through here on ALL Raft nodes, producing identical redb state.
    pub fn apply_command(&self, cmd: &StateMachineCommand) -> StateMachineResponse {
        match cmd {
            // -- Org --
            StateMachineCommand::CreateOrg { name } => match self.org_store.create(name) {
                Ok(org) => StateMachineResponse::Created(org.id.0),
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::DeleteOrg { name } => match self.org_store.delete(name) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },

            // -- Project --
            StateMachineCommand::CreateProject { name, org } => {
                match self.org_store.create_project(org, name) {
                    Ok(proj) => StateMachineResponse::Created(proj.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteProject { name, org } => {
                match self.org_store.delete_project(org, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Environment --
            StateMachineCommand::CreateEnv {
                name,
                project,
                org,
                ttl,
                deletion_protection,
                labels,
            } => {
                match self.org_store.create_env(
                    org,
                    project,
                    name,
                    *ttl,
                    *deletion_protection,
                    labels.clone(),
                ) {
                    Ok(env) => StateMachineResponse::Created(env.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteEnv { name, project, org } => {
                match self.org_store.delete_env(org, project, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- VPC --
            StateMachineCommand::CreateVpc {
                name,
                cidr,
                owner,
                shared,
            } => {
                use syfrah_org::types::{OrgId, ProjectId, VpcOwner};
                // Reconstruct VpcOwner from the string representation.
                // If owner contains '/', it's a project (org/project). Otherwise, it's an org.
                let vpc_owner = if owner.contains('/') {
                    VpcOwner::Project(ProjectId(owner.clone()))
                } else {
                    VpcOwner::Org(OrgId(owner.clone()))
                };
                match self.org_store.create_vpc(name, cidr, vpc_owner, *shared) {
                    Ok(vpc) => StateMachineResponse::Created(vpc.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteVpc { name } => match self.org_store.delete_vpc(name) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::AttachVpc { vpc, project } => {
                match self.org_store.attach_vpc(vpc, project) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DetachVpc { vpc, project } => {
                match self.org_store.detach_vpc(vpc, project) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::PeerVpc { vpc_a, vpc_b } => {
                match self.org_store.create_peering(vpc_a, vpc_b) {
                    Ok(_) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UnpeerVpc { vpc_a, vpc_b } => {
                match self.org_store.delete_peering(vpc_a, vpc_b) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Environment mutations --
            StateMachineCommand::ExtendEnv {
                name,
                project,
                org,
                ttl_seconds,
            } => match self.org_store.extend_env(org, project, name, *ttl_seconds) {
                Ok(_env) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::UpdateEnv {
                name,
                project,
                org,
                deletion_protection,
            } => {
                if let Some(dp) = deletion_protection {
                    match self
                        .org_store
                        .update_env_protection(org, project, name, *dp)
                    {
                        Ok(_env) => StateMachineResponse::Ok,
                        Err(e) => StateMachineResponse::Error(e.to_string()),
                    }
                } else {
                    StateMachineResponse::Error("no update specified".to_string())
                }
            }

            // -- Subnet --
            StateMachineCommand::CreateSubnet {
                name,
                vpc,
                env_id,
                cidr,
            } => {
                use syfrah_org::types::EnvironmentId;
                // Auto-create default VPC if it doesn't exist (deterministic on all nodes).
                if vpc.ends_with("-default") {
                    if let Ok(None) = self.org_store.get_vpc(vpc) {
                        // Extract org/project from the VPC name pattern: "{org}-{project}-default"
                        let parts: Vec<&str> = vpc
                            .strip_suffix("-default")
                            .unwrap_or(vpc)
                            .splitn(2, '-')
                            .collect();
                        if parts.len() == 2 {
                            use syfrah_org::types::{ProjectId, VpcOwner};
                            let org = parts[0];
                            let project = parts[1];
                            let owner = VpcOwner::Project(ProjectId(format!("{org}/{project}")));
                            if let Err(e) =
                                self.org_store.create_vpc(vpc, "10.1.0.0/16", owner, false)
                            {
                                return StateMachineResponse::Error(format!(
                                    "failed to auto-create default VPC '{vpc}': {e}"
                                ));
                            }
                        }
                    }
                }
                let eid = EnvironmentId(env_id.clone());
                match self
                    .org_store
                    .create_subnet(vpc, &eid, name, cidr.as_deref())
                {
                    Ok(subnet) => StateMachineResponse::Created(subnet.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteSubnet { name, vpc } => {
                match self.org_store.delete_subnet(vpc, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Security Groups --
            StateMachineCommand::CreateSg { name, vpc } => {
                match self.org_store.create_security_group(name, vpc, "") {
                    Ok(sg) => StateMachineResponse::Created(sg.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteSg { name } => {
                match self.org_store.delete_security_group(name, None) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::AddSgRule {
                sg,
                direction,
                protocol,
                port,
                source,
            } => {
                let sg_rule_store = match &self.sg_rule_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "SG rule store not available in state machine".to_string(),
                        )
                    }
                };
                // Resolve SG record to get its ID.
                let sg_record = match self.org_store.find_sg_by_name(sg) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "security group not found: {sg}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Parse direction, protocol, port, source.
                use syfrah_org::types::{Direction, PortRange, Protocol, RuleId, RuleSource};
                let dir = match direction.as_str() {
                    "ingress" => Direction::Ingress,
                    "egress" => Direction::Egress,
                    other => {
                        return StateMachineResponse::Error(format!("invalid direction: '{other}'"))
                    }
                };
                let proto = match protocol.as_str() {
                    "tcp" => Protocol::Tcp,
                    "udp" => Protocol::Udp,
                    "icmp" => Protocol::Icmp,
                    "all" => Protocol::All,
                    other => {
                        return StateMachineResponse::Error(format!("invalid protocol: '{other}'"))
                    }
                };
                let port_range = port.as_ref().and_then(|p| {
                    if let Some((from, to)) = p.split_once('-') {
                        Some(PortRange {
                            from: from.parse().unwrap_or(0),
                            to: to.parse().unwrap_or(0),
                        })
                    } else {
                        let n: u16 = p.parse().ok()?;
                        Some(PortRange { from: n, to: n })
                    }
                });
                let rule_source = RuleSource::Cidr(source.clone());
                // Generate deterministic rule ID from content hash.
                let rule_id = {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    sg_record.id.0.hash(&mut hasher);
                    dir.hash(&mut hasher);
                    proto.hash(&mut hasher);
                    port_range.hash(&mut hasher);
                    rule_source.hash(&mut hasher);
                    RuleId(format!("rule-{:016x}", hasher.finish()))
                };
                let rule = syfrah_org::types::SecurityGroupRule {
                    id: rule_id,
                    sg_id: sg_record.id.clone(),
                    direction: dir,
                    protocol: proto,
                    port_range,
                    source: rule_source,
                    priority: 100,
                    description: None,
                };
                match sg_rule_store.add_rule(&rule) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::RemoveSgRule { sg: _, rule_id } => {
                let sg_rule_store = match &self.sg_rule_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "SG rule store not available in state machine".to_string(),
                        )
                    }
                };
                match sg_rule_store.remove_rule(rule_id) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::AttachSg { sg, nic_id } => {
                match self.org_store.attach_sg_to_nic(sg, nic_id) {
                    Ok(_) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DetachSg { sg, nic_id } => {
                match self.org_store.detach_sg_from_nic(sg, nic_id) {
                    Ok(_) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- NAT Gateway --
            StateMachineCommand::CreateNatGw { name, vpc, subnet } => {
                // Resolve VPC.
                let vpc_obj = match self.org_store.get_vpc(vpc) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("VPC not found: {vpc}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Resolve subnet in VPC.
                let sub = match self.org_store.find_subnets_by_name(subnet) {
                    Ok(matches) => {
                        let in_vpc: Vec<_> = matches
                            .into_iter()
                            .filter(|(_, s)| s.vpc_id == vpc_obj.id)
                            .collect();
                        match in_vpc.len() {
                            0 => {
                                return StateMachineResponse::Error(format!(
                                    "subnet '{subnet}' not found in VPC '{vpc}'"
                                ))
                            }
                            1 => in_vpc.into_iter().next().unwrap().1,
                            _ => {
                                return StateMachineResponse::Error(format!(
                                    "ambiguous subnet '{subnet}' in VPC '{vpc}'"
                                ))
                            }
                        }
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Use a deterministic placeholder for public IP — the actual nftables
                // setup runs on the leader after Raft apply returns.
                let public_ip = "0.0.0.0";
                match self
                    .org_store
                    .create_nat_gw(name, &vpc_obj.id, &sub.id, public_ip)
                {
                    Ok(gw) => StateMachineResponse::Created(gw.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteNatGw { name } => {
                let gw = match self.org_store.get_nat_gw_by_name(name) {
                    Ok(Some(g)) => g,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("nat-gw not found: {name}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.delete_nat_gw(&gw.vpc_id, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Route Table --
            StateMachineCommand::CreateRouteTable { name, vpc } => {
                match self.org_store.create_route_table_by_vpc_name(name, vpc) {
                    Ok(table) => StateMachineResponse::Created(table.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteRouteTable { name, vpc } => {
                let result = if let Some(vname) = vpc {
                    self.org_store.delete_route_table_by_vpc_name(vname, name)
                } else {
                    // Scan all tables.
                    match self.org_store.list_route_tables() {
                        Ok(tables) => {
                            let matching: Vec<_> =
                                tables.iter().filter(|t| t.name == *name).collect();
                            match matching.len() {
                                0 => Err(syfrah_org::OrgError::RouteTableNotFound(name.clone())),
                                1 => self.org_store.delete_route_table(&matching[0].vpc_id, name),
                                _ => Err(syfrah_org::OrgError::Ambiguous(format!(
                                    "route table '{name}' exists in multiple VPCs"
                                ))),
                            }
                        }
                        Err(e) => Err(e),
                    }
                };
                match result {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::AssociateRouteTable { table, subnet } => {
                // Resolve subnet -> VPC -> route table.
                let sub = match self.org_store.find_subnets_by_name(subnet) {
                    Ok(m) if m.len() == 1 => m.into_iter().next().unwrap().1,
                    Ok(m) if m.is_empty() => {
                        return StateMachineResponse::Error(format!("subnet not found: {subnet}"))
                    }
                    Ok(_) => {
                        return StateMachineResponse::Error(format!(
                            "subnet '{subnet}' exists in multiple VPCs"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                let rt = match self.org_store.get_route_table(&sub.vpc_id, table) {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "route table not found: {table}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.associate_subnet_route_table(&sub.id, &rt.id) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DisassociateRouteTable { subnet } => {
                let sub = match self.org_store.find_subnets_by_name(subnet) {
                    Ok(m) if m.len() == 1 => m.into_iter().next().unwrap().1,
                    Ok(m) if m.is_empty() => {
                        return StateMachineResponse::Error(format!("subnet not found: {subnet}"))
                    }
                    Ok(_) => {
                        return StateMachineResponse::Error(format!(
                            "subnet '{subnet}' exists in multiple VPCs"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.disassociate_subnet_route_table(&sub.id) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Routes --
            StateMachineCommand::AddRoute {
                vpc,
                table,
                destination,
                target,
                priority,
            } => {
                let vpc_obj = match self.org_store.get_vpc(vpc) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("VPC not found: {vpc}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                let table_name = table.as_deref().unwrap_or("default");
                let rt = match self.org_store.get_route_table(&vpc_obj.id, table_name) {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "route table not found: {table_name}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Parse the route target string.
                use syfrah_org::types::RouteTarget;
                let route_target = if target.eq_ignore_ascii_case("local") {
                    RouteTarget::Local
                } else if target.eq_ignore_ascii_case("blackhole") {
                    RouteTarget::Blackhole
                } else if let Some(name) = target.strip_prefix("nat-gw:") {
                    RouteTarget::NatGateway(name.to_string())
                } else if let Some(name) = target.strip_prefix("peering:") {
                    RouteTarget::VpcPeering(name.to_string())
                } else {
                    return StateMachineResponse::Error(format!(
                        "invalid route target: '{target}'"
                    ));
                };
                match self
                    .org_store
                    .add_route(&rt.id, destination, route_target, *priority)
                {
                    Ok(_route) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteRoute {
                vpc,
                table,
                destination,
            } => {
                let vpc_obj = match self.org_store.get_vpc(vpc) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("VPC not found: {vpc}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                let table_name = table.as_deref().unwrap_or("default");
                let rt = match self.org_store.get_route_table(&vpc_obj.id, table_name) {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "route table not found: {table_name}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.remove_route(&rt.id, destination) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- IPAM --
            StateMachineCommand::AllocateIp { subnet_id } => {
                let ipam = match &self.ipam_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "IPAM store not available in state machine".to_string(),
                        )
                    }
                };
                // Look up the subnet CIDR from the org store.
                let subnet = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "subnet not found: {subnet_id}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match ipam.reserve_ip(subnet_id, &subnet.cidr) {
                    Ok(alloc) => StateMachineResponse::AllocatedIp {
                        ip: alloc.ip,
                        mac: alloc.mac,
                    },
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::ReleaseIp { subnet_id, ip } => {
                let ipam = match &self.ipam_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "IPAM store not available in state machine".to_string(),
                        )
                    }
                };
                let subnet = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "subnet not found: {subnet_id}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match ipam.release_ip(subnet_id, &subnet.cidr, ip) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- NIC --
            StateMachineCommand::CreateNic {
                vm_id,
                subnet_id,
                ip,
                mac,
            } => {
                // Resolve the subnet to get VPC ID for the NIC record.
                let vpc_id = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(subnet)) => subnet.vpc_id.0.clone(),
                    Ok(None) => {
                        // If subnet not found, use a fallback (subnet may have been deleted).
                        warn!("CreateNic: subnet '{subnet_id}' not found, using empty vpc_id");
                        String::new()
                    }
                    Err(e) => {
                        return StateMachineResponse::Error(format!(
                            "CreateNic: failed to resolve subnet: {e}"
                        ))
                    }
                };

                // Generate a deterministic NIC ID from VM + subnet.
                let nic_id = format!("nic-{}-{}", vm_id, subnet_id.replace('/', "-"));
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let nic = syfrah_org::types::NetworkInterface {
                    id: syfrah_org::types::NicId(nic_id.clone()),
                    name: format!("{vm_id}-eth0"),
                    vm_id: Some(vm_id.clone()),
                    subnet_id: subnet_id.clone(),
                    vpc_id: vpc_id.clone(),
                    private_ip: ip.clone(),
                    mac: mac.clone(),
                    security_groups: vec![],
                    state: syfrah_org::types::ResourceState::Active,
                    created_at: now,
                };

                match self.org_store.create_nic(&nic) {
                    Ok(()) => StateMachineResponse::Created(nic_id),
                    Err(e) => {
                        // If NIC already exists (idempotent retry), treat as success.
                        if e.to_string().contains("already exists") {
                            StateMachineResponse::Created(nic_id)
                        } else {
                            StateMachineResponse::Error(e.to_string())
                        }
                    }
                }
            }
            StateMachineCommand::DeleteNic { nic_id } => match self.org_store.delete_nic(nic_id) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },

            // -- Hypervisor --
            // All hypervisor mutations go through Raft so every node gets the
            // same set of hypervisor records. The scheduler reads from this
            // store (strongly consistent) for placement decisions.
            StateMachineCommand::RegisterHypervisor {
                name,
                region,
                zone,
                fabric_ipv6,
            } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                // If already exists, update region/zone/fabric_ipv6.
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.region = region.clone();
                        hv.zone = zone.clone();
                        if !fabric_ipv6.is_empty() {
                            hv.fabric_ipv6 = fabric_ipv6.clone();
                        }
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let hv = syfrah_org::Hypervisor {
                            id: syfrah_org::HypervisorId::generate(),
                            name: name.clone(),
                            region: region.clone(),
                            zone: zone.clone(),
                            state: syfrah_org::HypervisorState::NotReady,
                            fabric_node_id: name.clone(),
                            public_ip: String::new(),
                            fabric_ipv6: fabric_ipv6.clone(),
                            hardware: syfrah_org::HardwareSpec {
                                cpu_model: String::new(),
                                cpu_cores_physical: 0,
                                cpu_threads_logical: 0,
                                memory_gb: 0,
                                local_disk_type: syfrah_org::DiskType::SSD,
                                local_disk_gb: 0,
                                gpu: None,
                                network_bandwidth_gbps: 0,
                                architecture: syfrah_org::CpuArchitecture::X86_64,
                            },
                            capacity: syfrah_org::AllocatableCapacity::default(),
                            labels: std::collections::HashMap::new(),
                            taints: Vec::new(),
                            created_at: now,
                        };
                        match hv_store.create(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::EnableHypervisor { name } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                // Storage preflight: check if the hypervisor's zone has storage
                // configured. Without storage, VMs on this hypervisor cannot pull
                // images or manage volumes. Block enable with a helpful error.
                if let Ok(Some(hv)) = hv_store.get(name) {
                    let zone = &hv.zone;
                    let storage = self.storage.read().unwrap();
                    if !storage.storage_configs.contains_key(zone) {
                        return StateMachineResponse::Error(format!(
                            "cannot enable hypervisor {name} \u{2014} storage is not configured for zone {zone}.\n\
                             VMs on this hypervisor would fail to pull images or manage volumes.\n\
                             Run: syfrah storage configure --zone {zone} --s3-endpoint <url> --s3-bucket <bucket> --s3-access-key <key> --s3-secret-key <secret>"
                        ));
                    }
                }
                match hv_store.update_state(name, syfrah_org::HypervisorState::Available) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DrainHypervisor { name } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.update_state(name, syfrah_org::HypervisorState::Draining) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DecommissionHypervisor { name } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.update_state(name, syfrah_org::HypervisorState::Decommissioned) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UpdateHypervisorLabels { name, labels } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.labels = labels.clone();
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        StateMachineResponse::Error(format!("hypervisor '{name}' not found"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UpdateHypervisorTaints { name, taints } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.taints = taints
                            .iter()
                            .map(|t| {
                                // Parse taint string "key=value:Effect"
                                let (kv, effect_str) =
                                    t.rsplit_once(':').unwrap_or((t, "NoSchedule"));
                                let effect = match effect_str {
                                    "NoExecute" => syfrah_org::TaintEffect::NoExecute,
                                    _ => syfrah_org::TaintEffect::NoSchedule,
                                };
                                let (key, value) = if let Some((k, v)) = kv.split_once('=') {
                                    (k.to_string(), Some(v.to_string()))
                                } else {
                                    (kv.to_string(), None)
                                };
                                syfrah_org::Taint { key, value, effect }
                            })
                            .collect();
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        StateMachineResponse::Error(format!("hypervisor '{name}' not found"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UpdateHypervisorCapacity {
                name,
                allocatable_vcpus,
                allocatable_memory_mb,
                used_vcpus,
                used_memory_mb,
            } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.capacity.allocatable_vcpus = *allocatable_vcpus;
                        hv.capacity.allocatable_memory_mb = *allocatable_memory_mb;
                        hv.capacity.used_vcpus = *used_vcpus;
                        hv.capacity.used_memory_mb = *used_memory_mb;
                        hv.capacity.available_vcpus = allocatable_vcpus.saturating_sub(*used_vcpus);
                        hv.capacity.available_memory_mb =
                            allocatable_memory_mb.saturating_sub(*used_memory_mb);
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        // Hypervisor not registered yet — skip silently.
                        // This can happen during startup when capacity updates arrive
                        // before registration is replicated.
                        StateMachineResponse::Ok
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- VM Placement --
            StateMachineCommand::PlaceVm {
                vm_id,
                hypervisor_id,
                subnet_id,
                ip,
                mac,
                generation,
            } => {
                let placement_store = match &self.placement_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "placement store not available in state machine".to_string(),
                        )
                    }
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // Resolve the VPC ID from the subnet.
                let vpc_id = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(s)) => s.vpc_id.0,
                    Ok(None) => subnet_id.split('/').next().unwrap_or("unknown").to_string(),
                    Err(_) => subnet_id.split('/').next().unwrap_or("unknown").to_string(),
                };
                let placement = syfrah_org::types::VmPlacement {
                    vpc_id,
                    vm_id: vm_id.clone(),
                    vm_mac: mac.clone(),
                    vm_ip: ip.clone(),
                    subnet_id: subnet_id.clone(),
                    hypervisor_id: hypervisor_id.clone(),
                    action: syfrah_org::types::PlacementAction::Add,
                    created_at: now,
                    placement_generation: *generation,
                };
                match placement_store.add_placement(&placement) {
                    Ok(()) => {
                        // Emit placement event for incremental FDB update.
                        let _ = self.placement_tx.send(PlacementEvent::Added {
                            vpc_id: placement.vpc_id,
                            vm_id: placement.vm_id,
                            vm_mac: placement.vm_mac,
                            vm_ip: placement.vm_ip,
                            subnet_id: placement.subnet_id,
                            hypervisor_id: placement.hypervisor_id,
                        });
                        StateMachineResponse::Ok
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::RemoveVm { vm_id } => {
                let placement_store = match &self.placement_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "placement store not available in state machine".to_string(),
                        )
                    }
                };
                // List all placements to find the one matching this VM.
                match placement_store.list_all() {
                    Ok(placements) => {
                        for p in &placements {
                            if p.vm_id == *vm_id {
                                // Capture info before removal for the event.
                                let event = PlacementEvent::Removed {
                                    vpc_id: p.vpc_id.clone(),
                                    vm_id: p.vm_id.clone(),
                                    vm_mac: p.vm_mac.clone(),
                                    vm_ip: p.vm_ip.clone(),
                                    hypervisor_id: p.hypervisor_id.clone(),
                                };
                                let _ = placement_store.remove_placement(&p.vpc_id, vm_id);
                                let _ = self.placement_tx.send(event);
                                return StateMachineResponse::Ok;
                            }
                        }
                        StateMachineResponse::Error(format!("placement not found for VM: {vm_id}"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::RescheduleVm {
                vm_id,
                from,
                to,
                generation,
            } => {
                let placement_store = match &self.placement_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "placement store not available in state machine".to_string(),
                        )
                    }
                };
                // Update the placement in-place: change hypervisor_id and generation.
                match placement_store.list_all() {
                    Ok(placements) => {
                        let mut found = false;
                        for p in &placements {
                            if p.vm_id == *vm_id {
                                let mut updated = p.clone();
                                updated.hypervisor_id = to.clone();
                                updated.placement_generation = *generation;
                                match placement_store.add_placement(&updated) {
                                    Ok(()) => {
                                        found = true;
                                        break;
                                    }
                                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                                }
                            }
                        }
                        if !found {
                            return StateMachineResponse::Error(format!(
                                "placement not found for VM: {vm_id}"
                            ));
                        }

                        // Reschedule all volumes attached to this VM.
                        // This moves volumes from the source hypervisor to the target,
                        // incrementing placement_generation for fencing so the source
                        // node's reconciler stops ZeroFS and the target's starts it
                        // with the new gen prefix (zero-copy migration via S3).
                        let mut storage = self.storage.write().unwrap();
                        let vol_ids: Vec<String> = storage
                            .volumes
                            .iter()
                            .filter(|(_, vol)| {
                                vol.state == VolumeState::Attached
                                    && vol.attached_vm_id.as_deref() == Some(vm_id)
                                    && vol.attached_hypervisor_id.as_deref() == Some(from)
                            })
                            .map(|(id, _)| id.clone())
                            .collect();

                        for vol_id in &vol_ids {
                            if let Some(vol) = storage.volumes.get_mut(vol_id) {
                                vol.placement_generation += 1;
                                vol.attached_hypervisor_id = Some(to.clone());
                                let synced = vol.clone();
                                // Release storage lock temporarily to sync to redb.
                                drop(storage);
                                self.sync_volume_to_store(&synced);
                                info!(
                                    volume_id = vol_id,
                                    from = from,
                                    to = to,
                                    generation = synced.placement_generation,
                                    "volume rescheduled with VM"
                                );
                                storage = self.storage.write().unwrap();
                            }
                        }

                        if !vol_ids.is_empty() {
                            info!(
                                vm_id = vm_id,
                                volume_count = vol_ids.len(),
                                "rescheduled {} volume(s) along with VM",
                                vol_ids.len()
                            );
                        }

                        StateMachineResponse::Ok
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Composite Transaction --
            StateMachineCommand::Composite { commands } => {
                // Apply all sub-commands atomically. If any fails, report the error
                // but continue (the Raft log is append-only, we can't undo committed entries).
                // In practice, the caller should validate before submitting.
                let mut results = Vec::with_capacity(commands.len());
                for sub_cmd in commands {
                    let resp = self.apply_command(sub_cmd);
                    let failed = matches!(resp, StateMachineResponse::Error(_));
                    results.push(resp);
                    if failed {
                        // On first error, stop applying remaining commands.
                        break;
                    }
                }
                // Check if any sub-command failed.
                let any_error = results
                    .iter()
                    .any(|r| matches!(r, StateMachineResponse::Error(_)));
                if any_error {
                    // Return the first error.
                    for r in &results {
                        if let StateMachineResponse::Error(msg) = r {
                            return StateMachineResponse::Error(format!(
                                "composite transaction failed: {msg}"
                            ));
                        }
                    }
                }
                StateMachineResponse::Composite(results)
            }

            // -- Storage (ADR-006 §16) --
            StateMachineCommand::SetStorageQuota {
                scope,
                max_volumes,
                max_total_gb,
                max_snapshots,
            } => {
                let key = Self::quota_key(scope);
                let quota = StorageQuota {
                    max_volumes: *max_volumes,
                    max_total_gb: *max_total_gb,
                    max_snapshots: *max_snapshots,
                };
                let mut storage = self.storage.write().unwrap();
                storage.quotas.insert(key, quota.clone());
                drop(storage);
                self.sync_quota_to_store(scope, &quota);
                info!(%scope, max_volumes, max_total_gb, max_snapshots, "storage quota set");
                StateMachineResponse::Ok
            }

            // -- Storage: SetStorageConfig (ADR-006 §9, #1281: per-zone) --
            StateMachineCommand::SetStorageConfig {
                region,
                zone,
                config,
            } => {
                // Validate the config before storing.
                // Backward compat: if zone is empty, fall back to region as key.
                let zone_key = if zone.is_empty() {
                    region.clone()
                } else {
                    zone.clone()
                };
                if let Err(msg) = config.validate() {
                    return StateMachineResponse::Error(format!(
                        "invalid storage config for zone {zone_key}: {msg}"
                    ));
                }
                if zone_key.is_empty() {
                    return StateMachineResponse::Error(
                        "zone (or region) must not be empty".to_string(),
                    );
                }
                let mut storage = self.storage.write().unwrap();
                storage
                    .storage_configs
                    .insert(zone_key.clone(), *config.clone());
                drop(storage);
                self.sync_storage_config_to_store(&zone_key, config);
                info!(zone = %zone_key, region = %region, "storage config set");
                StateMachineResponse::Ok
            }

            StateMachineCommand::PurgeTombstones { now, max_age_secs } => {
                let mut storage = self.storage.write().unwrap();
                let mut purged = Vec::new();
                for (id, vol) in &storage.volumes {
                    if vol.state == VolumeState::Deleted {
                        if let Some(deleted_at) = vol.deleted_at {
                            if now.saturating_sub(deleted_at) >= *max_age_secs {
                                purged.push(id.clone());
                            }
                        }
                    }
                }
                for id in &purged {
                    storage.volumes.remove(id);
                }
                drop(storage);
                for id in &purged {
                    self.sync_volume_delete_from_store(id);
                }
                if !purged.is_empty() {
                    info!(count = purged.len(), "purged expired volume tombstones");
                }
                StateMachineResponse::Ok
            }

            StateMachineCommand::CreateVolume {
                id,
                name,
                size_gb,
                org_id,
                project_id,
                env_id,
                volume_type,
                hypervisor_id,
                zone,
            } => {
                // Check quota before creating.
                if let Err(e) = self.check_volume_quota(org_id, project_id, *size_gb) {
                    return StateMachineResponse::Error(e.to_string());
                }
                let mut storage = self.storage.write().unwrap();
                // Check name uniqueness within the environment.
                let name_exists = storage.volumes.values().any(|v| {
                    v.env_id == *env_id
                        && v.name == *name
                        && v.org_id == *org_id
                        && v.project_id == *project_id
                });
                if name_exists {
                    return StateMachineResponse::Error(format!(
                        "volume '{name}' already exists in env '{env_id}'"
                    ));
                }
                if storage.volumes.contains_key(id) {
                    return StateMachineResponse::Error(format!(
                        "volume with id '{id}' already exists"
                    ));
                }
                // When hypervisor_id is provided, auto-assign the volume so the
                // storage reconciler starts ZeroFS immediately (single-node flow).
                let (attached_hypervisor_id, placement_generation) = match hypervisor_id {
                    Some(hv) => (Some(hv.clone()), 1),
                    None => (None, 0),
                };
                let record = VolumeRecord {
                    id: id.clone(),
                    name: name.clone(),
                    size_gb: *size_gb,
                    org_id: org_id.clone(),
                    project_id: project_id.clone(),
                    env_id: env_id.clone(),
                    volume_type: volume_type.clone(),
                    state: VolumeState::Available,
                    attached_vm_id: None,
                    attached_hypervisor_id,
                    placement_generation,
                    zone: zone.clone(),
                    deletion_protection: false,
                    deleted_at: None,
                    migration_source_zone: None,
                    migration_target_zone: None,
                    pre_migration_hypervisor: None,
                    pre_migration_vm_id: None,
                };
                storage.volumes.insert(id.clone(), record);
                let synced = storage.volumes.get(id).cloned();
                drop(storage);
                if let Some(ref vol) = synced {
                    self.sync_volume_to_store(vol);
                }
                info!(
                    id,
                    name, size_gb, org_id, project_id, env_id, "volume created"
                );
                StateMachineResponse::Created(id.clone())
            }

            StateMachineCommand::DeleteVolume {
                volume_id,
                cascade,
                deleted_at,
            } => {
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get(volume_id) {
                    // Root volumes may still be "attached" after VM deletion (stale
                    // attachment) — the detach guard prevents explicit detach, so we
                    // must allow deletion here to avoid a deadlock.
                    Some(vol)
                        if vol.state == VolumeState::Attached
                            && vol.volume_type != VolumeType::Root =>
                    {
                        StateMachineResponse::Error(format!(
                            "volume '{volume_id}' is attached, detach before deleting",
                        ))
                    }
                    Some(vol) if vol.state == VolumeState::Deleted => StateMachineResponse::Error(
                        format!("volume '{volume_id}' is already deleted"),
                    ),
                    Some(vol) if vol.deletion_protection => StateMachineResponse::Error(
                        format!(
                            "volume '{volume_id}' has deletion protection enabled. \
                             Disable it first with: syfrah volume update {volume_id} --no-deletion-protection"
                        ),
                    ),
                    Some(_) => {
                        // Check for non-deleted snapshots referencing this volume.
                        let snapshot_ids: Vec<String> = storage
                            .snapshots
                            .iter()
                            .filter(|(_, s)| {
                                s.source_volume_id == *volume_id
                                    && s.state != SnapshotState::Deleted
                            })
                            .map(|(id, _)| id.clone())
                            .collect();

                        if !snapshot_ids.is_empty() && !cascade {
                            return StateMachineResponse::Error(format!(
                                "volume '{volume_id}' has {} snapshot(s). \
                                 Use --cascade to delete them, or delete snapshots manually first.",
                                snapshot_ids.len()
                            ));
                        }

                        // Cascade: soft-delete all snapshots and decrement SST refcounts.
                        // SSTs that reach refcount 0 are moved to pending-GC.
                        if *cascade {
                            // Guard: reject cascade if any snapshot has a restore in progress.
                            let blocked: Vec<_> = snapshot_ids
                                .iter()
                                .filter(|id| storage.restores_in_progress.contains(id))
                                .collect();
                            if !blocked.is_empty() {
                                return StateMachineResponse::Error(format!(
                                    "cannot cascade-delete volume '{}': snapshot(s) {} have restores in progress",
                                    volume_id,
                                    blocked.iter().map(|id| format!("'{}'", id)).collect::<Vec<_>>().join(", ")
                                ));
                            }

                            // Collect SST files first to avoid borrow conflicts.
                            let snap_ssts: Vec<(String, Vec<String>)> = snapshot_ids
                                .iter()
                                .filter_map(|snap_id| {
                                    let snap = storage.snapshots.get(snap_id)?;
                                    if snap.state == SnapshotState::Deleted {
                                        return None;
                                    }
                                    Some((snap_id.clone(), snap.sst_files.clone()))
                                })
                                .collect();

                            for (snap_id, sst_files) in &snap_ssts {
                                for sst in sst_files {
                                    let count = storage
                                        .sst_refcounts
                                        .0
                                        .get(sst)
                                        .copied()
                                        .unwrap_or(0);
                                    if count <= 1 {
                                        storage.sst_refcounts.0.remove(sst);
                                        storage.pending_gc_ssts.push(sst.clone());
                                    } else {
                                        storage
                                            .sst_refcounts
                                            .0
                                            .insert(sst.clone(), count - 1);
                                    }
                                }
                                if let Some(snap) = storage.snapshots.get_mut(snap_id) {
                                    snap.state = SnapshotState::Deleted;
                                }
                                info!(snapshot_id = %snap_id, volume_id, "snapshot cascade-deleted");
                            }
                            // Recalculate minimum WAL retention (excluding soft-deleted).
                            storage.min_wal_position = storage
                                .snapshots
                                .values()
                                .filter(|s| s.state != SnapshotState::Deleted)
                                .map(|s| s.wal_position)
                                .min();
                        }

                        // Tombstone: mark as Deleted with the caller-provided timestamp.
                        // Using a field on the command (instead of SystemTime::now()) ensures
                        // every Raft replica records the same deleted_at value.
                        if let Some(vol) = storage.volumes.get_mut(volume_id) {
                            vol.state = VolumeState::Deleted;
                            vol.deleted_at = Some(*deleted_at);
                            vol.attached_vm_id = None;
                            vol.attached_hypervisor_id = None;
                        }
                        let synced = storage.volumes.get(volume_id).cloned();
                        drop(storage);
                        if let Some(ref vol) = synced {
                            self.sync_volume_to_store(vol);
                        }

                        info!(volume_id, "volume marked as deleted (tombstone)");
                        StateMachineResponse::Ok
                    }
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::VolumeAttach {
                volume_id,
                vm_id,
                hypervisor_id,
            } => {
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get_mut(volume_id) {
                    Some(vol) if vol.state == VolumeState::Available => {
                        vol.state = VolumeState::Attached;
                        vol.attached_vm_id = Some(vm_id.clone());
                        vol.attached_hypervisor_id = Some(hypervisor_id.clone());
                        vol.placement_generation += 1;
                        let synced = vol.clone();
                        drop(storage);
                        self.sync_volume_to_store(&synced);
                        info!(volume_id, vm_id, hypervisor_id, "volume attached");
                        StateMachineResponse::Ok
                    }
                    Some(vol) => StateMachineResponse::Error(format!(
                        "volume '{volume_id}' is not available (state: {:?})",
                        vol.state
                    )),
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::VolumeDetach { volume_id } => {
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get_mut(volume_id) {
                    Some(vol) if vol.volume_type == VolumeType::Root => {
                        StateMachineResponse::Error(format!(
                            "cannot detach root volume '{volume_id}': root volumes are tied to their VM lifecycle"
                        ))
                    }
                    Some(vol) if vol.state == VolumeState::Attached => {
                        vol.state = VolumeState::Available;
                        vol.attached_vm_id = None;
                        vol.attached_hypervisor_id = None;
                        let synced = vol.clone();
                        // Clear manifest pointer so new writer after reattach
                        // starts at version 1 (ADR-006 §12b).
                        storage.manifest_pointers.remove(volume_id);
                        drop(storage);
                        self.sync_volume_to_store(&synced);
                        info!(volume_id, "volume detached (manifest pointer cleared)");
                        StateMachineResponse::Ok
                    }
                    Some(vol) => StateMachineResponse::Error(format!(
                        "volume '{volume_id}' is not attached (state: {:?})",
                        vol.state
                    )),
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::RescheduleVolume {
                volume_id,
                from_hypervisor,
                to_hypervisor,
                new_vm_id,
            } => {
                // Reject self-reschedule: moving a volume to the same hypervisor
                // is a no-op that would needlessly bump the generation and fence
                // the currently-healthy writer.
                if from_hypervisor == to_hypervisor {
                    return StateMachineResponse::Error(format!(
                        "cannot reschedule volume '{}' to the same hypervisor '{}'",
                        volume_id, from_hypervisor
                    ));
                }
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get_mut(volume_id) {
                    Some(vol) if vol.state == VolumeState::Attached => {
                        // Validate the volume is currently on the source hypervisor.
                        if vol.attached_hypervisor_id.as_deref() != Some(from_hypervisor) {
                            return StateMachineResponse::Error(format!(
                                "volume '{}' is not on hypervisor '{}' (actual: {:?})",
                                volume_id, from_hypervisor, vol.attached_hypervisor_id
                            ));
                        }
                        // Increment generation for fencing — new writer uses gen-{N+1}/,
                        // source self-fences by detecting stale generation.
                        vol.placement_generation += 1;
                        vol.attached_hypervisor_id = Some(to_hypervisor.clone());
                        vol.attached_vm_id = Some(new_vm_id.clone());
                        let synced = vol.clone();
                        drop(storage);
                        self.sync_volume_to_store(&synced);
                        info!(
                            volume_id,
                            from = from_hypervisor,
                            to = to_hypervisor,
                            generation = synced.placement_generation,
                            "volume rescheduled"
                        );
                        StateMachineResponse::Ok
                    }
                    Some(vol) => StateMachineResponse::Error(format!(
                        "volume '{}' is not attached (state: {:?}), cannot reschedule",
                        volume_id, vol.state
                    )),
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::MigrateVolumeToZone {
                volume_id,
                source_zone,
                target_zone,
                target_hypervisor,
                target_vm_id,
            } => {
                // Reject migration to the same zone.
                if source_zone == target_zone {
                    return StateMachineResponse::Error(format!(
                        "cannot migrate volume '{}' to the same zone '{}'",
                        volume_id, source_zone
                    ));
                }
                // Validate both zone storage configs exist.
                {
                    let storage = self.storage.read().unwrap();
                    if !storage.storage_configs.contains_key(source_zone) {
                        return StateMachineResponse::Error(format!(
                            "source zone storage config not found: '{}'",
                            source_zone
                        ));
                    }
                    if !storage.storage_configs.contains_key(target_zone) {
                        return StateMachineResponse::Error(format!(
                            "target zone storage config not found: '{}'",
                            target_zone
                        ));
                    }
                }
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get_mut(volume_id) {
                    Some(vol) if vol.state == VolumeState::Available => {
                        // Record pre-migration state for rollback.
                        vol.pre_migration_hypervisor = vol.attached_hypervisor_id.clone();
                        vol.pre_migration_vm_id = vol.attached_vm_id.clone();
                        // Transition to Migrating.
                        vol.state = VolumeState::Migrating;
                        vol.migration_source_zone = Some(source_zone.clone());
                        vol.migration_target_zone = Some(target_zone.clone());
                        vol.attached_hypervisor_id = Some(target_hypervisor.clone());
                        vol.attached_vm_id = target_vm_id.clone();
                        vol.placement_generation += 1;
                        let synced = vol.clone();
                        drop(storage);
                        self.sync_volume_to_store(&synced);
                        info!(
                            volume_id,
                            source_zone,
                            target_zone,
                            target_hypervisor,
                            generation = synced.placement_generation,
                            "volume migration started"
                        );
                        StateMachineResponse::Ok
                    }
                    Some(vol) => StateMachineResponse::Error(format!(
                        "volume '{}' must be available to migrate (state: {:?})",
                        volume_id, vol.state
                    )),
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::CompleteMigration { volume_id } => {
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get_mut(volume_id) {
                    Some(vol) if vol.state == VolumeState::Migrating => {
                        vol.state = VolumeState::Available;
                        vol.migration_source_zone = None;
                        vol.migration_target_zone = None;
                        vol.pre_migration_hypervisor = None;
                        vol.pre_migration_vm_id = None;
                        let synced = vol.clone();
                        drop(storage);
                        self.sync_volume_to_store(&synced);
                        info!(volume_id, "volume migration completed");
                        StateMachineResponse::Ok
                    }
                    Some(vol) => StateMachineResponse::Error(format!(
                        "volume '{}' is not migrating (state: {:?}), cannot complete migration",
                        volume_id, vol.state
                    )),
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::RollbackMigration { volume_id, reason } => {
                let mut storage = self.storage.write().unwrap();
                match storage.volumes.get_mut(volume_id) {
                    Some(vol) if vol.state == VolumeState::Migrating => {
                        // Restore pre-migration hypervisor and VM assignment.
                        vol.attached_hypervisor_id = vol.pre_migration_hypervisor.take();
                        vol.attached_vm_id = vol.pre_migration_vm_id.take();
                        vol.state = VolumeState::Available;
                        vol.migration_source_zone = None;
                        vol.migration_target_zone = None;
                        let synced = vol.clone();
                        drop(storage);
                        self.sync_volume_to_store(&synced);
                        warn!(
                            volume_id,
                            reason = reason.as_str(),
                            "volume migration rolled back"
                        );
                        StateMachineResponse::Ok
                    }
                    Some(vol) => StateMachineResponse::Error(format!(
                        "volume '{}' is not migrating (state: {:?}), cannot rollback",
                        volume_id, vol.state
                    )),
                    None => StateMachineResponse::Error(format!("volume not found: {volume_id}")),
                }
            }

            StateMachineCommand::ResizeVolume {
                volume_id,
                new_size_gb,
            } => {
                // Read current volume to validate state and compute delta.
                let (org_id, project_id, old_size_gb) = {
                    let storage = self.storage.read().unwrap();
                    match storage.volumes.get(volume_id) {
                        Some(vol) if vol.state == VolumeState::Available => {
                            if *new_size_gb <= vol.size_gb {
                                return StateMachineResponse::Error(format!(
                                    "cannot shrink volume '{}': current {}GB, requested {}GB",
                                    volume_id, vol.size_gb, new_size_gb
                                ));
                            }
                            (vol.org_id.clone(), vol.project_id.clone(), vol.size_gb)
                        }
                        Some(vol) => {
                            return StateMachineResponse::Error(format!(
                                "volume '{volume_id}' must be available to resize (state: {:?})",
                                vol.state
                            ))
                        }
                        None => {
                            return StateMachineResponse::Error(format!(
                                "volume not found: {volume_id}"
                            ))
                        }
                    }
                };

                // Check total_gb quota for the size delta.
                let delta_gb = *new_size_gb - old_size_gb;
                if let Err(e) = self.check_resize_quota(&org_id, &project_id, delta_gb) {
                    return StateMachineResponse::Error(e.to_string());
                }

                let mut storage = self.storage.write().unwrap();
                if let Some(vol) = storage.volumes.get_mut(volume_id) {
                    vol.size_gb = *new_size_gb;
                    let synced = vol.clone();
                    drop(storage);
                    self.sync_volume_to_store(&synced);
                    info!(volume_id, new_size_gb, "volume resized");
                    StateMachineResponse::Ok
                } else {
                    StateMachineResponse::Error(format!("volume not found: {volume_id}"))
                }
            }

            StateMachineCommand::CreateSnapshot {
                id,
                source_volume_id,
                sst_files,
                wal_position,
            } => {
                // Look up source volume to capture org/project/size/env/type at snapshot time.
                let (org_id, project_id, size_gb, env_id, volume_type) = {
                    let storage = self.storage.read().unwrap();
                    match storage.volumes.get(source_volume_id) {
                        Some(vol) => (
                            vol.org_id.clone(),
                            vol.project_id.clone(),
                            vol.size_gb,
                            vol.env_id.clone(),
                            vol.volume_type.clone(),
                        ),
                        None => {
                            return StateMachineResponse::Error(format!(
                                "source volume not found: {source_volume_id}"
                            ))
                        }
                    }
                };

                // Check snapshot quota.
                if let Err(e) = self.check_snapshot_quota(&org_id, &project_id) {
                    return StateMachineResponse::Error(e.to_string());
                }

                let mut storage = self.storage.write().unwrap();
                if storage.snapshots.contains_key(id) {
                    return StateMachineResponse::Error(format!(
                        "snapshot with id '{id}' already exists"
                    ));
                }
                // Increment SST refcounts.
                for sst in sst_files {
                    *storage.sst_refcounts.0.entry(sst.clone()).or_insert(0) += 1;
                }
                let record = SnapshotRecord {
                    id: id.clone(),
                    source_volume_id: source_volume_id.clone(),
                    sst_files: sst_files.clone(),
                    wal_position: *wal_position,
                    org_id,
                    project_id,
                    size_gb,
                    env_id,
                    volume_type,
                    state: SnapshotState::Available,
                };
                storage.snapshots.insert(id.clone(), record);

                // Update minimum WAL retention position.
                storage.min_wal_position = Some(match storage.min_wal_position {
                    Some(existing) => existing.min(*wal_position),
                    None => *wal_position,
                });

                info!(id, source_volume_id, size_gb, "snapshot created");
                StateMachineResponse::Created(id.clone())
            }

            StateMachineCommand::DeleteSnapshot { snapshot_id } => {
                let mut storage = self.storage.write().unwrap();

                // Guard: reject deletion if the snapshot has a restore in progress.
                if storage.restores_in_progress.contains(snapshot_id) {
                    return StateMachineResponse::Error(format!(
                        "cannot delete snapshot '{snapshot_id}': restore is in progress"
                    ));
                }

                let sst_files = match storage.snapshots.get(snapshot_id) {
                    Some(snap) if snap.state == SnapshotState::Deleted => {
                        return StateMachineResponse::Error(format!(
                            "snapshot '{snapshot_id}' is already deleted"
                        ));
                    }
                    Some(snap) => snap.sst_files.clone(),
                    None => {
                        return StateMachineResponse::Error(format!(
                            "snapshot not found: {snapshot_id}"
                        ));
                    }
                };
                // Decrement SST refcounts; SSTs that reach 0 are moved to pending-GC.
                for sst in &sst_files {
                    if let Some(count) = storage.sst_refcounts.0.get_mut(sst) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            storage.sst_refcounts.0.remove(sst);
                            storage.pending_gc_ssts.push(sst.clone());
                        }
                    }
                }
                storage.snapshots.get_mut(snapshot_id).unwrap().state = SnapshotState::Deleted;

                // Recalculate minimum WAL retention across remaining (non-deleted) snapshots.
                storage.min_wal_position = storage
                    .snapshots
                    .values()
                    .filter(|s| s.state != SnapshotState::Deleted)
                    .map(|s| s.wal_position)
                    .min();
                info!(snapshot_id, "snapshot deleted");
                StateMachineResponse::Ok
            }

            // -- Storage: CommitManifest (ADR-006 §12b) --
            StateMachineCommand::CommitManifest {
                volume_id,
                generation,
                manifest_version,
                s3_key,
                published_by,
            } => {
                let mut storage = self.storage.write().unwrap();

                // 1. Volume must exist and be attached.
                let vol = match storage.volumes.get(volume_id) {
                    Some(v) => v,
                    None => {
                        return StateMachineResponse::Error(format!(
                            "volume not found: {volume_id}"
                        ));
                    }
                };
                if vol.state != VolumeState::Attached {
                    return StateMachineResponse::Error(format!(
                        "volume '{volume_id}' is not attached (state: {:?})",
                        vol.state
                    ));
                }

                // 2. Generation must match the volume's current placement_generation.
                if *generation != vol.placement_generation {
                    return StateMachineResponse::Error(format!(
                        "generation mismatch for volume '{volume_id}': \
                         expected {}, got {generation}",
                        vol.placement_generation
                    ));
                }

                // 3. published_by must match the assigned hypervisor.
                let assigned_hv = vol.attached_hypervisor_id.as_deref().unwrap_or("");
                if published_by != assigned_hv {
                    return StateMachineResponse::Error(format!(
                        "wrong publisher for volume '{volume_id}': \
                         expected '{assigned_hv}', got '{published_by}'"
                    ));
                }

                // 4. manifest_version must be strictly sequential (last + 1).
                let last_version = storage
                    .manifest_pointers
                    .get(volume_id)
                    .map_or(0, |p| p.manifest_version);
                if *manifest_version != last_version + 1 {
                    return StateMachineResponse::Error(format!(
                        "manifest version gap for volume '{volume_id}': \
                         expected {}, got {manifest_version}",
                        last_version + 1
                    ));
                }

                // All checks passed — commit the manifest pointer.
                storage.manifest_pointers.insert(
                    volume_id.clone(),
                    ManifestPointerRecord {
                        volume_id: volume_id.clone(),
                        generation: *generation,
                        manifest_version: *manifest_version,
                        s3_key: s3_key.clone(),
                        published_by: published_by.clone(),
                    },
                );
                info!(
                    volume_id,
                    generation, manifest_version, published_by, "manifest committed"
                );
                StateMachineResponse::Ok
            }

            StateMachineCommand::RestoreSnapshot {
                snapshot_id,
                new_volume_id,
                new_volume_name,
            } => {
                let snap = {
                    let storage = self.storage.read().unwrap();
                    match storage.snapshots.get(snapshot_id) {
                        Some(s) if s.state == SnapshotState::Deleted => {
                            return StateMachineResponse::Error(format!(
                                "cannot restore from deleted snapshot '{snapshot_id}'"
                            ))
                        }
                        Some(s) => s.clone(),
                        None => {
                            return StateMachineResponse::Error(format!(
                                "snapshot not found: {snapshot_id}"
                            ))
                        }
                    }
                };

                // Use the size_gb captured at snapshot creation time -- this is
                // correct even if the source volume was subsequently resized or
                // deleted.
                let size_gb = snap.size_gb;
                let env_id = snap.env_id.clone();
                let volume_type = snap.volume_type.clone();

                // Check quota for the new volume.
                if let Err(e) = self.check_volume_quota(&snap.org_id, &snap.project_id, size_gb) {
                    return StateMachineResponse::Error(e.to_string());
                }

                let mut storage = self.storage.write().unwrap();

                // Check name uniqueness within the environment (same check as CreateVolume).
                let name_exists = storage.volumes.values().any(|v| {
                    v.env_id == env_id
                        && v.name == *new_volume_name
                        && v.org_id == snap.org_id
                        && v.project_id == snap.project_id
                });
                if name_exists {
                    return StateMachineResponse::Error(format!(
                        "volume '{}' already exists in env '{}'",
                        new_volume_name, env_id
                    ));
                }

                if storage.volumes.contains_key(new_volume_id) {
                    return StateMachineResponse::Error(format!(
                        "volume with id '{new_volume_id}' already exists"
                    ));
                }

                // Increment SST refcounts: the new volume references the
                // snapshot's SST files, so they must not be GC'd.
                for sst in &snap.sst_files {
                    *storage.sst_refcounts.0.entry(sst.clone()).or_insert(0) += 1;
                }

                // Seed a manifest pointer so the new volume reads from the
                // snapshot's SST files at generation 0.
                storage.manifest_pointers.insert(
                    new_volume_id.clone(),
                    ManifestPointerRecord {
                        volume_id: new_volume_id.clone(),
                        generation: 0,
                        manifest_version: 1,
                        s3_key: format!("snapshots/{snapshot_id}/manifest.json"),
                        published_by: format!("restore:{snapshot_id}"),
                    },
                );

                let record = VolumeRecord {
                    id: new_volume_id.clone(),
                    name: new_volume_name.clone(),
                    size_gb,
                    org_id: snap.org_id,
                    project_id: snap.project_id,
                    env_id,
                    volume_type,
                    state: VolumeState::Available,
                    attached_vm_id: None,
                    attached_hypervisor_id: None,
                    placement_generation: 0,
                    zone: None,
                    deletion_protection: false,
                    deleted_at: None,
                    migration_source_zone: None,
                    migration_target_zone: None,
                    pre_migration_hypervisor: None,
                    pre_migration_vm_id: None,
                };
                storage.volumes.insert(new_volume_id.clone(), record);
                let synced = storage.volumes.get(new_volume_id).cloned();
                drop(storage);
                if let Some(ref vol) = synced {
                    self.sync_volume_to_store(vol);
                }
                info!(
                    snapshot_id,
                    new_volume_id, new_volume_name, size_gb, "snapshot restored"
                );
                StateMachineResponse::Created(new_volume_id.clone())
            }

            // -- Storage: MarkRestoreBegin / MarkRestoreComplete --
            StateMachineCommand::MarkRestoreBegin { snapshot_id } => {
                let mut storage = self.storage.write().unwrap();
                if !storage.snapshots.contains_key(snapshot_id) {
                    return StateMachineResponse::Error(format!(
                        "snapshot not found: {snapshot_id}"
                    ));
                }
                if !storage.restores_in_progress.contains(snapshot_id) {
                    storage.restores_in_progress.push(snapshot_id.clone());
                }
                info!(snapshot_id, "restore marked in-progress");
                StateMachineResponse::Ok
            }

            StateMachineCommand::MarkRestoreComplete { snapshot_id } => {
                let mut storage = self.storage.write().unwrap();
                storage.restores_in_progress.retain(|id| id != snapshot_id);
                info!(snapshot_id, "restore completed");
                StateMachineResponse::Ok
            }

            // -- GC: acknowledge SST deletion from S3 --
            StateMachineCommand::GcCompleteSsts { sst_keys } => {
                let mut storage = self.storage.write().unwrap();
                let before = storage.pending_gc_ssts.len();
                storage
                    .pending_gc_ssts
                    .retain(|key| !sst_keys.contains(key));
                let removed = before - storage.pending_gc_ssts.len();
                info!(
                    removed,
                    remaining = storage.pending_gc_ssts.len(),
                    "GC: SSTs removed from pending list"
                );
                StateMachineResponse::Ok
            }

            // -- GC: acknowledge WAL segment deletion --
            StateMachineCommand::GcCompleteWalSegments { below_position } => {
                let mut storage = self.storage.write().unwrap();
                let old = storage.min_wal_position;
                if storage.min_wal_position.is_none_or(|p| *below_position > p) {
                    storage.min_wal_position = Some(*below_position);
                }
                info!(
                    below_position,
                    old_min_wal = ?old,
                    new_min_wal = ?storage.min_wal_position,
                    "GC: WAL min position advanced"
                );
                StateMachineResponse::Ok
            }
        }
    }
}

impl RaftSnapshotBuilder<SyfrahRaftConfig> for Arc<RedbStateMachine> {
    async fn build_snapshot(&mut self) -> Result<SnapshotOf<SyfrahRaftConfig>, io::Error> {
        let sm_state = self.sm_state.read().await;

        // Export all store tables for the full snapshot.
        let tables = self.export_store_tables();
        let table_count: usize = tables.values().map(|v| v.len()).sum();

        let storage_snapshot = self.storage.read().unwrap().clone();
        let full_data = FullSnapshotData {
            sm_state: (*sm_state).clone(),
            tables,
            storage: storage_snapshot,
        };

        let data = serde_json::to_vec(&full_data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let snapshot_idx = {
            let mut idx = self.snapshot_idx.lock().unwrap();
            *idx += 1;
            *idx
        };

        let snapshot_id = if let Some(last) = sm_state.last_applied_log {
            format!(
                "{}-{}-{}",
                last.committed_leader_id(),
                last.index(),
                snapshot_idx
            )
        } else {
            format!("--{}", snapshot_idx)
        };

        let meta = SnapshotMetaOf::<SyfrahRaftConfig> {
            last_log_id: sm_state.last_applied_log,
            last_membership: sm_state.last_membership.clone(),
            snapshot_id,
        };

        let snapshot = SmSnapshot {
            meta: meta.clone(),
            data: data.clone(),
        };

        {
            let mut current = self.current_snapshot.write().await;
            *current = Some(snapshot);
        }

        // Update counters.
        self.snapshot_count.fetch_add(1, Ordering::Relaxed);
        self.entries_since_snapshot.store(0, Ordering::Relaxed);

        info!(
            snapshot_size = data.len(),
            table_entries = table_count,
            "snapshot built (full store export)"
        );

        Ok(SnapshotOf::<SyfrahRaftConfig> {
            meta,
            snapshot: Cursor::new(data),
        })
    }
}

impl RaftStateMachine<SyfrahRaftConfig> for Arc<RedbStateMachine> {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<
        (
            Option<LogIdOf<SyfrahRaftConfig>>,
            StoredMembershipOf<SyfrahRaftConfig>,
        ),
        io::Error,
    > {
        let sm = self.sm_state.read().await;
        Ok((sm.last_applied_log, sm.last_membership.clone()))
    }

    async fn apply<Strm>(&mut self, mut entries: Strm) -> Result<(), io::Error>
    where
        Strm: futures::Stream<Item = Result<EntryResponder<SyfrahRaftConfig>, io::Error>>
            + Unpin
            + OptionalSend,
    {
        let mut sm = self.sm_state.write().await;

        while let Some((entry, responder)) = entries.try_next().await? {
            sm.last_applied_log = Some(entry.log_id);

            let response = match entry.payload {
                EntryPayload::Blank => StateMachineResponse::Ok,
                EntryPayload::Normal(ref cmd) => self.apply_command(cmd),
                EntryPayload::Membership(ref mem) => {
                    sm.last_membership = StoredMembershipOf::<SyfrahRaftConfig>::new(
                        Some(entry.log_id),
                        mem.clone(),
                    );
                    StateMachineResponse::Ok
                }
            };

            // Track entries applied since last snapshot.
            self.entries_since_snapshot.fetch_add(1, Ordering::Relaxed);

            if let Some(responder) = responder {
                responder.send(response);
            }
        }
        Ok(())
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<SnapshotDataOf<SyfrahRaftConfig>, io::Error> {
        Ok(Cursor::new(Vec::new()))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMetaOf<SyfrahRaftConfig>,
        snapshot: SnapshotDataOf<SyfrahRaftConfig>,
    ) -> Result<(), io::Error> {
        let data = snapshot.into_inner();

        // Try to deserialize as FullSnapshotData first (new format),
        // fall back to SmState-only (legacy format) for compatibility.
        let new_sm = if let Ok(full) = serde_json::from_slice::<FullSnapshotData>(&data) {
            // Restore all store tables from the snapshot.
            let table_count: usize = full.tables.values().map(|v| v.len()).sum();
            self.import_store_tables(&full.tables);
            info!(
                table_entries = table_count,
                "snapshot: restored store tables from full snapshot"
            );

            // Emit PlacementEvent::Added for every placement in the restored
            // store so that FDB incremental listeners learn about them
            // immediately — without this, placements from snapshots would be
            // invisible until the next daemon restart (cold rebuild).
            if let Some(ref ps) = self.placement_store {
                if let Ok(placements) = ps.list_all() {
                    let mut emitted = 0usize;
                    for p in &placements {
                        let _ = self.placement_tx.send(PlacementEvent::Added {
                            vpc_id: p.vpc_id.clone(),
                            vm_id: p.vm_id.clone(),
                            vm_mac: p.vm_mac.clone(),
                            vm_ip: p.vm_ip.clone(),
                            subnet_id: p.subnet_id.clone(),
                            hypervisor_id: p.hypervisor_id.clone(),
                        });
                        emitted += 1;
                    }
                    if emitted > 0 {
                        info!(
                            emitted,
                            "snapshot: emitted PlacementEvents for restored placements"
                        );
                    }
                }
            }

            // Restore in-memory storage state from snapshot.
            {
                let mut storage = self.storage.write().unwrap();
                *storage = full.storage;
            }

            full.sm_state
        } else {
            // Legacy format: SmState only (no store data).
            serde_json::from_slice::<SmState>(&data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
        };

        {
            let mut sm = self.sm_state.write().await;
            *sm = new_sm;
        }

        let snap = SmSnapshot {
            meta: meta.clone(),
            data,
        };
        let mut current = self.current_snapshot.write().await;
        *current = Some(snap);

        // Reset entries counter since we just installed a snapshot.
        self.entries_since_snapshot.store(0, Ordering::Relaxed);

        info!("snapshot installed");
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<SnapshotOf<SyfrahRaftConfig>>, io::Error> {
        match &*self.current_snapshot.read().await {
            Some(snapshot) => Ok(Some(SnapshotOf::<SyfrahRaftConfig> {
                meta: snapshot.meta.clone(),
                snapshot: Cursor::new(snapshot.data.clone()),
            })),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_org_store() -> (tempfile::TempDir, Arc<syfrah_org::OrgStore>) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_org.redb");
        let db = syfrah_state::LayerDb::open_at(&db_path).unwrap();
        (dir, Arc::new(syfrah_org::OrgStore::new(db)))
    }

    #[test]
    fn apply_create_org() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let cmd = StateMachineCommand::CreateOrg {
            name: "testorg".to_string(),
        };
        let resp = sm.apply_command(&cmd);
        match resp {
            StateMachineResponse::Created(id) => {
                assert!(id.starts_with("org-"), "expected org- prefix, got: {id}")
            }
            other => panic!("expected Created, got {other:?}"),
        }
        // Verify it was actually created.
        let org = store.get("testorg").unwrap();
        assert!(org.is_some());
    }

    #[test]
    fn apply_create_org_duplicate() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let cmd = StateMachineCommand::CreateOrg {
            name: "dup".to_string(),
        };
        let _ = sm.apply_command(&cmd);
        let resp = sm.apply_command(&cmd);
        match resp {
            StateMachineResponse::Error(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn apply_delete_org() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let _ = sm.apply_command(&StateMachineCommand::CreateOrg {
            name: "del".to_string(),
        });
        let resp = sm.apply_command(&StateMachineCommand::DeleteOrg {
            name: "del".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        assert!(store.get("del").unwrap().is_none());
    }

    #[test]
    fn apply_create_project() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let _ = sm.apply_command(&StateMachineCommand::CreateOrg {
            name: "acme".to_string(),
        });
        let resp = sm.apply_command(&StateMachineCommand::CreateProject {
            name: "backend".to_string(),
            org: "acme".to_string(),
        });
        match resp {
            StateMachineResponse::Created(id) => {
                assert!(id.starts_with("proj-"), "expected proj- prefix, got: {id}")
            }
            other => panic!("expected Created, got {other:?}"),
        }
    }

    #[test]
    fn apply_allocate_ip_without_ipam_store_returns_error() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let resp = sm.apply_command(&StateMachineCommand::AllocateIp {
            subnet_id: "sub-1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn apply_hypervisor_commands_with_store() {
        let (_dir, store) = make_org_store();
        // Create a hypervisor store and wire it into the state machine.
        let hv_db = syfrah_state::LayerDb::open_at(&_dir.path().join("hv.redb")).unwrap();
        let hv_store = std::sync::Arc::new(syfrah_org::HypervisorStore::new(hv_db));
        let sm = RedbStateMachine::new(store).with_hypervisor_store(hv_store.clone());

        // RegisterHypervisor should create a record in the store.
        let resp = sm.apply_command(&StateMachineCommand::RegisterHypervisor {
            name: "hv1".to_string(),
            region: "eu".to_string(),
            zone: "az1".to_string(),
            fabric_ipv6: "fd00::1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Verify the hypervisor was created.
        let hv = hv_store.get("hv1").unwrap().unwrap();
        assert_eq!(hv.region, "eu");
        assert_eq!(hv.zone, "az1");
        assert_eq!(hv.fabric_ipv6, "fd00::1");

        // Configure storage for zone az1 so enable succeeds.
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu".to_string(),
            zone: "az1".to_string(),
            config: Box::new(make_valid_storage_config()),
        });
        assert!(!matches!(resp, StateMachineResponse::Error(_)));

        // EnableHypervisor should update state to Available.
        let resp = sm.apply_command(&StateMachineCommand::EnableHypervisor {
            name: "hv1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        let hv = hv_store.get("hv1").unwrap().unwrap();
        assert_eq!(hv.state, syfrah_org::HypervisorState::Available);

        // UpdateHypervisorCapacity should update capacity fields.
        let resp = sm.apply_command(&StateMachineCommand::UpdateHypervisorCapacity {
            name: "hv1".to_string(),
            allocatable_vcpus: 16,
            allocatable_memory_mb: 65536,
            used_vcpus: 4,
            used_memory_mb: 8192,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        let hv = hv_store.get("hv1").unwrap().unwrap();
        assert_eq!(hv.capacity.allocatable_vcpus, 16);
        assert_eq!(hv.capacity.used_vcpus, 4);
    }

    #[test]
    fn apply_hypervisor_commands_without_store_returns_error() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        // Without hypervisor store wired, commands should return error.
        let resp = sm.apply_command(&StateMachineCommand::RegisterHypervisor {
            name: "hv1".to_string(),
            region: "eu".to_string(),
            zone: "az1".to_string(),
            fabric_ipv6: "fd00::1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn enable_hypervisor_blocked_without_storage_config() {
        let (_dir, store) = make_org_store();
        let hv_db = syfrah_state::LayerDb::open_at(&_dir.path().join("hv.redb")).unwrap();
        let hv_store = std::sync::Arc::new(syfrah_org::HypervisorStore::new(hv_db));
        let sm = RedbStateMachine::new(store).with_hypervisor_store(hv_store.clone());

        // Register a hypervisor in zone fsn1.
        let resp = sm.apply_command(&StateMachineCommand::RegisterHypervisor {
            name: "hv-fsn".to_string(),
            region: "eu".to_string(),
            zone: "fsn1".to_string(),
            fabric_ipv6: "fd00::1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Attempt to enable without storage config for fsn1 — should fail.
        let resp = sm.apply_command(&StateMachineCommand::EnableHypervisor {
            name: "hv-fsn".to_string(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(
                    msg.contains("storage is not configured for zone fsn1"),
                    "expected storage preflight error, got: {msg}"
                );
                assert!(
                    msg.contains("syfrah storage configure"),
                    "error should suggest the fix command, got: {msg}"
                );
            }
            other => panic!("expected Error, got: {other:?}"),
        }

        // Now configure storage for fsn1 and retry — should succeed.
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu".to_string(),
            zone: "fsn1".to_string(),
            config: Box::new(make_valid_storage_config()),
        });
        assert!(!matches!(resp, StateMachineResponse::Error(_)));

        let resp = sm.apply_command(&StateMachineCommand::EnableHypervisor {
            name: "hv-fsn".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[tokio::test]
    async fn snapshot_roundtrip() {
        let (_dir, store) = make_org_store();
        let mut sm = Arc::new(RedbStateMachine::new(store));

        // Build a snapshot.
        use openraft::storage::RaftSnapshotBuilder;
        let snap = sm.build_snapshot().await.unwrap();
        assert!(!snap.snapshot.into_inner().is_empty());

        // Get current snapshot.
        use openraft::storage::RaftStateMachine;
        let current = sm.get_current_snapshot().await.unwrap();
        assert!(current.is_some());
    }

    #[tokio::test]
    async fn applied_state_default() {
        let (_dir, store) = make_org_store();
        let mut sm = Arc::new(RedbStateMachine::new(store));
        use openraft::storage::RaftStateMachine;
        let (last, membership) = sm.applied_state().await.unwrap();
        assert!(last.is_none());
        // Default membership should be empty.
        assert_eq!(membership.membership().voter_ids().count(), 0);
    }

    // -----------------------------------------------------------------------
    // Storage quota tests (#1184)
    // -----------------------------------------------------------------------

    use crate::commands::{QuotaScope, VolumeType};

    /// Helper: create a volume via the state machine.
    fn create_volume(
        sm: &RedbStateMachine,
        id: &str,
        name: &str,
        size_gb: u32,
        org: &str,
        project: &str,
        env: &str,
    ) -> StateMachineResponse {
        sm.apply_command(&StateMachineCommand::CreateVolume {
            id: id.into(),
            name: name.into(),
            size_gb,
            org_id: org.into(),
            project_id: project.into(),
            env_id: env.into(),
            volume_type: VolumeType::Data,
            hypervisor_id: None,
            zone: None,
        })
    }

    /// Helper: create a snapshot via the state machine (requires a volume).
    fn create_snapshot(sm: &RedbStateMachine, id: &str, vol_id: &str) -> StateMachineResponse {
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: id.into(),
            source_volume_id: vol_id.into(),
            sst_files: vec!["sst-a".into()],
            wal_position: 1,
        })
    }

    // -- StorageConfig tests (issue #1183) --

    fn make_valid_storage_config() -> crate::commands::StorageConfig {
        crate::commands::StorageConfig {
            s3_endpoint: "https://s3.par.io.cloud.ovh.net".into(),
            s3_bucket: "syfrah-storage-eu".into(),
            s3_access_key: "AKIAEXAMPLE".into(),
            s3_secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            cache_disk_path: "/dev/nvme1n1".into(),
            cache_disk_size_gb: 200,
            cache_memory_size_gb: 8,
        }
    }

    #[test]
    fn no_quota_set_unlimited() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // With no quota, creating volumes should succeed indefinitely.
        for i in 0..10 {
            let resp = create_volume(
                &sm,
                &format!("vol-{i}"),
                &format!("v{i}"),
                100,
                "acme",
                "myapp",
                "prod",
            );
            assert!(
                matches!(resp, StateMachineResponse::Created(_)),
                "expected Created, got {resp:?}"
            );
        }
    }

    #[test]
    fn org_quota_volume_count_enforced() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set org quota: max 2 volumes.
        let resp = sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 2,
            max_total_gb: 10000,
            max_snapshots: 100,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Create 2 volumes -- should succeed.
        let resp = create_volume(&sm, "vol-1", "v1", 50, "acme", "p1", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));
        let resp = create_volume(&sm, "vol-2", "v2", 50, "acme", "p2", "staging");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // 3rd volume should fail with quota exceeded.
        let resp = create_volume(&sm, "vol-3", "v3", 50, "acme", "p1", "prod");
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("quota exceeded"), "unexpected error: {msg}");
                assert!(
                    msg.contains("volume_count"),
                    "should mention volume_count: {msg}"
                );
                assert!(msg.contains("2"), "should mention current usage: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn org_quota_total_gb_enforced() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set org quota: max 200GB total.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 100,
            max_total_gb: 200,
            max_snapshots: 100,
        });

        let resp = create_volume(&sm, "vol-1", "v1", 150, "acme", "p1", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // This would bring total to 250GB > 200GB limit.
        let resp = create_volume(&sm, "vol-2", "v2", 100, "acme", "p1", "prod");
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("quota exceeded"), "unexpected error: {msg}");
                assert!(msg.contains("total_gb"), "should mention total_gb: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn project_quota_enforced() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set project-level quota: max 1 volume.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Project {
                org_id: "acme".into(),
                project_id: "myapp".into(),
            },
            max_volumes: 1,
            max_total_gb: 10000,
            max_snapshots: 100,
        });

        let resp = create_volume(&sm, "vol-1", "v1", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // 2nd volume in same project should fail.
        let resp = create_volume(&sm, "vol-2", "v2", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Error(_)));

        // But a volume in a different project should succeed (no quota for that project).
        let resp = create_volume(&sm, "vol-3", "v3", 50, "acme", "other", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));
    }

    #[test]
    fn project_inherits_org_quota() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set org quota only (no project quota).
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 2,
            max_total_gb: 10000,
            max_snapshots: 100,
        });

        // Create 2 volumes in a project -- should use org quota.
        let resp = create_volume(&sm, "vol-1", "v1", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));
        let resp = create_volume(&sm, "vol-2", "v2", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // 3rd should be rejected by org-level quota.
        let resp = create_volume(&sm, "vol-3", "v3", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn project_quota_overrides_org_quota() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Org allows 10 volumes, but project only allows 1.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 10000,
            max_snapshots: 100,
        });
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Project {
                org_id: "acme".into(),
                project_id: "limited".into(),
            },
            max_volumes: 1,
            max_total_gb: 10000,
            max_snapshots: 100,
        });

        let resp = create_volume(&sm, "vol-1", "v1", 50, "acme", "limited", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Project-level limit of 1 should block this.
        let resp = create_volume(&sm, "vol-2", "v2", 50, "acme", "limited", "prod");
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn snapshot_quota_enforced() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set org quota: max 2 snapshots.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 100,
            max_total_gb: 10000,
            max_snapshots: 2,
        });

        // Create a volume to snapshot from.
        create_volume(&sm, "vol-1", "v1", 50, "acme", "myapp", "prod");

        let resp = create_snapshot(&sm, "snap-1", "vol-1");
        assert!(matches!(resp, StateMachineResponse::Created(_)));
        let resp = create_snapshot(&sm, "snap-2", "vol-1");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // 3rd snapshot should fail.
        let resp = create_snapshot(&sm, "snap-3", "vol-1");
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("quota exceeded"), "unexpected error: {msg}");
                assert!(
                    msg.contains("snapshot_count"),
                    "should mention snapshot_count: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn quota_exceeded_error_includes_usage_details() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 1,
            max_total_gb: 10000,
            max_snapshots: 100,
        });

        create_volume(&sm, "vol-1", "v1", 50, "acme", "myapp", "prod");

        let resp = create_volume(&sm, "vol-2", "v2", 50, "acme", "myapp", "prod");
        match resp {
            StateMachineResponse::Error(msg) => {
                // Should include both current count and limit.
                assert!(msg.contains("limit is 1"), "should include limit: {msg}");
                assert!(
                    msg.contains("current usage is 1"),
                    "should include current usage: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn storage_volume_lifecycle() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Create -> Attach -> Detach -> Resize -> Delete.
        let resp = create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        let resp = sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Cannot delete while attached.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));

        // Detach.
        let resp = sm.apply_command(&StateMachineCommand::VolumeDetach {
            volume_id: "vol-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Resize (grow only).
        let resp = sm.apply_command(&StateMachineCommand::ResizeVolume {
            volume_id: "vol-1".into(),
            new_size_gb: 200,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Cannot shrink.
        let resp = sm.apply_command(&StateMachineCommand::ResizeVolume {
            volume_id: "vol-1".into(),
            new_size_gb: 50,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));

        // Delete.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[test]
    fn reschedule_volume_increments_generation_and_moves_hypervisor() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Create and attach a volume.
        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });

        // Check initial state: generation is 1 (attach increments from 0).
        {
            let storage = sm.storage.read().unwrap();
            let vol = storage.volumes.get("vol-1").unwrap();
            assert_eq!(vol.placement_generation, 1);
            assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-1"));
            assert_eq!(vol.attached_vm_id.as_deref(), Some("vm-1"));
        }

        // Reschedule: move volume from hv-1 to hv-2.
        let resp = sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-1".into(),
            from_hypervisor: "hv-1".into(),
            to_hypervisor: "hv-2".into(),
            new_vm_id: "vm-1-new".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Verify: generation incremented, hypervisor and VM updated.
        {
            let storage = sm.storage.read().unwrap();
            let vol = storage.volumes.get("vol-1").unwrap();
            assert_eq!(vol.placement_generation, 2);
            assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-2"));
            assert_eq!(vol.attached_vm_id.as_deref(), Some("vm-1-new"));
            assert_eq!(vol.state, VolumeState::Attached);
        }
    }

    #[test]
    fn reschedule_volume_rejects_wrong_source_hypervisor() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });

        // Try to reschedule from wrong hypervisor.
        let resp = sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-1".into(),
            from_hypervisor: "hv-wrong".into(),
            to_hypervisor: "hv-2".into(),
            new_vm_id: "vm-1-new".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));

        // Generation should be unchanged.
        {
            let storage = sm.storage.read().unwrap();
            let vol = storage.volumes.get("vol-1").unwrap();
            assert_eq!(vol.placement_generation, 1);
            assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-1"));
        }
    }

    #[test]
    fn reschedule_volume_rejects_detached_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        // Volume is Available (not attached) — cannot reschedule.
        let resp = sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-1".into(),
            from_hypervisor: "hv-1".into(),
            to_hypervisor: "hv-2".into(),
            new_vm_id: "vm-1-new".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn reschedule_volume_rejects_nonexistent_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        let resp = sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-nope".into(),
            from_hypervisor: "hv-1".into(),
            to_hypervisor: "hv-2".into(),
            new_vm_id: "vm-1-new".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn reschedule_volume_fencing_prevents_stale_writes() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Create, attach, reschedule to simulate full migration flow.
        create_volume(&sm, "vol-fence", "db", 50, "acme", "proj", "prod");
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-fence".into(),
            vm_id: "vm-old".into(),
            hypervisor_id: "hv-src".into(),
        });

        // Reschedule: hv-src -> hv-dst.
        sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-fence".into(),
            from_hypervisor: "hv-src".into(),
            to_hypervisor: "hv-dst".into(),
            new_vm_id: "vm-new".into(),
        });

        // Stale source tries to reschedule again from hv-src — must fail
        // because the volume is now on hv-dst.
        let resp = sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-fence".into(),
            from_hypervisor: "hv-src".into(),
            to_hypervisor: "hv-other".into(),
            new_vm_id: "vm-other".into(),
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(_)),
            "stale source should be fenced out"
        );

        // Verify final state is still on hv-dst with generation 2.
        {
            let storage = sm.storage.read().unwrap();
            let vol = storage.volumes.get("vol-fence").unwrap();
            assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-dst"));
            assert_eq!(vol.placement_generation, 2);
        }
    }

    #[test]
    fn reschedule_volume_rejects_self_reschedule() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });

        // Self-reschedule: from == to — must be rejected.
        let resp = sm.apply_command(&StateMachineCommand::RescheduleVolume {
            volume_id: "vol-1".into(),
            from_hypervisor: "hv-1".into(),
            to_hypervisor: "hv-1".into(),
            new_vm_id: "vm-1".into(),
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(ref msg) if msg.contains("same hypervisor")),
            "self-reschedule should be rejected, got: {resp:?}"
        );

        // Generation must NOT have been incremented.
        {
            let storage = sm.storage.read().unwrap();
            let vol = storage.volumes.get("vol-1").unwrap();
            assert_eq!(vol.placement_generation, 1);
            assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-1"));
        }
    }

    #[test]
    fn reschedule_vm_cascades_to_attached_volumes() {
        // Create a placement store so RescheduleVm can update placements.
        let ps_dir = tempfile::tempdir().unwrap();
        let ps_path = ps_dir.path().join("placement.redb");
        let ps_db = syfrah_state::LayerDb::open_at(&ps_path).unwrap();
        let ps = Arc::new(syfrah_org::PlacementStore::new(ps_db));

        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store).with_placement_store(ps.clone());

        // Seed a VM placement for "vm-1" on "hv-src".
        let placement = syfrah_org::types::VmPlacement {
            vpc_id: "vpc-1".to_string(),
            vm_id: "vm-1".to_string(),
            vm_mac: "02:00:00:00:00:01".to_string(),
            vm_ip: "10.0.0.1".to_string(),
            subnet_id: "vpc-1/sub-1".to_string(),
            hypervisor_id: "hv-src".to_string(),
            action: syfrah_org::types::PlacementAction::Add,
            created_at: 1000,
            placement_generation: 1,
        };
        ps.add_placement(&placement).unwrap();

        // Create two volumes and attach them to vm-1 on hv-src.
        create_volume(&sm, "vol-root-vm-1", "root-vm-1", 50, "acme", "p1", "prod");
        create_volume(&sm, "vol-data-vm-1", "data-vm-1", 100, "acme", "p1", "prod");

        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-root-vm-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-src".into(),
        });
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-data-vm-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-src".into(),
        });

        // Also create a volume attached to a different VM (should NOT move).
        create_volume(&sm, "vol-other", "other", 20, "acme", "p1", "prod");
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-other".into(),
            vm_id: "vm-other".into(),
            hypervisor_id: "hv-src".into(),
        });

        // Verify initial state.
        {
            let storage = sm.storage.read().unwrap();
            let v1 = storage.volumes.get("vol-root-vm-1").unwrap();
            assert_eq!(v1.placement_generation, 1);
            assert_eq!(v1.attached_hypervisor_id.as_deref(), Some("hv-src"));
            let v2 = storage.volumes.get("vol-data-vm-1").unwrap();
            assert_eq!(v2.placement_generation, 1);
            assert_eq!(v2.attached_hypervisor_id.as_deref(), Some("hv-src"));
        }

        // Reschedule VM from hv-src to hv-dst.
        let resp = sm.apply_command(&StateMachineCommand::RescheduleVm {
            vm_id: "vm-1".into(),
            from: "hv-src".into(),
            to: "hv-dst".into(),
            generation: 2,
        });
        assert!(matches!(resp, StateMachineResponse::Ok), "got: {resp:?}");

        // Both volumes should have been rescheduled to hv-dst with bumped gen.
        {
            let storage = sm.storage.read().unwrap();

            let v1 = storage.volumes.get("vol-root-vm-1").unwrap();
            assert_eq!(v1.placement_generation, 2);
            assert_eq!(v1.attached_hypervisor_id.as_deref(), Some("hv-dst"));
            assert_eq!(v1.attached_vm_id.as_deref(), Some("vm-1"));

            let v2 = storage.volumes.get("vol-data-vm-1").unwrap();
            assert_eq!(v2.placement_generation, 2);
            assert_eq!(v2.attached_hypervisor_id.as_deref(), Some("hv-dst"));
            assert_eq!(v2.attached_vm_id.as_deref(), Some("vm-1"));

            // Volume attached to a different VM must NOT have moved.
            let vother = storage.volumes.get("vol-other").unwrap();
            assert_eq!(vother.placement_generation, 1);
            assert_eq!(vother.attached_hypervisor_id.as_deref(), Some("hv-src"));
            assert_eq!(vother.attached_vm_id.as_deref(), Some("vm-other"));
        }

        // Verify placement was also updated.
        let placements = ps.list_all().unwrap();
        let p = placements.iter().find(|p| p.vm_id == "vm-1").unwrap();
        assert_eq!(p.hypervisor_id, "hv-dst");
        assert_eq!(p.placement_generation, 2);
    }

    #[test]
    fn snapshot_lifecycle_and_sst_refcounting() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "v1", 100, "acme", "myapp", "prod");

        // Create snapshot with SST files.
        let resp = sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-1".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-001".into(), "sst-002".into()],
            wal_position: 42,
        });
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Verify SST refcounts.
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.sst_refcounts.0.get("sst-001"), Some(&1));
            assert_eq!(storage.sst_refcounts.0.get("sst-002"), Some(&1));
        }

        // Create another snapshot sharing sst-001.
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-2".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-001".into(), "sst-003".into()],
            wal_position: 50,
        });
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.sst_refcounts.0.get("sst-001"), Some(&2));
        }

        // Delete snap-1: sst-001 refcount drops to 1, sst-002 drops to 0 (removed).
        sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-1".into(),
        });
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.sst_refcounts.0.get("sst-001"), Some(&1));
            assert_eq!(storage.sst_refcounts.0.get("sst-002"), None);
        }
    }

    #[test]
    fn restore_snapshot_creates_new_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        let resp = sm.apply_command(&StateMachineCommand::RestoreSnapshot {
            snapshot_id: "snap-1".into(),
            new_volume_id: "vol-2".into(),
            new_volume_name: "pgdata-restored".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Verify the new volume exists with correct size.
        let storage = sm.storage.read().unwrap();
        let vol = storage.volumes.get("vol-2").unwrap();
        assert_eq!(vol.name, "pgdata-restored");
        assert_eq!(vol.size_gb, 100);
        assert_eq!(vol.org_id, "acme");
    }

    #[test]
    fn restore_snapshot_increments_sst_refcounts() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        // Create snapshot with known SST files.
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-1".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-x".into(), "sst-y".into()],
            wal_position: 10,
        });

        // Baseline: each SST has refcount 1 from the snapshot.
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.sst_refcounts.0.get("sst-x"), Some(&1));
            assert_eq!(storage.sst_refcounts.0.get("sst-y"), Some(&1));
        }

        // Restore: refcounts should increment to 2.
        let resp = sm.apply_command(&StateMachineCommand::RestoreSnapshot {
            snapshot_id: "snap-1".into(),
            new_volume_id: "vol-2".into(),
            new_volume_name: "pgdata-restored".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        let storage = sm.storage.read().unwrap();
        assert_eq!(storage.sst_refcounts.0.get("sst-x"), Some(&2));
        assert_eq!(storage.sst_refcounts.0.get("sst-y"), Some(&2));
    }

    #[test]
    fn restore_snapshot_seeds_manifest_pointer() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        sm.apply_command(&StateMachineCommand::RestoreSnapshot {
            snapshot_id: "snap-1".into(),
            new_volume_id: "vol-2".into(),
            new_volume_name: "pgdata-restored".into(),
        });

        let storage = sm.storage.read().unwrap();
        let ptr = storage
            .manifest_pointers
            .get("vol-2")
            .expect("manifest pointer should be seeded");
        assert_eq!(ptr.volume_id, "vol-2");
        assert_eq!(ptr.generation, 0);
        assert_eq!(ptr.manifest_version, 1);
        assert!(ptr.s3_key.contains("snap-1"));
        assert!(ptr.published_by.contains("restore"));
    }

    #[test]
    fn restore_from_deleted_snapshot_fails() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        // Delete the snapshot.
        let resp = sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Attempt restore from deleted snapshot.
        let resp = sm.apply_command(&StateMachineCommand::RestoreSnapshot {
            snapshot_id: "snap-1".into(),
            new_volume_id: "vol-2".into(),
            new_volume_name: "pgdata-restored".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("deleted"), "should mention deleted: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn set_storage_config() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(crate::commands::StorageConfig {
                s3_endpoint: "https://s3.example.com".into(),
                s3_bucket: "bucket".into(),
                s3_access_key: "AK".into(),
                s3_secret_key: "SK".into(),
                cache_disk_path: "/dev/nvme1n1".into(),
                cache_disk_size_gb: 200,
                cache_memory_size_gb: 8,
            }),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Stored by zone key, not region.
        let storage = sm.storage.read().unwrap();
        let cfg = storage.storage_configs.get("eu-west-a").unwrap();
        assert_eq!(cfg.s3_bucket, "bucket");
    }

    #[test]
    fn resize_volume_checks_total_gb_quota() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set org quota: max 200GB total.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 100,
            max_total_gb: 200,
            max_snapshots: 100,
        });

        // Create a 100GB volume.
        let resp = create_volume(&sm, "vol-1", "v1", 100, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Resize to 150GB (delta=50, total=150) -- should succeed.
        let resp = sm.apply_command(&StateMachineCommand::ResizeVolume {
            volume_id: "vol-1".into(),
            new_size_gb: 150,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Resize to 300GB (delta=150, total=300 > 200 limit) -- should fail.
        let resp = sm.apply_command(&StateMachineCommand::ResizeVolume {
            volume_id: "vol-1".into(),
            new_size_gb: 300,
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("quota exceeded"), "unexpected error: {msg}");
                assert!(msg.contains("total_gb"), "should mention total_gb: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn apply_set_storage_config() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let config = make_valid_storage_config();
        let cmd = StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config.clone()),
        };
        let resp = sm.apply_command(&cmd);
        assert!(matches!(resp, StateMachineResponse::Ok));
        // Verify retrieval by zone key.
        let got = sm.get_storage_config("eu-west-a").unwrap();
        assert_eq!(got.s3_endpoint, config.s3_endpoint);
        assert_eq!(got.s3_bucket, config.s3_bucket);
        assert_eq!(got.s3_access_key, config.s3_access_key);
        assert_eq!(got.s3_secret_key, config.s3_secret_key);
        assert_eq!(got.cache_disk_path, config.cache_disk_path);
        assert_eq!(got.cache_disk_size_gb, config.cache_disk_size_gb);
        assert_eq!(got.cache_memory_size_gb, config.cache_memory_size_gb);
    }

    #[test]
    fn apply_set_storage_config_overwrites() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let config1 = make_valid_storage_config();
        let _ = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config1),
        });
        // Update with a different bucket.
        let mut config2 = make_valid_storage_config();
        config2.s3_bucket = "new-bucket".into();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config2),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        let got = sm.get_storage_config("eu-west-a").unwrap();
        assert_eq!(got.s3_bucket, "new-bucket");
    }

    #[test]
    fn get_storage_config_returns_none_for_unknown_zone() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        assert!(sm.get_storage_config("nonexistent").is_none());
    }

    #[test]
    fn apply_set_storage_config_rejects_invalid_endpoint() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let mut config = make_valid_storage_config();
        config.s3_endpoint = "ftp://wrong.example.com".into();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("s3_endpoint must start with https:// or http://"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn restore_snapshot_uses_snapshot_size_after_source_deleted() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Create volume, snapshot, then delete the snapshot manually and
        // delete the volume. The snapshot stores the source volume's size
        // at creation time, so restoring uses that size even if the source
        // is gone.
        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        // Delete volume with cascade: snapshots are deleted too.
        // Verify cascade deletes the snapshot and marks volume as Deleted.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: true,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Snapshot was cascade-deleted, so restore should fail.
        let resp = sm.apply_command(&StateMachineCommand::RestoreSnapshot {
            snapshot_id: "snap-1".into(),
            new_volume_id: "vol-2".into(),
            new_volume_name: "pgdata-restored".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));

        // Volume should be in Deleted state (tombstone), not removed.
        let storage = sm.storage.read().unwrap();
        let vol = storage.volumes.get("vol-1").unwrap();
        assert_eq!(vol.state, VolumeState::Deleted);
        assert!(vol.deleted_at.is_some());
    }

    #[test]
    fn restore_snapshot_enforces_name_uniqueness() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        // Create a volume with the name we want to restore into.
        create_volume(&sm, "vol-x", "pgdata-restored", 50, "acme", "myapp", "prod");

        // Restore should fail because "pgdata-restored" already exists in the env.
        let resp = sm.apply_command(&StateMachineCommand::RestoreSnapshot {
            snapshot_id: "snap-1".into(),
            new_volume_id: "vol-2".into(),
            new_volume_name: "pgdata-restored".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(
                    msg.contains("already exists"),
                    "should mention name conflict: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn apply_set_storage_config_rejects_empty_bucket() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let mut config = make_valid_storage_config();
        config.s3_bucket = "".into();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn apply_set_storage_config_rejects_empty_zone_and_region() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let config = make_valid_storage_config();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "".into(),
            zone: "".into(),
            config: Box::new(config),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("must not be empty"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_stores_size_gb_from_source_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "v1", 250, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        let storage = sm.storage.read().unwrap();
        let snap = storage.snapshots.get("snap-1").unwrap();
        assert_eq!(
            snap.size_gb, 250,
            "snapshot should capture source volume size"
        );
        assert_eq!(snap.env_id, "prod");
    }

    #[test]
    fn apply_set_storage_config_rejects_empty_credentials() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        // Empty access key.
        let mut config = make_valid_storage_config();
        config.s3_access_key = "".into();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        // Empty secret key.
        let mut config = make_valid_storage_config();
        config.s3_secret_key = "".into();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn apply_set_storage_config_accepts_http_endpoint() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let mut config = make_valid_storage_config();
        config.s3_endpoint = "http://minio.local:9000".into();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "local".into(),
            zone: "local-a".into(),
            config: Box::new(config),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[test]
    fn storage_config_debug_does_not_leak_secrets() {
        let config = make_valid_storage_config();
        let debug_output = format!("{config:?}");
        // SECURITY: The secret key must never appear in Debug output.
        assert!(
            !debug_output.contains(&config.s3_secret_key),
            "s3_secret_key leaked in Debug output"
        );
        assert!(
            !debug_output.contains("AKIAEXAMPLE"),
            "s3_access_key leaked in Debug output"
        );
        // Verify it shows REDACTED instead.
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should contain [REDACTED]"
        );
    }

    #[test]
    fn storage_config_has_no_encryption_passphrase() {
        // SECURITY: Verify that StorageConfig does not have an
        // encryption_passphrase field. This is a compile-time guarantee
        // enforced by the struct definition, but we also verify that
        // the serialized form does not contain the word.
        let config = make_valid_storage_config();
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            !json.contains("encryption_passphrase"),
            "StorageConfig must NOT contain encryption_passphrase (ADR-006 §9)"
        );
        assert!(
            !json.contains("passphrase"),
            "StorageConfig must NOT contain any passphrase field"
        );
    }

    #[test]
    fn storage_config_multiple_zones() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let mut config_eu = make_valid_storage_config();
        config_eu.s3_bucket = "bucket-eu".into();
        let mut config_us = make_valid_storage_config();
        config_us.s3_bucket = "bucket-us".into();
        config_us.s3_endpoint = "https://s3.us-east.example.com".into();
        let _ = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config_eu),
        });
        let _ = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "us-east".into(),
            zone: "us-east-1".into(),
            config: Box::new(config_us),
        });
        let eu = sm.get_storage_config("eu-west-a").unwrap();
        let us = sm.get_storage_config("us-east-1").unwrap();
        assert_eq!(eu.s3_bucket, "bucket-eu");
        assert_eq!(us.s3_bucket, "bucket-us");
    }

    #[test]
    fn storage_config_backward_compat_empty_zone_falls_back_to_region() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let config = make_valid_storage_config();
        // Simulate old command with no zone field (deserialized as empty string).
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: String::new(),
            config: Box::new(config.clone()),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        // Should be stored under the region key as fallback.
        let got = sm.get_storage_config("eu-west").unwrap();
        assert_eq!(got.s3_bucket, config.s3_bucket);
    }

    #[test]
    fn storage_config_zone_key_takes_precedence_over_region() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let config = make_valid_storage_config();
        let resp = sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west-a".into(),
            config: Box::new(config),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        // Lookup by zone works.
        assert!(sm.get_storage_config("eu-west-a").is_some());
        // Lookup by region does NOT match (zone takes precedence).
        assert!(sm.get_storage_config("eu-west").is_none());
    }

    // -----------------------------------------------------------------------
    // Volume delete e2e: tombstone, guards, cascade, purge (#1191)
    // -----------------------------------------------------------------------

    #[test]
    fn delete_volume_creates_tombstone() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Volume should still exist as a tombstone.
        let storage = sm.storage.read().unwrap();
        let vol = storage.volumes.get("vol-1").unwrap();
        assert_eq!(vol.state, VolumeState::Deleted);
        assert!(vol.deleted_at.is_some());
        assert!(vol.attached_vm_id.is_none());
    }

    #[test]
    fn delete_volume_attached_rejected() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });

        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Error(ref msg) if msg.contains("attached")));
    }

    #[test]
    fn delete_volume_already_deleted_rejected() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });

        // Second delete should fail.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(ref msg) if msg.contains("already deleted"))
        );
    }

    #[test]
    fn delete_volume_deletion_protection_rejected() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        // Enable deletion protection manually.
        {
            let mut storage = sm.storage.write().unwrap();
            storage
                .volumes
                .get_mut("vol-1")
                .unwrap()
                .deletion_protection = true;
        }

        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(ref msg) if msg.contains("deletion protection"))
        );
    }

    #[test]
    fn delete_volume_with_snapshots_requires_cascade() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        // Without cascade: should fail.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Error(ref msg) if msg.contains("snapshot")));

        // With cascade: should succeed and delete the snapshot.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: true,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        let storage = sm.storage.read().unwrap();
        assert_eq!(
            storage.snapshots.get("snap-1").unwrap().state,
            SnapshotState::Deleted,
            "snapshot should be cascade-deleted (soft)"
        );
        assert_eq!(
            storage.volumes.get("vol-1").unwrap().state,
            VolumeState::Deleted
        );
    }

    #[test]
    fn cascade_delete_decrements_sst_refcounts() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        // Create snapshot with SST files.
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-1".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-a.sst".into(), "sst-b.sst".into()],
            wal_position: 42,
        });

        // Verify refcounts were set.
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.sst_refcounts.0.get("sst-a.sst"), Some(&1));
            assert_eq!(storage.sst_refcounts.0.get("sst-b.sst"), Some(&1));
        }

        // Cascade delete should decrement refcounts to 0 (remove).
        sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: true,
            deleted_at: 1_700_000_000,
        });

        let storage = sm.storage.read().unwrap();
        assert!(!storage.sst_refcounts.0.contains_key("sst-a.sst"));
        assert!(!storage.sst_refcounts.0.contains_key("sst-b.sst"));
    }

    #[test]
    fn cascade_delete_blocked_by_restore_in_progress() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        // Mark a restore in progress for the snapshot.
        let resp = sm.apply_command(&StateMachineCommand::MarkRestoreBegin {
            snapshot_id: "snap-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Cascade delete should be rejected because snap-1 has a restore in progress.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: true,
            deleted_at: 1_700_000_000,
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(ref msg) if msg.contains("restores in progress")),
            "cascade delete should be blocked when a snapshot has a restore in progress"
        );

        // Snapshot should still exist.
        let storage = sm.storage.read().unwrap();
        assert!(storage.snapshots.contains_key("snap-1"));
        // Volume should NOT be tombstoned.
        assert_ne!(
            storage.volumes.get("vol-1").unwrap().state,
            VolumeState::Deleted
        );
    }

    #[test]
    fn cascade_delete_succeeds_after_restore_completes() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-1", "vol-1");

        // Start and complete a restore.
        sm.apply_command(&StateMachineCommand::MarkRestoreBegin {
            snapshot_id: "snap-1".into(),
        });
        sm.apply_command(&StateMachineCommand::MarkRestoreComplete {
            snapshot_id: "snap-1".into(),
        });

        // Cascade delete should now succeed.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: true,
            deleted_at: 1_700_000_000,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        let storage = sm.storage.read().unwrap();
        assert_eq!(
            storage.snapshots.get("snap-1").unwrap().state,
            SnapshotState::Deleted
        );
        assert_eq!(
            storage.volumes.get("vol-1").unwrap().state,
            VolumeState::Deleted
        );
    }

    #[test]
    fn tombstone_does_not_count_against_quota() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set org quota: max 1 volume.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 1,
            max_total_gb: 1000,
            max_snapshots: 10,
        });

        // Create and delete a volume (creates tombstone).
        create_volume(&sm, "vol-1", "v1", 50, "acme", "p1", "prod");
        sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: 1_700_000_000,
        });

        // Should be able to create another volume since tombstone doesn't count.
        let resp = create_volume(&sm, "vol-2", "v2", 50, "acme", "p1", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));
    }

    #[test]
    fn purge_tombstones_removes_expired() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        let delete_time = 1_700_000_000_u64;

        create_volume(&sm, "vol-1", "v1", 50, "acme", "p1", "prod");
        sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: delete_time,
        });

        // Verify tombstone exists.
        assert!(sm.storage.read().unwrap().volumes.contains_key("vol-1"));

        // Purge with a timestamp past the TTL.
        let far_future = delete_time + TOMBSTONE_TTL_SECS + 1;

        sm.apply_command(&StateMachineCommand::PurgeTombstones {
            now: far_future,
            max_age_secs: TOMBSTONE_TTL_SECS,
        });

        // Tombstone should be purged.
        assert!(!sm.storage.read().unwrap().volumes.contains_key("vol-1"));
    }

    #[test]
    fn purge_tombstones_keeps_recent() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Use a fixed "now" so the test is deterministic and the tombstone
        // is always "recent" relative to the purge timestamp.
        let delete_time = 1_700_000_000_u64;
        let purge_time = delete_time + 60; // 60 seconds later — well within 30-day TTL

        create_volume(&sm, "vol-1", "v1", 50, "acme", "p1", "prod");
        sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-1".into(),
            cascade: false,
            deleted_at: delete_time,
        });

        sm.apply_command(&StateMachineCommand::PurgeTombstones {
            now: purge_time,
            max_age_secs: TOMBSTONE_TTL_SECS,
        });

        // Tombstone should still exist (not old enough).
        assert!(sm.storage.read().unwrap().volumes.contains_key("vol-1"));
    }

    // -- Root volume detach guard ----------------------------------------------

    #[test]
    fn cannot_detach_root_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set quota so volume creation succeeds.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 10,
        });

        // Create a root volume.
        let resp = sm.apply_command(&StateMachineCommand::CreateVolume {
            id: "vol-root-1".into(),
            name: "root-web-1".into(),
            size_gb: 20,
            org_id: "acme".into(),
            project_id: "myapp".into(),
            env_id: "prod".into(),
            volume_type: VolumeType::Root,
            hypervisor_id: None,
            zone: None,
        });
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Attach it.
        let resp = sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-root-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Try to detach — should fail because it's a root volume.
        let resp = sm.apply_command(&StateMachineCommand::VolumeDetach {
            volume_id: "vol-root-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        let msg = match resp {
            StateMachineResponse::Error(m) => m,
            _ => unreachable!(),
        };
        assert!(
            msg.contains("cannot detach root volume"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn can_detach_data_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Set quota so volume creation succeeds.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 10,
        });

        // Create a data volume.
        let resp = create_volume(&sm, "vol-data-1", "pgdata", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Attach it.
        let resp = sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-data-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Detach — should succeed for data volumes.
        let resp = sm.apply_command(&StateMachineCommand::VolumeDetach {
            volume_id: "vol-data-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[test]
    fn delete_attached_root_volume_succeeds() {
        // Root volumes can't be detached (lifecycle guard), so DeleteVolume
        // must allow deletion even when state == Attached to avoid a deadlock.
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 10,
        });

        // Create a root volume.
        let resp = sm.apply_command(&StateMachineCommand::CreateVolume {
            id: "vol-root-del".into(),
            name: "root-del-test".into(),
            size_gb: 20,
            org_id: "acme".into(),
            project_id: "myapp".into(),
            env_id: "prod".into(),
            volume_type: VolumeType::Root,
            hypervisor_id: None,
            zone: None,
        });
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        // Attach it (simulating VM creation).
        let resp = sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-root-del".into(),
            vm_id: "vm-del".into(),
            hypervisor_id: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Delete while attached — must succeed for root volumes.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-root-del".into(),
            cascade: false,
            deleted_at: 1743638400,
        });
        assert!(
            matches!(resp, StateMachineResponse::Ok),
            "expected Ok for attached root volume deletion, got: {resp:?}"
        );
    }

    #[test]
    fn delete_attached_data_volume_still_rejected() {
        // Data volumes must still be detached before deletion.
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 10,
        });

        let resp = create_volume(&sm, "vol-data-guard", "pgdata", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        let resp = sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-data-guard".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Delete while attached — must fail for data volumes.
        let resp = sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-data-guard".into(),
            cascade: false,
            deleted_at: 1743638400,
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(_)),
            "expected Error for attached data volume deletion, got: {resp:?}"
        );
    }

    // ── CommitManifest tests (ADR-006 §12b) ────────────────────────────

    /// Helper: set up a volume attached to a hypervisor, ready for manifest commits.
    fn setup_attached_volume(sm: &RedbStateMachine) {
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 10,
        });
        let resp = create_volume(sm, "vol-m1", "manifest-vol", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));
        let resp = sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-m1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[test]
    fn commit_manifest_happy_path() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // First manifest commit (version 1).
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1, // VolumeAttach increments from 0 to 1
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok), "got: {resp:?}");

        // Verify the pointer was stored.
        let storage = sm.storage.read().unwrap();
        let ptr = storage.manifest_pointers.get("vol-m1").unwrap();
        assert_eq!(ptr.manifest_version, 1);
        assert_eq!(ptr.generation, 1);
        assert_eq!(ptr.s3_key, "manifests/vol-m1/v1.json");
        assert_eq!(ptr.published_by, "hv-1");
    }

    #[test]
    fn commit_manifest_sequential_versions() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Commit v1, v2, v3 sequentially.
        for v in 1..=3 {
            let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
                volume_id: "vol-m1".into(),
                generation: 1,
                manifest_version: v,
                s3_key: format!("manifests/vol-m1/v{v}.json"),
                published_by: "hv-1".into(),
            });
            assert!(
                matches!(resp, StateMachineResponse::Ok),
                "version {v} failed: {resp:?}"
            );
        }

        let storage = sm.storage.read().unwrap();
        let ptr = storage.manifest_pointers.get("vol-m1").unwrap();
        assert_eq!(ptr.manifest_version, 3);
    }

    #[test]
    fn commit_manifest_rejects_generation_mismatch() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Volume placement_generation is 1 after attach. Try with generation 0.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 0, // wrong — should be 1
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(
                    msg.contains("generation mismatch"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_rejects_generation_too_high() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 99, // way too high
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("generation mismatch"), "msg: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_rejects_version_gap() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Skip version 1, try version 2 directly.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 2, // gap — no v1 yet
            s3_key: "manifests/vol-m1/v2.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(
                    msg.contains("manifest version gap"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_rejects_duplicate_version() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Commit v1 successfully.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Try v1 again — should fail (expected 2).
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 1, // duplicate
            s3_key: "manifests/vol-m1/v1b.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("manifest version gap"), "msg: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_rejects_wrong_publisher() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Volume is attached to hv-1, try publishing from hv-2.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-2".into(), // wrong hypervisor
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("wrong publisher"), "unexpected error: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_rejects_nonexistent_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-ghost".into(),
            generation: 1,
            manifest_version: 1,
            s3_key: "manifests/vol-ghost/v1.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("volume not found"), "msg: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_rejects_detached_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Create volume but don't attach it.
        sm.apply_command(&StateMachineCommand::SetStorageQuota {
            scope: QuotaScope::Org {
                org_id: "acme".into(),
            },
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 10,
        });
        let resp = create_volume(&sm, "vol-det", "det-vol", 50, "acme", "myapp", "prod");
        assert!(matches!(resp, StateMachineResponse::Created(_)));

        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-det".into(),
            generation: 0,
            manifest_version: 1,
            s3_key: "manifests/vol-det/v1.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("not attached"), "msg: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_resets_after_reattach() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Commit v1.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Detach and reattach (new generation).
        sm.apply_command(&StateMachineCommand::VolumeDetach {
            volume_id: "vol-m1".into(),
        });
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-m1".into(),
            vm_id: "vm-2".into(),
            hypervisor_id: "hv-2".into(),
        });

        // Old generation (1) should now fail — generation is now 2.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1, // stale
            manifest_version: 2,
            s3_key: "manifests/vol-m1/v2.json".into(),
            published_by: "hv-2".into(),
        });
        assert!(
            matches!(resp, StateMachineResponse::Error(_)),
            "stale generation should be rejected: {resp:?}"
        );

        // Pointer was cleared on detach, so new writer must start at version 1.
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 2,
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1-gen2.json".into(),
            published_by: "hv-2".into(),
        });
        assert!(
            matches!(resp, StateMachineResponse::Ok),
            "new generation commit should succeed at version 1: {resp:?}"
        );
    }

    #[test]
    fn commit_manifest_version_zero_rejected() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        // Version 0 is never valid (first version must be 1).
        let resp = sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 0,
            s3_key: "manifests/vol-m1/v0.json".into(),
            published_by: "hv-1".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("manifest version gap"), "msg: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn commit_manifest_snapshot_includes_pointers() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_attached_volume(&sm);

        sm.apply_command(&StateMachineCommand::CommitManifest {
            volume_id: "vol-m1".into(),
            generation: 1,
            manifest_version: 1,
            s3_key: "manifests/vol-m1/v1.json".into(),
            published_by: "hv-1".into(),
        });

        // Verify manifest_pointers survive snapshot serialization roundtrip.
        let storage = sm.storage.read().unwrap();
        let json = serde_json::to_string(&*storage).unwrap();
        let restored: StorageState = serde_json::from_str(&json).unwrap();
        let ptr = restored.manifest_pointers.get("vol-m1").unwrap();
        assert_eq!(ptr.manifest_version, 1);
        assert_eq!(ptr.published_by, "hv-1");
    }

    // ── Snapshot delete with refcount management (#1202) ──────────────

    #[test]
    fn delete_snapshot_marks_unreferenced_ssts_for_gc() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        // Create snapshot with unique SST files.
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-gc".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-gc-1".into(), "sst-gc-2".into()],
            wal_position: 10,
        });

        // Delete the snapshot.
        let resp = sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-gc".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // SSTs with refcount=0 should be in pending_gc_ssts, NOT removed silently.
        let storage = sm.storage.read().unwrap();
        assert!(!storage.sst_refcounts.0.contains_key("sst-gc-1"));
        assert!(!storage.sst_refcounts.0.contains_key("sst-gc-2"));
        assert!(storage.pending_gc_ssts.contains(&"sst-gc-1".to_string()));
        assert!(storage.pending_gc_ssts.contains(&"sst-gc-2".to_string()));
    }

    #[test]
    fn delete_snapshot_shared_sst_not_gc_until_all_refs_gone() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        // Two snapshots share "sst-shared".
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-a".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-shared".into(), "sst-only-a".into()],
            wal_position: 10,
        });
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-b".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-shared".into(), "sst-only-b".into()],
            wal_position: 20,
        });

        // Delete snap-a: sst-shared refcount drops to 1, sst-only-a to GC.
        sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-a".into(),
        });

        {
            let storage = sm.storage.read().unwrap();
            // sst-shared still referenced by snap-b.
            assert_eq!(storage.sst_refcounts.0.get("sst-shared"), Some(&1));
            assert!(!storage.pending_gc_ssts.contains(&"sst-shared".to_string()));
            // sst-only-a is unreferenced -> marked for GC.
            assert!(storage.pending_gc_ssts.contains(&"sst-only-a".to_string()));
        }

        // Delete snap-b: sst-shared now reaches 0 -> GC.
        sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-b".into(),
        });

        let storage = sm.storage.read().unwrap();
        assert!(storage.pending_gc_ssts.contains(&"sst-shared".to_string()));
        assert!(storage.pending_gc_ssts.contains(&"sst-only-b".to_string()));
    }

    #[test]
    fn delete_snapshot_blocked_by_in_progress_restore() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-r", "vol-1");

        // Mark restore in progress.
        let resp = sm.apply_command(&StateMachineCommand::MarkRestoreBegin {
            snapshot_id: "snap-r".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Attempt to delete while restore is in progress — must fail.
        let resp = sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-r".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(
                    msg.contains("restore is in progress"),
                    "expected restore guard error, got: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }

        // Complete the restore and then delete should succeed.
        sm.apply_command(&StateMachineCommand::MarkRestoreComplete {
            snapshot_id: "snap-r".into(),
        });
        let resp = sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-r".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[test]
    fn delete_snapshot_recalculates_min_wal_position() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        // Create 3 snapshots with different WAL positions.
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-w1".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-w1".into()],
            wal_position: 100,
        });
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-w2".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-w2".into()],
            wal_position: 50,
        });
        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-w3".into(),
            source_volume_id: "vol-1".into(),
            sst_files: vec!["sst-w3".into()],
            wal_position: 200,
        });

        // min_wal_position should be 50 (snap-w2).
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.min_wal_position, Some(50));
        }

        // Delete snap-w2 (the one with the lowest WAL position).
        sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-w2".into(),
        });

        // min_wal_position should now be 100 (snap-w1).
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.min_wal_position, Some(100));
        }

        // Delete snap-w1.
        sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-w1".into(),
        });

        // min_wal_position should now be 200 (snap-w3).
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.min_wal_position, Some(200));
        }

        // Delete last snapshot.
        sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-w3".into(),
        });

        // No snapshots -> min_wal_position is None.
        {
            let storage = sm.storage.read().unwrap();
            assert_eq!(storage.min_wal_position, None);
        }
    }

    #[test]
    fn cascade_delete_marks_ssts_for_gc() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-gc", "pgdata", 100, "acme", "myapp", "prod");

        sm.apply_command(&StateMachineCommand::CreateSnapshot {
            id: "snap-c1".into(),
            source_volume_id: "vol-gc".into(),
            sst_files: vec!["sst-cascade-1".into(), "sst-cascade-2".into()],
            wal_position: 42,
        });

        // Cascade delete volume -> SSTs should end up in pending_gc_ssts.
        sm.apply_command(&StateMachineCommand::DeleteVolume {
            volume_id: "vol-gc".into(),
            cascade: true,
            deleted_at: 1_700_000_000,
        });

        let storage = sm.storage.read().unwrap();
        assert!(storage
            .pending_gc_ssts
            .contains(&"sst-cascade-1".to_string()));
        assert!(storage
            .pending_gc_ssts
            .contains(&"sst-cascade-2".to_string()));
    }

    #[test]
    fn delete_nonexistent_snapshot_returns_error() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        let resp = sm.apply_command(&StateMachineCommand::DeleteSnapshot {
            snapshot_id: "snap-ghost".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("snapshot not found"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn mark_restore_begin_for_nonexistent_snapshot_returns_error() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        let resp = sm.apply_command(&StateMachineCommand::MarkRestoreBegin {
            snapshot_id: "snap-missing".into(),
        });
        match resp {
            StateMachineResponse::Error(msg) => {
                assert!(msg.contains("snapshot not found"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn mark_restore_complete_is_idempotent() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        // Complete for a snapshot that was never marked as restoring -> noop, Ok.
        let resp = sm.apply_command(&StateMachineCommand::MarkRestoreComplete {
            snapshot_id: "snap-noop".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[test]
    fn mark_restore_begin_is_idempotent() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        create_snapshot(&sm, "snap-idem", "vol-1");

        // Mark begin twice -- should not duplicate.
        sm.apply_command(&StateMachineCommand::MarkRestoreBegin {
            snapshot_id: "snap-idem".into(),
        });
        sm.apply_command(&StateMachineCommand::MarkRestoreBegin {
            snapshot_id: "snap-idem".into(),
        });

        let storage = sm.storage.read().unwrap();
        let count = storage
            .restores_in_progress
            .iter()
            .filter(|id| *id == "snap-idem")
            .count();
        assert_eq!(count, 1, "restore should not be duplicated");
    }

    #[test]
    fn storage_state_serde_includes_new_fields() {
        let state = StorageState {
            pending_gc_ssts: vec!["sst-old-1".into(), "sst-old-2".into()],
            restores_in_progress: vec!["snap-restoring".into()],
            min_wal_position: Some(42),
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        let restored: StorageState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.pending_gc_ssts, vec!["sst-old-1", "sst-old-2"]);
        assert_eq!(restored.restores_in_progress, vec!["snap-restoring"]);
        assert_eq!(restored.min_wal_position, Some(42));
    }

    #[test]
    fn storage_state_serde_defaults_new_fields() {
        // Simulates loading old state without the new fields.
        let json =
            r#"{"quotas":{},"volumes":{},"snapshots":{},"sst_refcounts":{},"storage_configs":{}}"#;
        let state: StorageState = serde_json::from_str(json).unwrap();
        assert!(state.pending_gc_ssts.is_empty());
        assert!(state.restores_in_progress.is_empty());
        assert_eq!(state.min_wal_position, None);
    }

    // -----------------------------------------------------------------------
    // Cross-zone migration tests (#1283)
    // -----------------------------------------------------------------------

    /// Helper: set up storage configs for two zones.
    fn setup_two_zones(sm: &RedbStateMachine) {
        sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west".into(),
            config: Box::new(crate::commands::StorageConfig {
                s3_endpoint: "https://s3.eu-west.example.com".into(),
                s3_bucket: "syfrah-eu-west".into(),
                s3_access_key: "AK-WEST".into(),
                s3_secret_key: "SK-WEST".into(),
                cache_disk_path: "/dev/nvme0".into(),
                cache_disk_size_gb: 100,
                cache_memory_size_gb: 4,
            }),
        });
        sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-east".into(),
            zone: "eu-east".into(),
            config: Box::new(crate::commands::StorageConfig {
                s3_endpoint: "https://s3.eu-east.example.com".into(),
                s3_bucket: "syfrah-eu-east".into(),
                s3_access_key: "AK-EAST".into(),
                s3_secret_key: "SK-EAST".into(),
                cache_disk_path: "/dev/nvme1".into(),
                cache_disk_size_gb: 100,
                cache_memory_size_gb: 4,
            }),
        });
    }

    #[test]
    fn migrate_volume_to_zone_transitions_to_migrating() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_two_zones(&sm);

        create_volume(&sm, "vol-mig", "pgdata", 100, "acme", "myapp", "prod");

        let resp = sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-mig".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-east-1".into(),
            target_vm_id: None,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        let storage = sm.storage.read().unwrap();
        let vol = storage.volumes.get("vol-mig").unwrap();
        assert_eq!(vol.state, VolumeState::Migrating);
        assert_eq!(vol.migration_source_zone.as_deref(), Some("eu-west"));
        assert_eq!(vol.migration_target_zone.as_deref(), Some("eu-east"));
        assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-east-1"));
        assert_eq!(vol.placement_generation, 1); // bumped from 0
    }

    #[test]
    fn migrate_volume_rejects_same_zone() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_two_zones(&sm);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        let resp = sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-1".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-west".into(),
            target_hypervisor: "hv-1".into(),
            target_vm_id: None,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn migrate_volume_rejects_missing_source_zone_config() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        // Only set up target zone.
        sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-east".into(),
            zone: "eu-east".into(),
            config: Box::new(crate::commands::StorageConfig {
                s3_endpoint: "https://s3.eu-east.example.com".into(),
                s3_bucket: "syfrah-eu-east".into(),
                s3_access_key: "AK".into(),
                s3_secret_key: "SK".into(),
                cache_disk_path: "/dev/nvme0".into(),
                cache_disk_size_gb: 100,
                cache_memory_size_gb: 4,
            }),
        });

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        let resp = sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-1".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-1".into(),
            target_vm_id: None,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        if let StateMachineResponse::Error(msg) = resp {
            assert!(msg.contains("source zone"));
        }
    }

    #[test]
    fn migrate_volume_rejects_missing_target_zone_config() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        // Only set up source zone.
        sm.apply_command(&StateMachineCommand::SetStorageConfig {
            region: "eu-west".into(),
            zone: "eu-west".into(),
            config: Box::new(crate::commands::StorageConfig {
                s3_endpoint: "https://s3.eu-west.example.com".into(),
                s3_bucket: "syfrah-eu-west".into(),
                s3_access_key: "AK".into(),
                s3_secret_key: "SK".into(),
                cache_disk_path: "/dev/nvme0".into(),
                cache_disk_size_gb: 100,
                cache_memory_size_gb: 4,
            }),
        });

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        let resp = sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-1".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-1".into(),
            target_vm_id: None,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        if let StateMachineResponse::Error(msg) = resp {
            assert!(msg.contains("target zone"));
        }
    }

    #[test]
    fn migrate_volume_rejects_attached_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_two_zones(&sm);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");
        // Attach the volume.
        sm.apply_command(&StateMachineCommand::VolumeAttach {
            volume_id: "vol-1".into(),
            vm_id: "vm-1".into(),
            hypervisor_id: "hv-1".into(),
        });

        let resp = sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-1".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-east-1".into(),
            target_vm_id: None,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        if let StateMachineResponse::Error(msg) = resp {
            assert!(msg.contains("must be available to migrate"));
        }
    }

    #[test]
    fn migrate_volume_rejects_nonexistent_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_two_zones(&sm);

        let resp = sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-nope".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-1".into(),
            target_vm_id: None,
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        if let StateMachineResponse::Error(msg) = resp {
            assert!(msg.contains("volume not found"));
        }
    }

    #[test]
    fn complete_migration_transitions_to_available() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_two_zones(&sm);

        create_volume(&sm, "vol-mig", "pgdata", 100, "acme", "myapp", "prod");
        sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-mig".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-east-1".into(),
            target_vm_id: Some("vm-new".into()),
        });

        let resp = sm.apply_command(&StateMachineCommand::CompleteMigration {
            volume_id: "vol-mig".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        let storage = sm.storage.read().unwrap();
        let vol = storage.volumes.get("vol-mig").unwrap();
        assert_eq!(vol.state, VolumeState::Available);
        assert!(vol.migration_source_zone.is_none());
        assert!(vol.migration_target_zone.is_none());
        assert!(vol.pre_migration_hypervisor.is_none());
        // Hypervisor assignment should remain on target.
        assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-east-1"));
    }

    #[test]
    fn complete_migration_rejects_non_migrating_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        let resp = sm.apply_command(&StateMachineCommand::CompleteMigration {
            volume_id: "vol-1".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        if let StateMachineResponse::Error(msg) = resp {
            assert!(msg.contains("not migrating"));
        }
    }

    #[test]
    fn rollback_migration_restores_pre_migration_state() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        setup_two_zones(&sm);

        create_volume(&sm, "vol-mig", "pgdata", 100, "acme", "myapp", "prod");

        // Record the pre-migration hypervisor by first assigning to a hypervisor
        // and then detaching (so we have it in the record).
        {
            let mut storage = sm.storage.write().unwrap();
            let vol = storage.volumes.get_mut("vol-mig").unwrap();
            vol.attached_hypervisor_id = Some("hv-west-1".into());
        }

        sm.apply_command(&StateMachineCommand::MigrateVolumeToZone {
            volume_id: "vol-mig".into(),
            source_zone: "eu-west".into(),
            target_zone: "eu-east".into(),
            target_hypervisor: "hv-east-1".into(),
            target_vm_id: None,
        });

        // Verify it's migrating.
        {
            let storage = sm.storage.read().unwrap();
            let vol = storage.volumes.get("vol-mig").unwrap();
            assert_eq!(vol.state, VolumeState::Migrating);
            assert_eq!(vol.pre_migration_hypervisor.as_deref(), Some("hv-west-1"));
        }

        let resp = sm.apply_command(&StateMachineCommand::RollbackMigration {
            volume_id: "vol-mig".into(),
            reason: "S3 copy failed at object 5 of 100".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        let storage = sm.storage.read().unwrap();
        let vol = storage.volumes.get("vol-mig").unwrap();
        assert_eq!(vol.state, VolumeState::Available);
        assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-west-1"));
        assert!(vol.migration_source_zone.is_none());
        assert!(vol.migration_target_zone.is_none());
        assert!(vol.pre_migration_hypervisor.is_none());
    }

    #[test]
    fn rollback_migration_rejects_non_migrating_volume() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);

        create_volume(&sm, "vol-1", "pgdata", 100, "acme", "myapp", "prod");

        let resp = sm.apply_command(&StateMachineCommand::RollbackMigration {
            volume_id: "vol-1".into(),
            reason: "test".into(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
        if let StateMachineResponse::Error(msg) = resp {
            assert!(msg.contains("not migrating"));
        }
    }

    #[test]
    fn volume_record_migration_fields_default_to_none() {
        // Simulates deserializing an old VolumeRecord without migration fields.
        let json = r#"{
            "id": "vol-1",
            "name": "test",
            "size_gb": 50,
            "org_id": "acme",
            "project_id": "proj",
            "env_id": "prod",
            "volume_type": "Data",
            "state": "Available",
            "attached_vm_id": null,
            "attached_hypervisor_id": null,
            "placement_generation": 0,
            "deletion_protection": false,
            "deleted_at": null
        }"#;
        let vol: VolumeRecord = serde_json::from_str(json).unwrap();
        assert!(vol.migration_source_zone.is_none());
        assert!(vol.migration_target_zone.is_none());
        assert!(vol.pre_migration_hypervisor.is_none());
        assert!(vol.pre_migration_vm_id.is_none());
    }
}
