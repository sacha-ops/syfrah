//! VPC CIDR validation, overlap detection, and auto-allocation.

use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use ipnet::Ipv4Net;
use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{Vpc, VpcId, VpcOwner};
use crate::validation::validate_name;

const VPCS_TABLE: &str = "vpcs";
const VNI_COUNTER_KEY: &str = "vni_counter";

/// The RFC 1918 private address ranges that are allowed for VPC CIDRs.
const PRIVATE_RANGES: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::new(10, 0, 0, 0), 8),     // 10.0.0.0/8
    (Ipv4Addr::new(172, 16, 0, 0), 12),  // 172.16.0.0/12
    (Ipv4Addr::new(192, 168, 0, 0), 16), // 192.168.0.0/16
];

/// Minimum allowed prefix length for a VPC CIDR.
const MIN_PREFIX_LEN: u8 = 8;

/// Maximum allowed prefix length for a VPC CIDR.
const MAX_PREFIX_LEN: u8 = 28;

/// Starting VNI for auto-allocation.
const VNI_START: u32 = 100;

/// Auto-allocation range: 10.0.0.0/8.
const AUTO_ALLOC_BASE: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 0);

/// Auto-allocation prefix length: /16.
const AUTO_ALLOC_PREFIX: u8 = 16;

/// Parse and validate a CIDR string, returning an `Ipv4Net`.
///
/// Checks:
/// - Valid CIDR format (e.g. "10.1.0.0/16")
/// - Falls within a private address range (RFC 1918)
/// - Prefix length is between 8 and 28
/// - The address is the network address (no host bits set)
pub fn parse_and_validate_cidr(cidr_str: &str) -> Result<Ipv4Net> {
    let net: Ipv4Net = cidr_str
        .parse()
        .map_err(|_| OrgError::InvalidCidr(format!("'{cidr_str}': invalid CIDR format")))?;

    let prefix_len = net.prefix_len();

    // Check prefix length bounds
    if !(MIN_PREFIX_LEN..=MAX_PREFIX_LEN).contains(&prefix_len) {
        return Err(OrgError::InvalidCidr(format!(
            "'{cidr_str}': prefix length must be between {MIN_PREFIX_LEN} and {MAX_PREFIX_LEN}, got {prefix_len}"
        )));
    }

    // Ensure the address is the network address (no host bits set)
    if net.addr() != net.network() {
        return Err(OrgError::InvalidCidr(format!(
            "'{cidr_str}': address has host bits set; did you mean {}?",
            Ipv4Net::new(net.network(), prefix_len).unwrap()
        )));
    }

    // Check that the CIDR falls within a private range
    if !is_private_range(&net) {
        return Err(OrgError::InvalidCidr(format!(
            "'{cidr_str}': CIDR must be within a private range (10.0.0.0/8, 172.16.0.0/12, or 192.168.0.0/16)"
        )));
    }

    Ok(net)
}

/// Check whether a CIDR falls entirely within one of the RFC 1918 private ranges.
fn is_private_range(net: &Ipv4Net) -> bool {
    for &(base, prefix) in PRIVATE_RANGES {
        let private = Ipv4Net::new(base, prefix).unwrap();
        if private.contains(&net.network()) && private.contains(&net.broadcast()) {
            return true;
        }
    }
    false
}

/// Check if two CIDRs overlap: either one contains the other's network address.
pub fn cidrs_overlap(a: &Ipv4Net, b: &Ipv4Net) -> bool {
    a.contains(&b.network()) || b.contains(&a.network())
}

/// Find an available /16 in 10.0.0.0/8 that does not overlap with any existing CIDRs.
fn auto_allocate_cidr(existing: &[Ipv4Net]) -> Result<Ipv4Net> {
    // Try 10.0.0.0/16, 10.1.0.0/16, ..., 10.255.0.0/16
    for second_octet in 0..=255u8 {
        let candidate = Ipv4Net::new(
            Ipv4Addr::new(AUTO_ALLOC_BASE.octets()[0], second_octet, 0, 0),
            AUTO_ALLOC_PREFIX,
        )
        .unwrap();

        let overlaps = existing.iter().any(|e| cidrs_overlap(&candidate, e));
        if !overlaps {
            return Ok(candidate);
        }
    }

    Err(OrgError::CidrExhausted)
}

/// Persistent store for VPCs.
pub struct VpcStore {
    db: LayerDb,
}

impl VpcStore {
    /// Create a new `VpcStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Allocate the next VNI (monotonically increasing from 100).
    fn next_vni(&self) -> Result<u32> {
        let current = self
            .db
            .get_metric(VNI_COUNTER_KEY)
            .map_err(|e| OrgError::StoreError(e.to_string()))?;
        let vni = if current == 0 {
            VNI_START
        } else {
            current as u32 + 1
        };
        self.db
            .set_metric(VNI_COUNTER_KEY, vni as u64)
            .map_err(|e| OrgError::StoreError(e.to_string()))?;
        Ok(vni)
    }

    /// List all VPCs.
    pub fn list(&self) -> Result<Vec<Vpc>> {
        let entries: Vec<(String, Vpc)> = self.db.list(VPCS_TABLE)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    /// List VPCs belonging to a specific org (by org name).
    pub fn list_by_org(&self, org: &str) -> Result<Vec<Vpc>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|v| match &v.owner {
                VpcOwner::Org(org_id) => org_id.0 == org,
                VpcOwner::Project(proj_id) => proj_id.0.starts_with(&format!("{org}/")),
            })
            .collect())
    }

    /// List VPCs owned by a specific project.
    pub fn list_for_project(&self, org: &str, project: &str) -> Result<Vec<Vpc>> {
        let project_id = format!("{org}/{project}");
        Ok(self
            .list()?
            .into_iter()
            .filter(|v| matches!(&v.owner, VpcOwner::Project(pid) if pid.0 == project_id))
            .collect())
    }

    /// Get a VPC by name.
    pub fn get(&self, name: &str) -> Result<Option<Vpc>> {
        Ok(self.db.get(VPCS_TABLE, name)?)
    }

    /// Create a new VPC.
    ///
    /// If `cidr_str` is `None`, auto-allocates a /16 from 10.0.0.0/8.
    /// Validates the CIDR and checks for overlap with existing VPCs in the same org.
    pub fn create(
        &self,
        name: &str,
        owner: VpcOwner,
        cidr_str: Option<&str>,
        shared: bool,
    ) -> Result<Vpc> {
        validate_name(name, "vpc")?;

        if self.db.exists(VPCS_TABLE, name)? {
            return Err(OrgError::VpcAlreadyExists(name.to_string()));
        }

        // Determine the org for overlap checking
        let org_name = match &owner {
            VpcOwner::Org(org_id) => org_id.0.clone(),
            VpcOwner::Project(proj_id) => {
                // Project IDs are "org/project"
                proj_id
                    .0
                    .split('/')
                    .next()
                    .unwrap_or(&proj_id.0)
                    .to_string()
            }
        };

        // Get existing VPCs in the same org for overlap checking
        let org_vpcs = self.list_by_org(&org_name)?;
        let existing_cidrs: Vec<Ipv4Net> = org_vpcs
            .iter()
            .filter_map(|v| v.cidr.parse::<Ipv4Net>().ok())
            .collect();

        // Parse/validate or auto-allocate CIDR
        let cidr = match cidr_str {
            Some(s) => {
                let net = parse_and_validate_cidr(s)?;
                // Check overlap with existing VPCs in the same org
                for existing in &existing_cidrs {
                    if cidrs_overlap(&net, existing) {
                        return Err(OrgError::CidrOverlap {
                            new_cidr: net.to_string(),
                            existing_cidr: existing.to_string(),
                        });
                    }
                }
                net
            }
            None => auto_allocate_cidr(&existing_cidrs)?,
        };

        let vni = self.next_vni()?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let vpc = Vpc {
            id: VpcId(format!("vpc-{name}")),
            name: name.to_string(),
            cidr: cidr.to_string(),
            vni,
            owner,
            shared,
            created_at: now,
        };

        self.db.set(VPCS_TABLE, name, &vpc)?;
        Ok(vpc)
    }

    /// Ensure a default VPC exists for the given org/project.
    ///
    /// If a VPC named "{org}-{project}-default" already exists, returns it.
    /// Otherwise, creates one with an auto-allocated /16 CIDR and a new VNI.
    ///
    /// This is the entry point for auto-creation when the first subnet is
    /// created in a project that has no VPC.
    pub fn ensure_default_vpc(&self, org: &str, project: &str) -> Result<Vpc> {
        let default_name = format!("{org}-{project}-default");

        // Idempotency: return existing default VPC if present.
        if let Some(existing) = self.get(&default_name)? {
            return Ok(existing);
        }

        let owner = VpcOwner::Project(crate::types::ProjectId(format!("{org}/{project}")));
        self.create(&default_name, owner, None, false)
    }

    /// Delete a VPC by name.
    pub fn delete(&self, name: &str) -> Result<()> {
        if !self.db.exists(VPCS_TABLE, name)? {
            return Err(OrgError::VpcNotFound(name.to_string()));
        }
        self.db.delete(VPCS_TABLE, name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrgId, ProjectId};

    fn temp_store() -> (tempfile::TempDir, VpcStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpc-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, VpcStore::new(db))
    }

    // ── CIDR parsing tests ──────────────────────────────────────────

    #[test]
    fn valid_cidr() {
        let net = parse_and_validate_cidr("10.1.0.0/16").unwrap();
        assert_eq!(net, "10.1.0.0/16".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn valid_cidr_various() {
        assert!(parse_and_validate_cidr("10.0.0.0/8").is_ok());
        assert!(parse_and_validate_cidr("172.16.0.0/12").is_ok());
        assert!(parse_and_validate_cidr("192.168.0.0/16").is_ok());
        assert!(parse_and_validate_cidr("192.168.1.0/24").is_ok());
        assert!(parse_and_validate_cidr("10.100.0.0/28").is_ok());
    }

    #[test]
    fn invalid_cidr_rejected() {
        assert!(parse_and_validate_cidr("not-a-cidr").is_err());
        assert!(parse_and_validate_cidr("256.0.0.0/16").is_err());
        assert!(parse_and_validate_cidr("10.0.0.0").is_err());
    }

    #[test]
    fn non_private_range_rejected() {
        assert!(parse_and_validate_cidr("8.8.8.0/24").is_err());
        assert!(parse_and_validate_cidr("1.0.0.0/8").is_err());
    }

    #[test]
    fn prefix_too_small_rejected() {
        assert!(parse_and_validate_cidr("10.0.0.0/7").is_err());
    }

    #[test]
    fn prefix_too_large_rejected() {
        assert!(parse_and_validate_cidr("10.0.0.0/29").is_err());
    }

    #[test]
    fn host_bits_set_rejected() {
        assert!(parse_and_validate_cidr("10.1.0.1/16").is_err());
    }

    // ── Overlap detection tests ─────────────────────────────────────

    #[test]
    fn overlapping_cidr_rejected() {
        let a: Ipv4Net = "10.1.0.0/16".parse().unwrap();
        let b: Ipv4Net = "10.1.0.0/24".parse().unwrap();
        assert!(cidrs_overlap(&a, &b));
        assert!(cidrs_overlap(&b, &a));
    }

    #[test]
    fn identical_cidrs_overlap() {
        let a: Ipv4Net = "10.1.0.0/16".parse().unwrap();
        let b: Ipv4Net = "10.1.0.0/16".parse().unwrap();
        assert!(cidrs_overlap(&a, &b));
    }

    #[test]
    fn non_overlapping_cidrs_ok() {
        let a: Ipv4Net = "10.1.0.0/16".parse().unwrap();
        let b: Ipv4Net = "10.2.0.0/16".parse().unwrap();
        assert!(!cidrs_overlap(&a, &b));
    }

    #[test]
    fn supernet_overlap() {
        let a: Ipv4Net = "10.0.0.0/8".parse().unwrap();
        let b: Ipv4Net = "10.1.0.0/16".parse().unwrap();
        assert!(cidrs_overlap(&a, &b));
    }

    // ── VPC store tests ─────────────────────────────────────────────

    #[test]
    fn create_vpc_with_cidr() {
        let (_dir, store) = temp_store();
        let vpc = store
            .create(
                "test-vpc",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();
        assert_eq!(vpc.name, "test-vpc");
        assert_eq!(vpc.cidr, "10.1.0.0/16");
        assert_eq!(vpc.vni, VNI_START);
    }

    #[test]
    fn create_vpc_auto_allocate() {
        let (_dir, store) = temp_store();
        let vpc = store
            .create(
                "auto-vpc",
                VpcOwner::Org(OrgId("acme".to_string())),
                None,
                false,
            )
            .unwrap();
        assert_eq!(vpc.cidr, "10.0.0.0/16");
    }

    #[test]
    fn auto_allocate_skips_used_cidrs() {
        let (_dir, store) = temp_store();
        store
            .create(
                "vpc-one",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.0.0.0/16"),
                false,
            )
            .unwrap();

        let vpc2 = store
            .create(
                "vpc-two",
                VpcOwner::Org(OrgId("acme".to_string())),
                None,
                false,
            )
            .unwrap();
        assert_eq!(vpc2.cidr, "10.1.0.0/16");
    }

    #[test]
    fn vni_increments() {
        let (_dir, store) = temp_store();
        let v1 = store
            .create(
                "vpc-aaa",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();
        let v2 = store
            .create(
                "vpc-bbb",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.2.0.0/16"),
                false,
            )
            .unwrap();
        assert_eq!(v1.vni, 100);
        assert_eq!(v2.vni, 101);
    }

    #[test]
    fn overlapping_cidr_in_same_org_rejected() {
        let (_dir, store) = temp_store();
        store
            .create(
                "vpc-one",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();

        let err = store
            .create(
                "vpc-two",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/24"),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::CidrOverlap { .. }));
    }

    #[test]
    fn same_cidr_different_orgs_ok() {
        let (_dir, store) = temp_store();
        store
            .create(
                "vpc-org-a",
                VpcOwner::Org(OrgId("alpha".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();

        let vpc2 = store
            .create(
                "vpc-org-b",
                VpcOwner::Org(OrgId("beta".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();
        assert_eq!(vpc2.cidr, "10.1.0.0/16");
    }

    #[test]
    fn duplicate_vpc_name_rejected() {
        let (_dir, store) = temp_store();
        store
            .create(
                "my-vpc",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();

        let err = store
            .create(
                "my-vpc",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.2.0.0/16"),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::VpcAlreadyExists(_)));
    }

    #[test]
    fn delete_vpc() {
        let (_dir, store) = temp_store();
        store
            .create(
                "del-vpc",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();

        store.delete("del-vpc").unwrap();
        assert!(store.get("del-vpc").unwrap().is_none());
    }

    #[test]
    fn list_vpcs() {
        let (_dir, store) = temp_store();
        store
            .create(
                "vpc-aaa",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();
        store
            .create(
                "vpc-bbb",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.2.0.0/16"),
                false,
            )
            .unwrap();

        let vpcs = store.list().unwrap();
        assert_eq!(vpcs.len(), 2);
    }

    #[test]
    fn project_vpc_overlap_detection() {
        let (_dir, store) = temp_store();
        store
            .create(
                "proj-vpc",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                Some("10.1.0.0/16"),
                false,
            )
            .unwrap();

        let err = store
            .create(
                "org-vpc",
                VpcOwner::Org(OrgId("acme".to_string())),
                Some("10.1.5.0/24"),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::CidrOverlap { .. }));
    }

    // ── Auto-creation tests ─────────────────────────────────────────

    #[test]
    fn ensure_default_vpc_creates_new() {
        let (_dir, store) = temp_store();
        let vpc = store.ensure_default_vpc("acme", "backend").unwrap();
        assert_eq!(vpc.name, "acme-backend-default");
        assert!(!vpc.shared);
        assert!(matches!(&vpc.owner, VpcOwner::Project(pid) if pid.0 == "acme/backend"));
        assert!(vpc.vni >= 100);
        assert!(vpc.cidr.ends_with("/16"));
    }

    #[test]
    fn ensure_default_vpc_idempotent() {
        let (_dir, store) = temp_store();
        let vpc1 = store.ensure_default_vpc("acme", "backend").unwrap();
        let vpc2 = store.ensure_default_vpc("acme", "backend").unwrap();
        assert_eq!(vpc1.id, vpc2.id);
        assert_eq!(vpc1.vni, vpc2.vni);
        assert_eq!(vpc1.cidr, vpc2.cidr);
    }

    #[test]
    fn ensure_default_vpc_unique_vnis() {
        let (_dir, store) = temp_store();
        let vpc1 = store.ensure_default_vpc("acme", "alpha").unwrap();
        let vpc2 = store.ensure_default_vpc("acme", "bravo").unwrap();
        assert_ne!(vpc1.vni, vpc2.vni);
        assert!(vpc2.vni > vpc1.vni);
    }
}
