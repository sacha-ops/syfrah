//! Network Interface (NIC) persistence — tracks NICs and their SG attachments.
//!
//! Backed by a redb table `network_interfaces` with key `nic_id`.

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{NetworkInterface, ResourceState, SecurityGroupId};

const TABLE: &str = "network_interfaces";

/// Persistent store for network interfaces backed by redb.
pub struct NicStore {
    db: LayerDb,
}

impl NicStore {
    /// Create a new `NicStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Create a new network interface. Returns an error if a NIC with the same
    /// ID already exists.
    pub fn create_nic(&self, nic: &NetworkInterface) -> Result<()> {
        if self.db.exists(TABLE, &nic.id.0)? {
            return Err(OrgError::NicAlreadyExists(nic.id.0.clone()));
        }
        self.db.set(TABLE, &nic.id.0, nic)?;
        Ok(())
    }

    /// Get a NIC by its ID. Returns `None` if not found.
    pub fn get_nic(&self, nic_id: &str) -> Result<Option<NetworkInterface>> {
        Ok(self.db.get(TABLE, nic_id)?)
    }

    /// List all NICs attached to a given VM.
    pub fn list_nics_by_vm(&self, vm_id: &str) -> Result<Vec<NetworkInterface>> {
        let entries: Vec<(String, NetworkInterface)> = self.db.list(TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, nic)| nic.vm_id.as_deref() == Some(vm_id))
            .map(|(_, nic)| nic)
            .collect())
    }

    /// List all NICs in a given subnet.
    pub fn list_nics_by_subnet(&self, subnet_id: &str) -> Result<Vec<NetworkInterface>> {
        let entries: Vec<(String, NetworkInterface)> = self.db.list(TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, nic)| nic.subnet_id == subnet_id)
            .map(|(_, nic)| nic)
            .collect())
    }

    /// Delete a NIC by its ID. Transitions the NIC to `Deleted` state.
    /// Returns an error if the NIC does not exist.
    pub fn delete_nic(&self, nic_id: &str) -> Result<()> {
        let nic: Option<NetworkInterface> = self.db.get(TABLE, nic_id)?;
        match nic {
            Some(mut nic) => {
                nic.state = ResourceState::Deleted;
                self.db.set(TABLE, nic_id, &nic)?;
                Ok(())
            }
            None => Err(OrgError::NicNotFound(nic_id.to_string())),
        }
    }

    /// Attach a security group to a NIC. No-op if the SG is already attached.
    pub fn attach_sg_to_nic(&self, nic_id: &str, sg_id: &SecurityGroupId) -> Result<()> {
        let nic: Option<NetworkInterface> = self.db.get(TABLE, nic_id)?;
        match nic {
            Some(mut nic) => {
                if !nic.security_groups.contains(sg_id) {
                    nic.security_groups.push(sg_id.clone());
                    self.db.set(TABLE, nic_id, &nic)?;
                }
                Ok(())
            }
            None => Err(OrgError::NicNotFound(nic_id.to_string())),
        }
    }

    /// Detach a security group from a NIC. Returns an error if the SG is not
    /// currently attached.
    pub fn detach_sg_from_nic(&self, nic_id: &str, sg_id: &SecurityGroupId) -> Result<()> {
        let nic: Option<NetworkInterface> = self.db.get(TABLE, nic_id)?;
        match nic {
            Some(mut nic) => {
                let before = nic.security_groups.len();
                nic.security_groups.retain(|id| id != sg_id);
                if nic.security_groups.len() == before {
                    return Err(OrgError::SgNotAttached {
                        nic: nic_id.to_string(),
                        sg: sg_id.0.clone(),
                    });
                }
                self.db.set(TABLE, nic_id, &nic)?;
                Ok(())
            }
            None => Err(OrgError::NicNotFound(nic_id.to_string())),
        }
    }
}
