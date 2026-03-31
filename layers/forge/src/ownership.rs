//! Ownership registry — tracks which resources are managed by Forge.
//!
//! Uses [`syfrah_state::LayerDb`] to map resource_id to ownership records.
//! Implements a 3-tier orphan policy: known -> manage, suspected -> quarantine,
//! unknown -> ignore.

use serde::{Deserialize, Serialize};
use syfrah_state::LayerDb;

const TABLE: &str = "ownership_registry";

/// Resource ownership record.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OwnershipRecord {
    /// Resource type (e.g., "vm", "bridge", "tap").
    pub resource_type: String,
    /// Kernel-visible name (e.g., "br-vpc-xxx", "tap-vm-yyy").
    pub kernel_name: String,
    /// Unix timestamp of registration.
    pub created_at: u64,
}

/// Orphan classification policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanPolicy {
    /// Known resource — manage normally.
    Known,
    /// Suspected orphan — quarantine for review.
    Suspected,
    /// Unknown resource — ignore (not ours).
    Unknown,
}

/// Ownership registry backed by LayerDb.
pub struct OwnershipRegistry {
    db: LayerDb,
}

impl OwnershipRegistry {
    /// Create a registry using the given LayerDb.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Register a resource.
    pub fn register(
        &self,
        resource_id: &str,
        resource_type: &str,
        kernel_name: &str,
    ) -> Result<(), syfrah_state::StateError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let record = OwnershipRecord {
            resource_type: resource_type.to_string(),
            kernel_name: kernel_name.to_string(),
            created_at: now,
        };

        self.db.set(TABLE, resource_id, &record)
    }

    /// Deregister a resource.
    pub fn deregister(&self, resource_id: &str) -> Result<bool, syfrah_state::StateError> {
        self.db.delete(TABLE, resource_id)
    }

    /// Look up a resource.
    pub fn lookup(
        &self,
        resource_id: &str,
    ) -> Result<Option<OwnershipRecord>, syfrah_state::StateError> {
        self.db.get(TABLE, resource_id)
    }

    /// List all registered resources.
    pub fn list_all(&self) -> Result<Vec<(String, OwnershipRecord)>, syfrah_state::StateError> {
        self.db.list(TABLE)
    }

    /// List resources filtered by type (e.g., "vm", "bridge").
    pub fn list_by_type(
        &self,
        resource_type: &str,
    ) -> Result<Vec<(String, OwnershipRecord)>, syfrah_state::StateError> {
        let all = self.list_all()?;
        Ok(all
            .into_iter()
            .filter(|(_, r)| r.resource_type == resource_type)
            .collect())
    }

    /// Rebuild the registry from a list of known resources.
    pub fn rebuild(
        &self,
        resources: &[(String, String, String)], // (id, type, kernel_name)
    ) -> Result<usize, syfrah_state::StateError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Clear existing entries.
        let existing: Vec<(String, OwnershipRecord)> = self.db.list(TABLE)?;
        for (key, _) in &existing {
            self.db.delete(TABLE, key)?;
        }

        // Insert new entries.
        for (id, rtype, kname) in resources {
            let record = OwnershipRecord {
                resource_type: rtype.clone(),
                kernel_name: kname.clone(),
                created_at: now,
            };
            self.db.set(TABLE, id, &record)?;
        }

        Ok(resources.len())
    }

    /// Classify a kernel resource against the registry.
    pub fn classify(&self, kernel_name: &str) -> Result<OrphanPolicy, syfrah_state::StateError> {
        let all: Vec<(String, OwnershipRecord)> = self.db.list(TABLE)?;

        for (_, record) in &all {
            if record.kernel_name == kernel_name {
                return Ok(OrphanPolicy::Known);
            }
        }

        // Check if the name looks like a Syfrah resource.
        if kernel_name.starts_with("br-")
            || kernel_name.starts_with("tap-")
            || kernel_name.starts_with("vx-")
        {
            return Ok(OrphanPolicy::Suspected);
        }

        Ok(OrphanPolicy::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry() -> (tempfile::TempDir, OwnershipRegistry) {
        let dir = tempfile::tempdir().unwrap();
        let db = LayerDb::open_at(&dir.path().join("ownership.redb")).unwrap();
        (dir, OwnershipRegistry::new(db))
    }

    #[test]
    fn register_and_lookup() {
        let (_dir, reg) = temp_registry();
        reg.register("vm-1", "vm", "ch-vm-1").unwrap();
        let record = reg.lookup("vm-1").unwrap().unwrap();
        assert_eq!(record.resource_type, "vm");
        assert_eq!(record.kernel_name, "ch-vm-1");
    }

    #[test]
    fn deregister() {
        let (_dir, reg) = temp_registry();
        reg.register("vm-2", "vm", "ch-vm-2").unwrap();
        assert!(reg.deregister("vm-2").unwrap());
        assert!(reg.lookup("vm-2").unwrap().is_none());
    }

    #[test]
    fn list_all() {
        let (_dir, reg) = temp_registry();
        reg.register("vm-1", "vm", "ch-1").unwrap();
        reg.register("br-1", "bridge", "br-vpc-1").unwrap();
        let all = reg.list_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn classify_orphan_policy() {
        let (_dir, reg) = temp_registry();
        reg.register("vm-1", "vm", "ch-vm-1").unwrap();

        assert_eq!(reg.classify("ch-vm-1").unwrap(), OrphanPolicy::Known);
        assert_eq!(reg.classify("br-unknown").unwrap(), OrphanPolicy::Suspected);
        assert_eq!(reg.classify("eth0").unwrap(), OrphanPolicy::Unknown);
    }

    #[test]
    fn rebuild_replaces_all() {
        let (_dir, reg) = temp_registry();
        reg.register("old-1", "vm", "ch-old").unwrap();

        let resources = vec![
            (
                "new-1".to_string(),
                "vm".to_string(),
                "ch-new-1".to_string(),
            ),
            (
                "new-2".to_string(),
                "bridge".to_string(),
                "br-new".to_string(),
            ),
        ];
        let count = reg.rebuild(&resources).unwrap();
        assert_eq!(count, 2);

        assert!(reg.lookup("old-1").unwrap().is_none());
        assert!(reg.lookup("new-1").unwrap().is_some());
    }
}
