//! VM placement persistence — tracks VM-to-node-to-subnet mapping.
//!
//! Backed by a redb table `vm_placements` with composite key `"{vpc_id}/{vm_id}"`.

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::VmPlacement;

const TABLE: &str = "vm_placements";

/// Persistent store for VM placements backed by redb.
pub struct PlacementStore {
    db: LayerDb,
}

impl PlacementStore {
    /// Create a new `PlacementStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Build the composite key for a placement entry.
    fn key(vpc_id: &str, vm_id: &str) -> String {
        format!("{vpc_id}/{vm_id}")
    }

    /// Add a VM placement record. Overwrites any existing entry for the same
    /// vpc_id/vm_id pair.
    pub fn add_placement(&self, placement: &VmPlacement) -> Result<()> {
        let k = Self::key(&placement.vpc_id, &placement.vm_id);
        self.db.set(TABLE, &k, placement)?;
        Ok(())
    }

    /// Remove a VM placement record. Returns an error if the entry does not exist.
    pub fn remove_placement(&self, vpc_id: &str, vm_id: &str) -> Result<()> {
        let k = Self::key(vpc_id, vm_id);
        let existed = self.db.delete(TABLE, &k)?;
        if !existed {
            return Err(OrgError::NotFound(format!("placement {vpc_id}/{vm_id}")));
        }
        Ok(())
    }

    /// Get a single placement by vpc_id and vm_id. Returns `None` if not found.
    pub fn get_placement(&self, vpc_id: &str, vm_id: &str) -> Result<Option<VmPlacement>> {
        let k = Self::key(vpc_id, vm_id);
        Ok(self.db.get(TABLE, &k)?)
    }

    /// List all placements for a given VPC.
    pub fn list_by_vpc(&self, vpc_id: &str) -> Result<Vec<VmPlacement>> {
        let entries: Vec<(String, VmPlacement)> = self.db.list(TABLE)?;
        let prefix = format!("{vpc_id}/");
        Ok(entries
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, p)| p)
            .collect())
    }

    /// List all placements across all VPCs and nodes.
    ///
    /// Used by the daemon restart recovery path to rebuild the complete
    /// network state from persisted placement records.
    pub fn list_all(&self) -> Result<Vec<VmPlacement>> {
        let entries: Vec<(String, VmPlacement)> = self.db.list(TABLE)?;
        Ok(entries.into_iter().map(|(_, p)| p).collect())
    }

    /// List all placements hosted on a given node (fabric IPv6).
    pub fn list_by_node(&self, hosting_node: &str) -> Result<Vec<VmPlacement>> {
        let entries: Vec<(String, VmPlacement)> = self.db.list(TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, p)| p.hosting_node == hosting_node)
            .map(|(_, p)| p)
            .collect())
    }
}
