//! Hypervisor persistence — CRUD for the hypervisor compute host model.
//!
//! Backed by a redb table `hypervisors` with key = hypervisor name.

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{AllocatableCapacity, Hypervisor, HypervisorId, HypervisorState};

const TABLE: &str = "hypervisors";

/// Persistent store for hypervisors backed by redb.
pub struct HypervisorStore {
    db: LayerDb,
}

impl HypervisorStore {
    /// Create a new `HypervisorStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Create a new hypervisor record. Fails if the name is already taken.
    pub fn create(&self, hv: &Hypervisor) -> Result<()> {
        if self.db.exists(TABLE, &hv.name)? {
            return Err(OrgError::AlreadyExists(hv.name.clone()));
        }
        self.db.set(TABLE, &hv.name, hv)?;
        Ok(())
    }

    /// Get a hypervisor by name. Returns `None` if not found.
    pub fn get(&self, name: &str) -> Result<Option<Hypervisor>> {
        Ok(self.db.get(TABLE, name)?)
    }

    /// Get a hypervisor by its ID. Scans all records.
    pub fn get_by_id(&self, id: &HypervisorId) -> Result<Option<Hypervisor>> {
        let all = self.list()?;
        Ok(all.into_iter().find(|h| h.id == *id))
    }

    /// Get a hypervisor by fabric_node_id. Used for identity recovery on restart.
    pub fn get_by_fabric_node_id(&self, fabric_node_id: &str) -> Result<Option<Hypervisor>> {
        let all = self.list()?;
        Ok(all.into_iter().find(|h| h.fabric_node_id == fabric_node_id))
    }

    /// List all hypervisors.
    pub fn list(&self) -> Result<Vec<Hypervisor>> {
        let entries: Vec<(String, Hypervisor)> = self.db.list(TABLE)?;
        Ok(entries.into_iter().map(|(_, hv)| hv).collect())
    }

    /// List hypervisors filtered by region.
    pub fn list_by_region(&self, region: &str) -> Result<Vec<Hypervisor>> {
        let all = self.list()?;
        Ok(all.into_iter().filter(|h| h.region == region).collect())
    }

    /// List hypervisors filtered by zone.
    pub fn list_by_zone(&self, zone: &str) -> Result<Vec<Hypervisor>> {
        let all = self.list()?;
        Ok(all.into_iter().filter(|h| h.zone == zone).collect())
    }

    /// Delete a hypervisor by name.
    pub fn delete(&self, name: &str) -> Result<()> {
        if !self.db.exists(TABLE, name)? {
            return Err(OrgError::NotFound(format!("hypervisor '{name}'")));
        }
        self.db.delete(TABLE, name)?;
        Ok(())
    }

    /// Update the hypervisor state. Enforces valid state transitions.
    pub fn update_state(&self, name: &str, new_state: HypervisorState) -> Result<()> {
        let mut hv = self
            .get(name)?
            .ok_or_else(|| OrgError::NotFound(format!("hypervisor '{name}'")))?;

        validate_state_transition(&hv.state, &new_state)?;
        hv.state = new_state;
        self.db.set(TABLE, name, &hv)?;
        Ok(())
    }

    /// Update the allocatable capacity for a hypervisor.
    pub fn update_capacity(&self, name: &str, capacity: AllocatableCapacity) -> Result<()> {
        let mut hv = self
            .get(name)?
            .ok_or_else(|| OrgError::NotFound(format!("hypervisor '{name}'")))?;
        hv.capacity = capacity;
        self.db.set(TABLE, name, &hv)?;
        Ok(())
    }

    /// Update the full hypervisor record (used for re-probe on restart).
    pub fn update(&self, hv: &Hypervisor) -> Result<()> {
        if !self.db.exists(TABLE, &hv.name)? {
            return Err(OrgError::NotFound(format!("hypervisor '{}'", hv.name)));
        }
        self.db.set(TABLE, &hv.name, hv)?;
        Ok(())
    }
}

/// Validate a hypervisor state transition per ADR-004.
fn validate_state_transition(from: &HypervisorState, to: &HypervisorState) -> Result<()> {
    use HypervisorState::*;

    let valid = matches!(
        (from, to),
        (Registering, NotReady)
            | (NotReady, Available)
            | (Available, Draining)
            | (Available, Maintenance)
            | (Available, Decommissioned)
            | (Draining, Available)
            | (Draining, Maintenance)
            | (Maintenance, Available)
            | (Maintenance, Decommissioned)
    );

    if !valid {
        return Err(OrgError::InvalidStateTransition {
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use std::collections::HashMap;

    fn temp_store() -> (tempfile::TempDir, HypervisorStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, HypervisorStore::new(db))
    }

    fn test_hypervisor(name: &str) -> Hypervisor {
        Hypervisor {
            id: HypervisorId(format!("hv-test-{name}")),
            name: name.to_string(),
            region: "eu-west".to_string(),
            zone: "eu-west-1".to_string(),
            state: HypervisorState::NotReady,
            fabric_node_id: format!("node-{name}"),
            public_ip: "203.0.113.10".to_string(),
            fabric_ipv6: "fd12::1".to_string(),
            hardware: HardwareSpec {
                cpu_model: "AMD EPYC 7763".to_string(),
                cpu_cores_physical: 64,
                cpu_threads_logical: 128,
                memory_gb: 256,
                local_disk_type: DiskType::NVMe,
                local_disk_gb: 1920,
                gpu: None,
                network_bandwidth_gbps: 25,
                architecture: CpuArchitecture::X86_64,
            },
            capacity: AllocatableCapacity::default(),
            labels: HashMap::new(),
            taints: vec![],
            created_at: 1700000000,
        }
    }

    #[test]
    fn create_and_get() {
        let (_dir, store) = temp_store();
        let hv = test_hypervisor("hv-001");
        store.create(&hv).unwrap();

        let got = store.get("hv-001").unwrap().unwrap();
        assert_eq!(got.id, hv.id);
        assert_eq!(got.name, "hv-001");
    }

    #[test]
    fn create_duplicate_fails() {
        let (_dir, store) = temp_store();
        let hv = test_hypervisor("hv-001");
        store.create(&hv).unwrap();
        assert!(store.create(&hv).is_err());
    }

    #[test]
    fn list_all() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();
        store.create(&test_hypervisor("hv-002")).unwrap();
        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn delete() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();
        store.delete("hv-001").unwrap();
        assert!(store.get("hv-001").unwrap().is_none());
    }

    #[test]
    fn update_state_valid() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();

        // NotReady -> Available
        store
            .update_state("hv-001", HypervisorState::Available)
            .unwrap();
        let hv = store.get("hv-001").unwrap().unwrap();
        assert_eq!(hv.state, HypervisorState::Available);
    }

    #[test]
    fn update_state_invalid() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();

        // NotReady -> Draining is invalid
        let result = store.update_state("hv-001", HypervisorState::Draining);
        assert!(result.is_err());
    }

    #[test]
    fn update_capacity() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();

        let cap = AllocatableCapacity {
            used_vcpus: 4,
            used_memory_mb: 8192,
            ..AllocatableCapacity::default()
        };
        store.update_capacity("hv-001", cap.clone()).unwrap();

        let hv = store.get("hv-001").unwrap().unwrap();
        assert_eq!(hv.capacity.used_vcpus, 4);
    }

    #[test]
    fn get_by_fabric_node_id() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();

        let found = store.get_by_fabric_node_id("node-hv-001").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "hv-001");

        let not_found = store.get_by_fabric_node_id("nonexistent").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn list_by_region() {
        let (_dir, store) = temp_store();
        store.create(&test_hypervisor("hv-001")).unwrap();
        let mut hv2 = test_hypervisor("hv-002");
        hv2.region = "us-east".to_string();
        store.create(&hv2).unwrap();

        let eu = store.list_by_region("eu-west").unwrap();
        assert_eq!(eu.len(), 1);
        assert_eq!(eu[0].name, "hv-001");
    }

    #[test]
    fn state_transition_decommissioned_is_terminal() {
        let (_dir, store) = temp_store();
        let mut hv = test_hypervisor("hv-001");
        hv.state = HypervisorState::Decommissioned;
        store.create(&hv).unwrap();

        // Decommissioned -> Available is invalid
        let result = store.update_state("hv-001", HypervisorState::Available);
        assert!(result.is_err());
    }
}
