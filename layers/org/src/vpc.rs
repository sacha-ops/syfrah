use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{OrgError, Result};
use crate::store::OrgStore;
use crate::types::{OrgId, ProjectId};

/// Unique identifier for a VPC.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct VpcId(pub String);

impl std::fmt::Display for VpcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who owns the VPC — a project or an org (shared).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpcOwner {
    Project(ProjectId),
    Org(OrgId),
}

/// A Virtual Private Cloud — one VXLAN VNI = one isolated L2 domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vpc {
    pub id: VpcId,
    pub name: String,
    pub cidr: String,
    pub vni: u32,
    pub owner: VpcOwner,
    pub shared: bool,
    pub created_at: u64,
}

const VPCS_TABLE: &str = "vpcs";
const VNI_COUNTER_KEY: &str = "vni_counter";

/// Starting VNI value. VNIs are allocated monotonically from this base.
const VNI_BASE: u64 = 100;

/// Auto-allocate the next /16 CIDR based on the VNI.
///
/// Uses the second octet derived from the VNI offset:
///   VNI 100 -> 10.0.0.0/16
///   VNI 101 -> 10.1.0.0/16
///   VNI 102 -> 10.2.0.0/16
fn auto_cidr(vni: u32) -> String {
    let second_octet = (vni - VNI_BASE as u32) % 256;
    let base = Ipv4Addr::new(10, second_octet as u8, 0, 0);
    format!("{}/16", base)
}

impl OrgStore {
    /// Allocate the next VNI, starting from 100.
    fn next_vni(&self) -> Result<u32> {
        // inc_metric returns the new value after incrementing.
        // First call: 0 + 1 = 1, so VNI = 99 + 1 = 100.
        let counter = self.db().inc_metric(VNI_COUNTER_KEY, 1)?;
        Ok((VNI_BASE - 1 + counter) as u32)
    }

    /// Create a VPC explicitly.
    pub fn create_vpc(
        &self,
        name: &str,
        owner: VpcOwner,
        cidr: Option<&str>,
        shared: bool,
    ) -> Result<Vpc> {
        crate::validation::validate_name(name, "vpc")?;

        let vpc_key = self.vpc_key_for_owner(name, &owner);
        if self.db().exists(VPCS_TABLE, &vpc_key)? {
            return Err(OrgError::VpcAlreadyExists(name.to_string()));
        }

        let vni = self.next_vni()?;
        let cidr = cidr
            .map(|c| c.to_string())
            .unwrap_or_else(|| auto_cidr(vni));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let vpc = Vpc {
            id: VpcId(vpc_key.clone()),
            name: name.to_string(),
            cidr,
            vni,
            owner,
            shared,
            created_at: now,
        };

        self.db().set(VPCS_TABLE, &vpc_key, &vpc)?;
        Ok(vpc)
    }

    /// Get a VPC by its storage key.
    pub fn get_vpc(&self, key: &str) -> Result<Option<Vpc>> {
        Ok(self.db().get(VPCS_TABLE, key)?)
    }

    /// List all VPCs.
    pub fn list_vpcs(&self) -> Result<Vec<Vpc>> {
        let entries: Vec<(String, Vpc)> = self.db().list(VPCS_TABLE)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    /// List VPCs owned by a specific project.
    pub fn list_vpcs_for_project(&self, org: &str, project: &str) -> Result<Vec<Vpc>> {
        let prefix = format!("{}/{}/", org, project);
        let entries: Vec<(String, Vpc)> = self.db().list(VPCS_TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect())
    }

    /// Ensure a default VPC exists for the given org/project.
    ///
    /// If a VPC named "default" already exists for this project, returns it.
    /// Otherwise, creates one with an auto-allocated /16 CIDR and a new VNI.
    ///
    /// This is the entry point for auto-creation when the first subnet is
    /// created in a project that has no VPC.
    pub fn ensure_default_vpc(&self, org: &str, project: &str) -> Result<Vpc> {
        let vpc_key = format!("{}/{}/default", org, project);

        // Idempotency: return existing default VPC if present.
        if let Some(existing) = self.db().get::<Vpc>(VPCS_TABLE, &vpc_key)? {
            return Ok(existing);
        }

        // Verify the project exists.
        let project_key = format!("{}/{}", org, project);
        if !self.db().exists("projects", &project_key)? {
            return Err(OrgError::ProjectNotFound {
                org: org.to_string(),
                project: project.to_string(),
            });
        }

        let vni = self.next_vni()?;
        let cidr = auto_cidr(vni);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let owner = VpcOwner::Project(ProjectId(project_key));

        let vpc = Vpc {
            id: VpcId(vpc_key.clone()),
            name: "default".to_string(),
            cidr,
            vni,
            owner,
            shared: false,
            created_at: now,
        };

        self.db().set(VPCS_TABLE, &vpc_key, &vpc)?;
        Ok(vpc)
    }

    /// Build a storage key for a VPC based on its owner.
    fn vpc_key_for_owner(&self, name: &str, owner: &VpcOwner) -> String {
        match owner {
            VpcOwner::Project(pid) => format!("{}/{}", pid.0, name),
            VpcOwner::Org(oid) => format!("{}/{}", oid.0, name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpc-test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();
        (dir, OrgStore::new(db))
    }

    fn setup_org_and_project(store: &OrgStore) {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
    }

    #[test]
    fn auto_create_on_first_subnet() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let vpc = store.ensure_default_vpc("acme", "backend").unwrap();

        assert_eq!(vpc.name, "default");
        assert_eq!(vpc.id.0, "acme/backend/default");
        assert!(!vpc.shared);
        assert!(
            matches!(&vpc.owner, VpcOwner::Project(pid) if pid.0 == "acme/backend"),
            "expected Project owner"
        );
        assert!(vpc.vni >= 100, "VNI must be >= 100, got {}", vpc.vni);
        assert!(vpc.cidr.ends_with("/16"), "CIDR must be a /16");
        assert!(vpc.created_at > 0);
    }

    #[test]
    fn auto_create_idempotent() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let vpc1 = store.ensure_default_vpc("acme", "backend").unwrap();
        let vpc2 = store.ensure_default_vpc("acme", "backend").unwrap();

        assert_eq!(vpc1.id, vpc2.id);
        assert_eq!(vpc1.vni, vpc2.vni);
        assert_eq!(vpc1.cidr, vpc2.cidr);
        assert_eq!(vpc1.name, vpc2.name);
    }

    #[test]
    fn auto_vpc_has_valid_vni() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let vpc = store.ensure_default_vpc("acme", "backend").unwrap();

        assert!(vpc.vni >= 100, "VNI must be >= 100, got {}", vpc.vni);
    }

    #[test]
    fn ensure_default_vpc_fails_without_project() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        let err = store.ensure_default_vpc("acme", "ghost").unwrap_err();
        assert!(matches!(err, OrgError::ProjectNotFound { .. }));
    }

    #[test]
    fn vni_increments_across_vpcs() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "alpha").unwrap();
        store.create_project("acme", "bravo").unwrap();

        let vpc1 = store.ensure_default_vpc("acme", "alpha").unwrap();
        let vpc2 = store.ensure_default_vpc("acme", "bravo").unwrap();

        assert_ne!(vpc1.vni, vpc2.vni, "each VPC must get a unique VNI");
        assert!(
            vpc2.vni > vpc1.vni,
            "VNIs should be monotonically increasing"
        );
    }

    #[test]
    fn explicit_vpc_create() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let owner = VpcOwner::Project(ProjectId("acme/backend".to_string()));
        let vpc = store
            .create_vpc("production", owner, Some("10.50.0.0/16"), false)
            .unwrap();

        assert_eq!(vpc.name, "production");
        assert_eq!(vpc.cidr, "10.50.0.0/16");
        assert!(vpc.vni >= 100);
    }

    #[test]
    fn duplicate_vpc_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let owner = VpcOwner::Project(ProjectId("acme/backend".to_string()));
        store
            .create_vpc("myvpc", owner.clone(), None, false)
            .unwrap();

        let err = store.create_vpc("myvpc", owner, None, false).unwrap_err();
        assert!(matches!(err, OrgError::VpcAlreadyExists(_)));
    }
}
