use std::time::{SystemTime, UNIX_EPOCH};

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{
    Ipv4Cidr, PeeringId, PeeringStatus, Subnet, SubnetId, Vpc, VpcId, VpcOwner, VpcPeering,
};
use crate::validation::validate_name;

const VPCS_TABLE: &str = "vpcs";
const SUBNETS_TABLE: &str = "subnets";
const PEERINGS_TABLE: &str = "vpc_peerings";
const VNI_COUNTER_KEY: &str = "vni_counter";

/// Persistent store for VPCs, subnets, and peerings backed by redb.
pub struct VpcStore {
    db: LayerDb,
}

impl VpcStore {
    /// Create a new `VpcStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    // ── VNI allocation ──────────────────────────────────────────────

    /// Allocate the next VNI. Starts at 100, monotonically increasing.
    fn next_vni(&self) -> Result<u32> {
        let current = self
            .db
            .get_metric(VNI_COUNTER_KEY)
            .map_err(|e| OrgError::StoreError(e.to_string()))?;
        let vni = if current == 0 {
            100
        } else {
            current as u32 + 1
        };
        self.db
            .set_metric(VNI_COUNTER_KEY, vni as u64)
            .map_err(|e| OrgError::StoreError(e.to_string()))?;
        Ok(vni)
    }

    // ── VPC CRUD ────────────────────────────────────────────────────

    /// Create a new VPC. Validates the name, allocates a VNI.
    pub fn create_vpc(
        &self,
        name: &str,
        cidr: Ipv4Cidr,
        owner: VpcOwner,
        shared: bool,
    ) -> Result<Vpc> {
        validate_name(name, "vpc")?;

        if self.db.exists(VPCS_TABLE, name)? {
            return Err(OrgError::VpcAlreadyExists(name.to_string()));
        }

        let vni = self.next_vni()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let vpc = Vpc {
            id: VpcId(format!("vpc-{name}")),
            name: name.to_string(),
            cidr,
            vni,
            owner,
            shared,
            created_at: now,
        };

        self.db.set(VPCS_TABLE, name, &vpc)?;
        Ok(vpc)
    }

    /// Get a VPC by name.
    pub fn get_vpc(&self, name: &str) -> Result<Option<Vpc>> {
        Ok(self.db.get(VPCS_TABLE, name)?)
    }

    /// List all VPCs.
    pub fn list_vpcs(&self) -> Result<Vec<Vpc>> {
        let entries: Vec<(String, Vpc)> = self.db.list(VPCS_TABLE)?;
        Ok(entries.into_iter().map(|(_, vpc)| vpc).collect())
    }

    /// Delete a VPC by name. Enforces deletion guards:
    /// 1. Cannot delete if subnets reference this VPC.
    /// 2. Cannot delete if peerings reference this VPC.
    /// 3. Cannot delete if VMs exist in subnets of this VPC.
    pub fn delete_vpc(&self, name: &str) -> Result<()> {
        let vpc = self
            .db
            .get::<Vpc>(VPCS_TABLE, name)?
            .ok_or_else(|| OrgError::VpcNotFound(name.to_string()))?;

        // Guard 1: check subnets
        let subnets = self.list_subnets_for_vpc(&vpc.id)?;
        if !subnets.is_empty() {
            return Err(OrgError::VpcHasSubnets {
                name: name.to_string(),
                count: subnets.len(),
            });
        }

        // Guard 2: check peerings
        let peerings = self.list_active_peerings_for_vpc(&vpc.id)?;
        if !peerings.is_empty() {
            return Err(OrgError::VpcHasPeerings {
                name: name.to_string(),
                count: peerings.len(),
            });
        }

        // Guard 3: check VMs in subnets of this VPC.
        // Subnets are already empty (guard 1 passed), so no VMs can exist.
        // This guard is for future use when VM tracking is wired.
        // Placeholder: always returns 0 VMs since we check subnets first.
        let vm_count = self.count_vms_in_vpc(&vpc.id)?;
        if vm_count > 0 {
            return Err(OrgError::VpcHasVms {
                name: name.to_string(),
                count: vm_count,
            });
        }

        self.db.delete(VPCS_TABLE, name)?;
        Ok(())
    }

    // ── Subnet operations ───────────────────────────────────────────

    /// Create a subnet within a VPC.
    pub fn create_subnet(&self, subnet: &Subnet) -> Result<()> {
        let key = subnet.id.0.clone();
        self.db.set(SUBNETS_TABLE, &key, subnet)?;
        Ok(())
    }

    /// List all subnets belonging to a VPC.
    pub fn list_subnets_for_vpc(&self, vpc_id: &VpcId) -> Result<Vec<Subnet>> {
        let all: Vec<(String, Subnet)> = self.db.list(SUBNETS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(_, s)| s.vpc_id == *vpc_id)
            .map(|(_, s)| s)
            .collect())
    }

    /// Delete a subnet by ID.
    pub fn delete_subnet(&self, subnet_id: &SubnetId) -> Result<()> {
        self.db.delete(SUBNETS_TABLE, &subnet_id.0)?;
        Ok(())
    }

    // ── Peering operations ──────────────────────────────────────────

    /// Create a VPC peering.
    pub fn create_peering(&self, peering: &VpcPeering) -> Result<()> {
        let key = peering.id.0.clone();
        self.db.set(PEERINGS_TABLE, &key, peering)?;
        Ok(())
    }

    /// List all active peerings that reference a given VPC.
    pub fn list_active_peerings_for_vpc(&self, vpc_id: &VpcId) -> Result<Vec<VpcPeering>> {
        let all: Vec<(String, VpcPeering)> = self.db.list(PEERINGS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(_, p)| {
                (p.vpc_a == *vpc_id || p.vpc_b == *vpc_id) && p.status == PeeringStatus::Active
            })
            .map(|(_, p)| p)
            .collect())
    }

    /// Delete a peering by ID.
    pub fn delete_peering(&self, peering_id: &PeeringId) -> Result<()> {
        self.db.delete(PEERINGS_TABLE, &peering_id.0)?;
        Ok(())
    }

    // ── VM guard (placeholder) ──────────────────────────────────────

    /// Count VMs in all subnets of a VPC.
    ///
    /// Placeholder: subnets and compute are not yet wired together.
    /// When the compute layer tracks VM-to-subnet assignments, this
    /// method will query the VM table for entries whose subnet belongs
    /// to this VPC. For now, it always returns 0.
    fn count_vms_in_vpc(&self, _vpc_id: &VpcId) -> Result<usize> {
        // TODO: wire to compute layer VM tracking
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EnvironmentId, OrgId, ProjectId};
    use std::net::Ipv4Addr;

    fn temp_store() -> (tempfile::TempDir, VpcStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpc-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, VpcStore::new(db))
    }

    fn sample_cidr() -> Ipv4Cidr {
        Ipv4Cidr {
            addr: Ipv4Addr::new(10, 1, 0, 0),
            prefix_len: 16,
        }
    }

    fn sample_owner() -> VpcOwner {
        VpcOwner::Project(ProjectId("acme/backend".to_string()))
    }

    fn make_subnet(vpc_id: &VpcId, name: &str) -> Subnet {
        Subnet {
            id: SubnetId(format!("subnet-{name}")),
            name: name.to_string(),
            vpc_id: vpc_id.clone(),
            env_id: EnvironmentId("acme/backend/production".to_string()),
            cidr: Ipv4Cidr {
                addr: Ipv4Addr::new(10, 1, 1, 0),
                prefix_len: 24,
            },
            gateway: Ipv4Addr::new(10, 1, 1, 1),
            created_at: 1000,
        }
    }

    fn make_peering(vpc_a: &VpcId, vpc_b: &VpcId) -> VpcPeering {
        VpcPeering {
            id: PeeringId(format!("peer-{}-{}", vpc_a, vpc_b)),
            vpc_a: vpc_a.clone(),
            vpc_b: vpc_b.clone(),
            status: PeeringStatus::Active,
            created_at: 1000,
        }
    }

    // ── Basic VPC CRUD ──────────────────────────────────────────────

    #[test]
    fn create_vpc_basic() {
        let (_dir, store) = temp_store();
        let vpc = store
            .create_vpc("default", sample_cidr(), sample_owner(), false)
            .unwrap();

        assert_eq!(vpc.name, "default");
        assert_eq!(vpc.id.0, "vpc-default");
        assert_eq!(vpc.vni, 100);
        assert!(!vpc.shared);
    }

    #[test]
    fn vni_increments() {
        let (_dir, store) = temp_store();
        let v1 = store
            .create_vpc("vpc-one", sample_cidr(), sample_owner(), false)
            .unwrap();
        let v2 = store
            .create_vpc("vpc-two", sample_cidr(), sample_owner(), false)
            .unwrap();

        assert_eq!(v1.vni, 100);
        assert_eq!(v2.vni, 101);
    }

    #[test]
    fn duplicate_vpc_rejected() {
        let (_dir, store) = temp_store();
        store
            .create_vpc("default", sample_cidr(), sample_owner(), false)
            .unwrap();

        let err = store
            .create_vpc("default", sample_cidr(), sample_owner(), false)
            .unwrap_err();
        assert!(matches!(err, OrgError::VpcAlreadyExists(_)));
    }

    // ── Deletion guards ─────────────────────────────────────────────

    #[test]
    fn delete_empty_ok() {
        let (_dir, store) = temp_store();
        store
            .create_vpc("default", sample_cidr(), sample_owner(), false)
            .unwrap();

        store.delete_vpc("default").unwrap();
        assert!(store.get_vpc("default").unwrap().is_none());
    }

    #[test]
    fn delete_with_subnets_rejected() {
        let (_dir, store) = temp_store();
        let vpc = store
            .create_vpc("default", sample_cidr(), sample_owner(), false)
            .unwrap();

        // Add 3 subnets
        for name in &["frontend", "backend", "database"] {
            store.create_subnet(&make_subnet(&vpc.id, name)).unwrap();
        }

        let err = store.delete_vpc("default").unwrap_err();
        match &err {
            OrgError::VpcHasSubnets { name, count } => {
                assert_eq!(name, "default");
                assert_eq!(*count, 3);
            }
            other => panic!("expected VpcHasSubnets, got: {other}"),
        }

        // Verify error message is clear
        let msg = err.to_string();
        assert!(
            msg.contains("cannot delete vpc 'default': has 3 active subnet(s)"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn delete_with_peerings_rejected() {
        let (_dir, store) = temp_store();
        let vpc_a = store
            .create_vpc("vpc-hub", sample_cidr(), sample_owner(), false)
            .unwrap();
        let vpc_b = store
            .create_vpc(
                "vpc-spoke",
                Ipv4Cidr {
                    addr: Ipv4Addr::new(10, 2, 0, 0),
                    prefix_len: 16,
                },
                sample_owner(),
                false,
            )
            .unwrap();

        store
            .create_peering(&make_peering(&vpc_a.id, &vpc_b.id))
            .unwrap();

        let err = store.delete_vpc("vpc-hub").unwrap_err();
        match &err {
            OrgError::VpcHasPeerings { name, count } => {
                assert_eq!(name, "vpc-hub");
                assert_eq!(*count, 1);
            }
            other => panic!("expected VpcHasPeerings, got: {other}"),
        }

        let msg = err.to_string();
        assert!(
            msg.contains("cannot delete vpc 'vpc-hub': has 1 active peering(s)"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn delete_nonexistent_vpc_fails() {
        let (_dir, store) = temp_store();
        let err = store.delete_vpc("ghost").unwrap_err();
        assert!(matches!(err, OrgError::VpcNotFound(_)));
    }

    #[test]
    fn delete_after_removing_subnets_ok() {
        let (_dir, store) = temp_store();
        let vpc = store
            .create_vpc("default", sample_cidr(), sample_owner(), false)
            .unwrap();

        let subnet = make_subnet(&vpc.id, "frontend");
        store.create_subnet(&subnet).unwrap();

        // Cannot delete yet
        assert!(store.delete_vpc("default").is_err());

        // Remove the subnet
        store.delete_subnet(&subnet.id).unwrap();

        // Now it works
        store.delete_vpc("default").unwrap();
        assert!(store.get_vpc("default").unwrap().is_none());
    }

    #[test]
    fn delete_after_removing_peerings_ok() {
        let (_dir, store) = temp_store();
        let vpc_a = store
            .create_vpc("vpc-hub", sample_cidr(), sample_owner(), false)
            .unwrap();
        let vpc_b = store
            .create_vpc(
                "vpc-spoke",
                Ipv4Cidr {
                    addr: Ipv4Addr::new(10, 2, 0, 0),
                    prefix_len: 16,
                },
                sample_owner(),
                false,
            )
            .unwrap();

        let peering = make_peering(&vpc_a.id, &vpc_b.id);
        store.create_peering(&peering).unwrap();

        // Cannot delete yet
        assert!(store.delete_vpc("vpc-hub").is_err());

        // Remove the peering
        store.delete_peering(&peering.id).unwrap();

        // Now it works
        store.delete_vpc("vpc-hub").unwrap();
    }

    #[test]
    fn list_vpcs() {
        let (_dir, store) = temp_store();
        store
            .create_vpc("alpha", sample_cidr(), sample_owner(), false)
            .unwrap();
        store
            .create_vpc(
                "beta",
                Ipv4Cidr {
                    addr: Ipv4Addr::new(10, 2, 0, 0),
                    prefix_len: 16,
                },
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        let vpcs = store.list_vpcs().unwrap();
        assert_eq!(vpcs.len(), 2);
    }
}
