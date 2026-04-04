//! Storage persistence — CRUD for volumes, snapshots, storage config,
//! quotas, SST refcounts, and manifest pointers.
//!
//! Backed by redb tables following the same pattern as `HypervisorStore`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};

// ---------------------------------------------------------------------------
// Table names
// ---------------------------------------------------------------------------

const VOLUMES_TABLE: &str = "volumes";
const SNAPSHOTS_TABLE: &str = "snapshots";
const STORAGE_CONFIG_TABLE: &str = "storage_config";
const STORAGE_QUOTAS_TABLE: &str = "storage_quotas";
const SST_REFCOUNTS_TABLE: &str = "sst_refcounts";
const MANIFEST_POINTERS_TABLE: &str = "manifest_pointers";
const VOLUMES_BY_ENV_TABLE: &str = "volumes_by_env";
const VOLUMES_BY_HYPERVISOR_TABLE: &str = "volumes_by_hypervisor";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Type of volume: root (tied to VM lifecycle) or data (independent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeType {
    Root,
    Data,
}

/// Current lifecycle state of a volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeState {
    Creating,
    Available,
    Attaching,
    Attached,
    Detaching,
    Resizing,
    Deleting,
    Deleted,
    Error,
}

/// A storage volume record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub id: String,
    pub name: String,
    pub size_gb: u32,
    pub org_id: String,
    pub project_id: String,
    pub env_id: String,
    pub volume_type: VolumeType,
    pub state: VolumeState,
    /// VM this volume is attached to, if any.
    pub attached_vm_id: Option<String>,
    /// Hypervisor hosting the attachment, if any.
    pub attached_hypervisor_id: Option<String>,
    /// Monotonically increasing generation for fencing (ADR-006 §8).
    pub placement_generation: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A crash-consistent snapshot record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub source_volume_id: String,
    /// SST files referenced by this snapshot.
    pub sst_files: Vec<String>,
    /// WAL position at the time of snapshot.
    pub wal_position: u64,
    pub created_at: u64,
}

/// Pointer to the current manifest for a volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPointer {
    pub volume_id: String,
    pub generation: u64,
    pub manifest_version: u64,
    pub s3_key: String,
}

/// Per-scope storage quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageQuota {
    pub max_volumes: u32,
    pub max_total_gb: u64,
    pub max_snapshots: u32,
}

/// Per-region S3 storage configuration.
///
/// Mirrors the `StorageConfig` in controlplane commands. Defined here so the
/// store layer has no upward dependency on the controlplane crate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    pub s3_endpoint: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub cache_disk_path: String,
    pub cache_disk_size_gb: u32,
    pub cache_memory_size_gb: u32,
}

// SECURITY: Custom Debug impl to prevent S3 credentials from appearing in logs
// or error messages. Mirrors the redaction pattern in controlplane StorageConfig.
impl std::fmt::Debug for StorageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageConfig")
            .field("s3_endpoint", &self.s3_endpoint)
            .field("s3_bucket", &self.s3_bucket)
            .field("s3_access_key", &"[REDACTED]")
            .field("s3_secret_key", &"[REDACTED]")
            .field("cache_disk_path", &self.cache_disk_path)
            .field("cache_disk_size_gb", &self.cache_disk_size_gb)
            .field("cache_memory_size_gb", &self.cache_memory_size_gb)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Persistent store for storage metadata backed by redb.
pub struct StorageStore {
    db: LayerDb,
}

impl StorageStore {
    /// Create a new `StorageStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Get a reference to the underlying database.
    pub fn db(&self) -> &LayerDb {
        &self.db
    }

    /// All table names used by this store (for snapshot export/import).
    pub fn table_names() -> &'static [&'static str] {
        &[
            VOLUMES_TABLE,
            SNAPSHOTS_TABLE,
            STORAGE_CONFIG_TABLE,
            STORAGE_QUOTAS_TABLE,
            SST_REFCOUNTS_TABLE,
            MANIFEST_POINTERS_TABLE,
            VOLUMES_BY_ENV_TABLE,
            VOLUMES_BY_HYPERVISOR_TABLE,
        ]
    }

    // ── Helpers ─────────────────────────────────────────────────────

    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    // ── Volume CRUD ─────────────────────────────────────────────────

    /// Create a new volume. Fails if the ID already exists.
    pub fn create_volume(&self, vol: &Volume) -> Result<()> {
        if self.db.exists(VOLUMES_TABLE, &vol.id)? {
            return Err(OrgError::AlreadyExists(vol.id.clone()));
        }
        self.db.set(VOLUMES_TABLE, &vol.id, vol)?;
        self.add_to_env_index(&vol.env_id, &vol.id)?;
        Ok(())
    }

    /// Get a volume by ID.
    pub fn get_volume(&self, id: &str) -> Result<Option<Volume>> {
        Ok(self.db.get(VOLUMES_TABLE, id)?)
    }

    /// Update an existing volume. Fails if it doesn't exist.
    pub fn update_volume(&self, vol: &Volume) -> Result<()> {
        if !self.db.exists(VOLUMES_TABLE, &vol.id)? {
            return Err(OrgError::NotFound(format!("volume '{}'", vol.id)));
        }
        self.db.set(VOLUMES_TABLE, &vol.id, vol)?;
        Ok(())
    }

    /// Delete a volume by ID. Removes from index tables.
    pub fn delete_volume(&self, id: &str) -> Result<()> {
        let vol: Volume = self
            .db
            .get(VOLUMES_TABLE, id)?
            .ok_or_else(|| OrgError::NotFound(format!("volume '{id}'")))?;
        self.remove_from_env_index(&vol.env_id, id)?;
        if let Some(ref hv_id) = vol.attached_hypervisor_id {
            self.remove_from_hypervisor_index(hv_id, id)?;
        }
        self.db.delete(VOLUMES_TABLE, id)?;
        // Clean up manifest pointer if present — ignore "not found" but propagate real errors.
        match self.db.delete(MANIFEST_POINTERS_TABLE, id) {
            Ok(_) => {}
            Err(e) if e.to_string().contains("not found") => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// List all volumes.
    pub fn list_volumes(&self) -> Result<Vec<Volume>> {
        let entries: Vec<(String, Volume)> = self.db.list(VOLUMES_TABLE)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    /// List volumes in a specific environment.
    pub fn list_volumes_by_env(&self, env_id: &str) -> Result<Vec<Volume>> {
        let ids: Vec<String> = self
            .db
            .get(VOLUMES_BY_ENV_TABLE, env_id)?
            .unwrap_or_default();
        let mut vols = Vec::new();
        for id in ids {
            if let Some(v) = self.get_volume(&id)? {
                vols.push(v);
            }
        }
        Ok(vols)
    }

    /// List volumes on a specific hypervisor.
    pub fn list_volumes_by_hypervisor(&self, hypervisor_id: &str) -> Result<Vec<Volume>> {
        let ids: Vec<String> = self
            .db
            .get(VOLUMES_BY_HYPERVISOR_TABLE, hypervisor_id)?
            .unwrap_or_default();
        let mut vols = Vec::new();
        for id in ids {
            if let Some(v) = self.get_volume(&id)? {
                vols.push(v);
            }
        }
        Ok(vols)
    }

    // ── Volume state transitions ────────────────────────────────────

    /// Attach a volume to a VM on a hypervisor. Bumps placement_generation.
    pub fn attach_volume(&self, volume_id: &str, vm_id: &str, hypervisor_id: &str) -> Result<()> {
        let mut vol = self
            .get_volume(volume_id)?
            .ok_or_else(|| OrgError::NotFound(format!("volume '{volume_id}'")))?;
        if vol.state != VolumeState::Available {
            return Err(OrgError::InvalidVolumeState {
                volume_id: volume_id.to_string(),
                current: format!("{:?}", vol.state),
                expected: "Available".to_string(),
            });
        }
        vol.state = VolumeState::Attached;
        vol.attached_vm_id = Some(vm_id.to_string());
        vol.attached_hypervisor_id = Some(hypervisor_id.to_string());
        vol.placement_generation += 1;
        vol.updated_at = Self::now();
        self.db.set(VOLUMES_TABLE, volume_id, &vol)?;
        self.add_to_hypervisor_index(hypervisor_id, volume_id)?;
        Ok(())
    }

    /// Detach a volume from its current VM.
    pub fn detach_volume(&self, volume_id: &str) -> Result<()> {
        let mut vol = self
            .get_volume(volume_id)?
            .ok_or_else(|| OrgError::NotFound(format!("volume '{volume_id}'")))?;
        if vol.state != VolumeState::Attached {
            return Err(OrgError::InvalidVolumeState {
                volume_id: volume_id.to_string(),
                current: format!("{:?}", vol.state),
                expected: "Attached".to_string(),
            });
        }
        if let Some(ref hv_id) = vol.attached_hypervisor_id {
            self.remove_from_hypervisor_index(hv_id, volume_id)?;
        }
        vol.state = VolumeState::Available;
        vol.attached_vm_id = None;
        vol.attached_hypervisor_id = None;
        vol.updated_at = Self::now();
        self.db.set(VOLUMES_TABLE, volume_id, &vol)?;
        Ok(())
    }

    /// Resize a volume (grow only).
    pub fn resize_volume(&self, volume_id: &str, new_size_gb: u32) -> Result<()> {
        let mut vol = self
            .get_volume(volume_id)?
            .ok_or_else(|| OrgError::NotFound(format!("volume '{volume_id}'")))?;
        if vol.state != VolumeState::Available {
            return Err(OrgError::InvalidVolumeState {
                volume_id: volume_id.to_string(),
                current: format!("{:?}", vol.state),
                expected: "Available".to_string(),
            });
        }
        if new_size_gb <= vol.size_gb {
            return Err(OrgError::InvalidArgument(format!(
                "new size {}GB must be greater than current {}GB (shrink is not supported)",
                new_size_gb, vol.size_gb
            )));
        }
        vol.size_gb = new_size_gb;
        vol.updated_at = Self::now();
        self.db.set(VOLUMES_TABLE, volume_id, &vol)?;
        Ok(())
    }

    // ── Snapshot CRUD ───────────────────────────────────────────────

    /// Create a snapshot. Increments SST refcounts for referenced files.
    pub fn create_snapshot(&self, snap: &Snapshot) -> Result<()> {
        if self.db.exists(SNAPSHOTS_TABLE, &snap.id)? {
            return Err(OrgError::AlreadyExists(snap.id.clone()));
        }
        self.db.set(SNAPSHOTS_TABLE, &snap.id, snap)?;
        // Increment refcounts for each SST file.
        for sst in &snap.sst_files {
            self.increment_sst_refcount(sst)?;
        }
        Ok(())
    }

    /// Get a snapshot by ID.
    pub fn get_snapshot(&self, id: &str) -> Result<Option<Snapshot>> {
        Ok(self.db.get(SNAPSHOTS_TABLE, id)?)
    }

    /// Delete a snapshot. Decrements SST refcounts.
    pub fn delete_snapshot(&self, id: &str) -> Result<()> {
        let snap: Snapshot = self
            .db
            .get(SNAPSHOTS_TABLE, id)?
            .ok_or_else(|| OrgError::NotFound(format!("snapshot '{id}'")))?;
        // Decrement refcounts for each SST file.
        for sst in &snap.sst_files {
            self.decrement_sst_refcount(sst)?;
        }
        self.db.delete(SNAPSHOTS_TABLE, id)?;
        Ok(())
    }

    /// List all snapshots.
    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>> {
        let entries: Vec<(String, Snapshot)> = self.db.list(SNAPSHOTS_TABLE)?;
        Ok(entries.into_iter().map(|(_, s)| s).collect())
    }

    /// List snapshots for a specific volume.
    pub fn list_snapshots_by_volume(&self, volume_id: &str) -> Result<Vec<Snapshot>> {
        let all = self.list_snapshots()?;
        Ok(all
            .into_iter()
            .filter(|s| s.source_volume_id == volume_id)
            .collect())
    }

    // ── Storage config ──────────────────────────────────────────────

    /// Set per-region storage configuration.
    pub fn set_storage_config(&self, region: &str, config: &StorageConfig) -> Result<()> {
        self.db.set(STORAGE_CONFIG_TABLE, region, config)?;
        Ok(())
    }

    /// Get storage configuration for a region.
    pub fn get_storage_config(&self, region: &str) -> Result<Option<StorageConfig>> {
        Ok(self.db.get(STORAGE_CONFIG_TABLE, region)?)
    }

    /// List all storage configs (region → config).
    pub fn list_storage_configs(&self) -> Result<Vec<(String, StorageConfig)>> {
        Ok(self.db.list(STORAGE_CONFIG_TABLE)?)
    }

    // ── Storage quotas ──────────────────────────────────────────────

    /// Set storage quota for a scope.
    pub fn set_storage_quota(&self, scope: &str, quota: &StorageQuota) -> Result<()> {
        self.db.set(STORAGE_QUOTAS_TABLE, scope, quota)?;
        Ok(())
    }

    /// Get storage quota for a scope.
    pub fn get_storage_quota(&self, scope: &str) -> Result<Option<StorageQuota>> {
        Ok(self.db.get(STORAGE_QUOTAS_TABLE, scope)?)
    }

    /// Delete storage quota for a scope.
    pub fn delete_storage_quota(&self, scope: &str) -> Result<()> {
        self.db.delete(STORAGE_QUOTAS_TABLE, scope)?;
        Ok(())
    }

    // ── SST refcounts ───────────────────────────────────────────────

    /// Get the refcount for an SST file key. Returns 0 if not tracked.
    pub fn get_sst_refcount(&self, sst_key: &str) -> Result<u64> {
        let val: Option<u64> = self.db.get(SST_REFCOUNTS_TABLE, sst_key)?;
        Ok(val.unwrap_or(0))
    }

    /// Increment SST refcount by 1. Returns the new count.
    /// Thread-safety guaranteed by single-writer access.
    pub fn increment_sst_refcount(&self, sst_key: &str) -> Result<u64> {
        let current = self.get_sst_refcount(sst_key)?;
        let new_val = current + 1;
        self.db.set(SST_REFCOUNTS_TABLE, sst_key, &new_val)?;
        Ok(new_val)
    }

    /// Decrement SST refcount by 1. Returns the new count.
    /// Removes the entry if the count reaches 0.
    /// Thread-safety guaranteed by single-writer access.
    pub fn decrement_sst_refcount(&self, sst_key: &str) -> Result<u64> {
        let current = self.get_sst_refcount(sst_key)?;
        if current == 0 {
            return Ok(0);
        }
        let new_val = current - 1;
        if new_val == 0 {
            self.db.delete(SST_REFCOUNTS_TABLE, sst_key)?;
        } else {
            self.db.set(SST_REFCOUNTS_TABLE, sst_key, &new_val)?;
        }
        Ok(new_val)
    }

    /// List all SST keys with non-zero refcounts.
    pub fn list_sst_refcounts(&self) -> Result<Vec<(String, u64)>> {
        Ok(self.db.list(SST_REFCOUNTS_TABLE)?)
    }

    // ── Manifest pointers ───────────────────────────────────────────

    /// Set the manifest pointer for a volume.
    pub fn set_manifest_pointer(&self, volume_id: &str, ptr: &ManifestPointer) -> Result<()> {
        self.db.set(MANIFEST_POINTERS_TABLE, volume_id, ptr)?;
        Ok(())
    }

    /// Get the manifest pointer for a volume.
    pub fn get_manifest_pointer(&self, volume_id: &str) -> Result<Option<ManifestPointer>> {
        Ok(self.db.get(MANIFEST_POINTERS_TABLE, volume_id)?)
    }

    /// Delete the manifest pointer for a volume.
    pub fn delete_manifest_pointer(&self, volume_id: &str) -> Result<()> {
        self.db.delete(MANIFEST_POINTERS_TABLE, volume_id)?;
        Ok(())
    }

    // ── Index helpers ───────────────────────────────────────────────

    fn add_to_env_index(&self, env_id: &str, volume_id: &str) -> Result<()> {
        let mut ids: Vec<String> = self
            .db
            .get(VOLUMES_BY_ENV_TABLE, env_id)?
            .unwrap_or_default();
        if !ids.contains(&volume_id.to_string()) {
            ids.push(volume_id.to_string());
            self.db.set(VOLUMES_BY_ENV_TABLE, env_id, &ids)?;
        }
        Ok(())
    }

    fn remove_from_env_index(&self, env_id: &str, volume_id: &str) -> Result<()> {
        let mut ids: Vec<String> = self
            .db
            .get(VOLUMES_BY_ENV_TABLE, env_id)?
            .unwrap_or_default();
        ids.retain(|id| id != volume_id);
        if ids.is_empty() {
            self.db.delete(VOLUMES_BY_ENV_TABLE, env_id)?;
        } else {
            self.db.set(VOLUMES_BY_ENV_TABLE, env_id, &ids)?;
        }
        Ok(())
    }

    pub fn add_to_hypervisor_index(&self, hypervisor_id: &str, volume_id: &str) -> Result<()> {
        let mut ids: Vec<String> = self
            .db
            .get(VOLUMES_BY_HYPERVISOR_TABLE, hypervisor_id)?
            .unwrap_or_default();
        if !ids.contains(&volume_id.to_string()) {
            ids.push(volume_id.to_string());
            self.db
                .set(VOLUMES_BY_HYPERVISOR_TABLE, hypervisor_id, &ids)?;
        }
        Ok(())
    }

    pub fn remove_from_hypervisor_index(&self, hypervisor_id: &str, volume_id: &str) -> Result<()> {
        let mut ids: Vec<String> = self
            .db
            .get(VOLUMES_BY_HYPERVISOR_TABLE, hypervisor_id)?
            .unwrap_or_default();
        ids.retain(|id| id != volume_id);
        if ids.is_empty() {
            self.db.delete(VOLUMES_BY_HYPERVISOR_TABLE, hypervisor_id)?;
        } else {
            self.db
                .set(VOLUMES_BY_HYPERVISOR_TABLE, hypervisor_id, &ids)?;
        }
        Ok(())
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, StorageStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-storage.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, StorageStore::new(db))
    }

    fn sample_volume(id: &str, env_id: &str) -> Volume {
        Volume {
            id: id.to_string(),
            name: format!("vol-{id}"),
            size_gb: 100,
            org_id: "acme".to_string(),
            project_id: "myapp".to_string(),
            env_id: env_id.to_string(),
            volume_type: VolumeType::Data,
            state: VolumeState::Available,
            attached_vm_id: None,
            attached_hypervisor_id: None,
            placement_generation: 0,
            created_at: 1000,
            updated_at: 1000,
        }
    }

    fn sample_snapshot(id: &str, volume_id: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            source_volume_id: volume_id.to_string(),
            sst_files: vec!["sst-001".to_string(), "sst-002".to_string()],
            wal_position: 42,
            created_at: 2000,
        }
    }

    // ── Volume CRUD ─────────────────────────────────────────────────

    #[test]
    fn create_and_get_volume() {
        let (_dir, store) = temp_store();
        let vol = sample_volume("vol-01", "prod");
        store.create_volume(&vol).unwrap();
        let got = store.get_volume("vol-01").unwrap().unwrap();
        assert_eq!(got, vol);
    }

    #[test]
    fn create_duplicate_volume_fails() {
        let (_dir, store) = temp_store();
        let vol = sample_volume("vol-01", "prod");
        store.create_volume(&vol).unwrap();
        assert!(store.create_volume(&vol).is_err());
    }

    #[test]
    fn get_missing_volume_returns_none() {
        let (_dir, store) = temp_store();
        assert!(store.get_volume("nope").unwrap().is_none());
    }

    #[test]
    fn update_volume() {
        let (_dir, store) = temp_store();
        let mut vol = sample_volume("vol-01", "prod");
        store.create_volume(&vol).unwrap();
        vol.size_gb = 200;
        store.update_volume(&vol).unwrap();
        let got = store.get_volume("vol-01").unwrap().unwrap();
        assert_eq!(got.size_gb, 200);
    }

    #[test]
    fn update_missing_volume_fails() {
        let (_dir, store) = temp_store();
        let vol = sample_volume("vol-01", "prod");
        assert!(store.update_volume(&vol).is_err());
    }

    #[test]
    fn delete_volume() {
        let (_dir, store) = temp_store();
        let vol = sample_volume("vol-01", "prod");
        store.create_volume(&vol).unwrap();
        store.delete_volume("vol-01").unwrap();
        assert!(store.get_volume("vol-01").unwrap().is_none());
    }

    #[test]
    fn delete_missing_volume_fails() {
        let (_dir, store) = temp_store();
        assert!(store.delete_volume("nope").is_err());
    }

    #[test]
    fn list_volumes() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store.create_volume(&sample_volume("v2", "prod")).unwrap();
        let vols = store.list_volumes().unwrap();
        assert_eq!(vols.len(), 2);
    }

    // ── Index queries ───────────────────────────────────────────────

    #[test]
    fn list_volumes_by_env() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store
            .create_volume(&sample_volume("v2", "staging"))
            .unwrap();
        store.create_volume(&sample_volume("v3", "prod")).unwrap();
        let prod = store.list_volumes_by_env("prod").unwrap();
        assert_eq!(prod.len(), 2);
        let staging = store.list_volumes_by_env("staging").unwrap();
        assert_eq!(staging.len(), 1);
    }

    #[test]
    fn list_volumes_by_hypervisor() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store.attach_volume("v1", "vm-1", "hv-1").unwrap();
        let vols = store.list_volumes_by_hypervisor("hv-1").unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].id, "v1");
    }

    // ── Attach / Detach ─────────────────────────────────────────────

    #[test]
    fn attach_and_detach_volume() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store.attach_volume("v1", "vm-1", "hv-1").unwrap();

        let vol = store.get_volume("v1").unwrap().unwrap();
        assert_eq!(vol.state, VolumeState::Attached);
        assert_eq!(vol.attached_vm_id.as_deref(), Some("vm-1"));
        assert_eq!(vol.attached_hypervisor_id.as_deref(), Some("hv-1"));
        assert_eq!(vol.placement_generation, 1);

        store.detach_volume("v1").unwrap();
        let vol = store.get_volume("v1").unwrap().unwrap();
        assert_eq!(vol.state, VolumeState::Available);
        assert!(vol.attached_vm_id.is_none());
    }

    #[test]
    fn attach_non_available_volume_fails() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store.attach_volume("v1", "vm-1", "hv-1").unwrap();
        // Already attached — should fail.
        assert!(store.attach_volume("v1", "vm-2", "hv-2").is_err());
    }

    #[test]
    fn detach_non_attached_volume_fails() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        // Available, not attached — should fail.
        assert!(store.detach_volume("v1").is_err());
    }

    // ── Resize ──────────────────────────────────────────────────────

    #[test]
    fn resize_volume() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store.resize_volume("v1", 200).unwrap();
        let vol = store.get_volume("v1").unwrap().unwrap();
        assert_eq!(vol.size_gb, 200);
    }

    #[test]
    fn resize_shrink_fails() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        assert!(store.resize_volume("v1", 50).is_err());
    }

    // ── Snapshot CRUD ───────────────────────────────────────────────

    #[test]
    fn create_and_get_snapshot() {
        let (_dir, store) = temp_store();
        let snap = sample_snapshot("snap-01", "vol-01");
        store.create_snapshot(&snap).unwrap();
        let got = store.get_snapshot("snap-01").unwrap().unwrap();
        assert_eq!(got, snap);
    }

    #[test]
    fn create_duplicate_snapshot_fails() {
        let (_dir, store) = temp_store();
        let snap = sample_snapshot("snap-01", "vol-01");
        store.create_snapshot(&snap).unwrap();
        assert!(store.create_snapshot(&snap).is_err());
    }

    #[test]
    fn delete_snapshot_decrements_refcounts() {
        let (_dir, store) = temp_store();
        let snap = sample_snapshot("snap-01", "vol-01");
        store.create_snapshot(&snap).unwrap();

        // Both SST files should have refcount 1.
        assert_eq!(store.get_sst_refcount("sst-001").unwrap(), 1);
        assert_eq!(store.get_sst_refcount("sst-002").unwrap(), 1);

        store.delete_snapshot("snap-01").unwrap();
        // Refcounts should be 0 (entries removed).
        assert_eq!(store.get_sst_refcount("sst-001").unwrap(), 0);
        assert_eq!(store.get_sst_refcount("sst-002").unwrap(), 0);
    }

    #[test]
    fn list_snapshots_by_volume() {
        let (_dir, store) = temp_store();
        store
            .create_snapshot(&sample_snapshot("s1", "vol-01"))
            .unwrap();
        store
            .create_snapshot(&sample_snapshot("s2", "vol-02"))
            .unwrap();
        store
            .create_snapshot(&sample_snapshot("s3", "vol-01"))
            .unwrap();

        let snaps = store.list_snapshots_by_volume("vol-01").unwrap();
        assert_eq!(snaps.len(), 2);
    }

    // ── SST refcounts ───────────────────────────────────────────────

    #[test]
    fn sst_refcount_increment_decrement() {
        let (_dir, store) = temp_store();
        assert_eq!(store.get_sst_refcount("sst-x").unwrap(), 0);

        assert_eq!(store.increment_sst_refcount("sst-x").unwrap(), 1);
        assert_eq!(store.increment_sst_refcount("sst-x").unwrap(), 2);
        assert_eq!(store.increment_sst_refcount("sst-x").unwrap(), 3);

        assert_eq!(store.decrement_sst_refcount("sst-x").unwrap(), 2);
        assert_eq!(store.decrement_sst_refcount("sst-x").unwrap(), 1);
        assert_eq!(store.decrement_sst_refcount("sst-x").unwrap(), 0);
        // Already 0 — stays at 0.
        assert_eq!(store.decrement_sst_refcount("sst-x").unwrap(), 0);
    }

    #[test]
    fn shared_sst_refcounts_across_snapshots() {
        let (_dir, store) = temp_store();
        // Two snapshots referencing the same SST file.
        let snap1 = Snapshot {
            id: "s1".to_string(),
            source_volume_id: "v1".to_string(),
            sst_files: vec!["shared-sst".to_string()],
            wal_position: 10,
            created_at: 1000,
        };
        let snap2 = Snapshot {
            id: "s2".to_string(),
            source_volume_id: "v1".to_string(),
            sst_files: vec!["shared-sst".to_string()],
            wal_position: 20,
            created_at: 2000,
        };
        store.create_snapshot(&snap1).unwrap();
        store.create_snapshot(&snap2).unwrap();
        assert_eq!(store.get_sst_refcount("shared-sst").unwrap(), 2);

        store.delete_snapshot("s1").unwrap();
        assert_eq!(store.get_sst_refcount("shared-sst").unwrap(), 1);

        store.delete_snapshot("s2").unwrap();
        assert_eq!(store.get_sst_refcount("shared-sst").unwrap(), 0);
    }

    // ── Storage config ──────────────────────────────────────────────

    #[test]
    fn set_and_get_storage_config() {
        let (_dir, store) = temp_store();
        let config = StorageConfig {
            s3_endpoint: "https://s3.example.com".to_string(),
            s3_bucket: "my-bucket".to_string(),
            s3_access_key: "AKID".to_string(),
            s3_secret_key: "secret".to_string(),
            cache_disk_path: "/dev/nvme1n1".to_string(),
            cache_disk_size_gb: 200,
            cache_memory_size_gb: 8,
        };
        store.set_storage_config("eu-west", &config).unwrap();
        let got = store.get_storage_config("eu-west").unwrap().unwrap();
        assert_eq!(got.s3_bucket, "my-bucket");
    }

    #[test]
    fn get_missing_storage_config() {
        let (_dir, store) = temp_store();
        assert!(store.get_storage_config("nope").unwrap().is_none());
    }

    // ── Storage quotas ──────────────────────────────────────────────

    #[test]
    fn set_and_get_storage_quota() {
        let (_dir, store) = temp_store();
        let quota = StorageQuota {
            max_volumes: 50,
            max_total_gb: 10_000,
            max_snapshots: 200,
        };
        store.set_storage_quota("org:acme", &quota).unwrap();
        let got = store.get_storage_quota("org:acme").unwrap().unwrap();
        assert_eq!(got, quota);
    }

    #[test]
    fn delete_storage_quota() {
        let (_dir, store) = temp_store();
        let quota = StorageQuota {
            max_volumes: 10,
            max_total_gb: 1000,
            max_snapshots: 50,
        };
        store.set_storage_quota("org:acme", &quota).unwrap();
        store.delete_storage_quota("org:acme").unwrap();
        assert!(store.get_storage_quota("org:acme").unwrap().is_none());
    }

    // ── Manifest pointers ───────────────────────────────────────────

    #[test]
    fn set_and_get_manifest_pointer() {
        let (_dir, store) = temp_store();
        let ptr = ManifestPointer {
            volume_id: "vol-01".to_string(),
            generation: 5,
            manifest_version: 3,
            s3_key: "manifests/vol-01/gen-5.json".to_string(),
        };
        store.set_manifest_pointer("vol-01", &ptr).unwrap();
        let got = store.get_manifest_pointer("vol-01").unwrap().unwrap();
        assert_eq!(got, ptr);
    }

    #[test]
    fn delete_manifest_pointer() {
        let (_dir, store) = temp_store();
        let ptr = ManifestPointer {
            volume_id: "vol-01".to_string(),
            generation: 1,
            manifest_version: 1,
            s3_key: "manifests/vol-01/gen-1.json".to_string(),
        };
        store.set_manifest_pointer("vol-01", &ptr).unwrap();
        store.delete_manifest_pointer("vol-01").unwrap();
        assert!(store.get_manifest_pointer("vol-01").unwrap().is_none());
    }

    // ── Delete volume cleans up index + manifest ────────────────────

    #[test]
    fn delete_volume_cleans_env_index() {
        let (_dir, store) = temp_store();
        store.create_volume(&sample_volume("v1", "prod")).unwrap();
        store.create_volume(&sample_volume("v2", "prod")).unwrap();
        store.delete_volume("v1").unwrap();
        let prod = store.list_volumes_by_env("prod").unwrap();
        assert_eq!(prod.len(), 1);
        assert_eq!(prod[0].id, "v2");
    }

    #[test]
    fn delete_attached_volume_cleans_hypervisor_index() {
        let (_dir, store) = temp_store();
        let mut vol = sample_volume("v1", "prod");
        vol.state = VolumeState::Attached;
        vol.attached_vm_id = Some("vm-1".to_string());
        vol.attached_hypervisor_id = Some("hv-1".to_string());
        // Manually create with attached state for this test.
        store.db.set(VOLUMES_TABLE, "v1", &vol).unwrap();
        store.add_to_env_index("prod", "v1").unwrap();
        store.add_to_hypervisor_index("hv-1", "v1").unwrap();

        store.delete_volume("v1").unwrap();
        let vols = store.list_volumes_by_hypervisor("hv-1").unwrap();
        assert!(vols.is_empty());
    }

    // ── Serde roundtrip ─────────────────────────────────────────────

    #[test]
    fn volume_serde_roundtrip() {
        let vol = sample_volume("vol-01", "prod");
        let json = serde_json::to_string(&vol).unwrap();
        let parsed: Volume = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, vol);
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = sample_snapshot("snap-01", "vol-01");
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, snap);
    }

    #[test]
    fn manifest_pointer_serde_roundtrip() {
        let ptr = ManifestPointer {
            volume_id: "v1".to_string(),
            generation: 42,
            manifest_version: 7,
            s3_key: "manifests/v1/gen-42.json".to_string(),
        };
        let json = serde_json::to_string(&ptr).unwrap();
        let parsed: ManifestPointer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ptr);
    }

    #[test]
    fn storage_quota_serde_roundtrip() {
        let quota = StorageQuota {
            max_volumes: 50,
            max_total_gb: 10_000,
            max_snapshots: 200,
        };
        let json = serde_json::to_string(&quota).unwrap();
        let parsed: StorageQuota = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, quota);
    }
}
