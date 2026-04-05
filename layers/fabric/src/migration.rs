//! Startup state migration — batch-migrates legacy name-keyed records to
//! deterministic ID-keyed records on daemon startup.
//!
//! When the cluster upgrades from name-keyed to ID-keyed schemas, existing
//! records in the local redb stores must be migrated. This module runs once
//! on each node at daemon start (before accepting requests), checking a
//! sentinel file to avoid redundant work.
//!
//! Migration is idempotent: safe to re-run if the daemon crashes mid-way.
//! IDs are deterministic (SHA-256 hash of prefix + name) so every Raft node
//! generates the same ID for the same resource name.

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use syfrah_org::OrgStore;

// ── Redb table names (mirrors constants in org/store.rs) ────────────────

const ORGS_TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";
const VPCS_TABLE: &str = "vpcs";
const SUBNETS_TABLE: &str = "subnets";
const SECURITY_GROUPS_TABLE: &str = "security_groups";

// Name-to-ID index tables
const ORG_NAME_IDX: &str = "org_name_idx";
const PROJECT_NAME_IDX: &str = "project_name_idx";
const ENV_NAME_IDX: &str = "env_name_idx";
const VPC_NAME_IDX: &str = "vpc_name_idx";
const SUBNET_NAME_IDX: &str = "subnet_name_idx";
const SG_NAME_IDX: &str = "sg_name_idx";

// Hypervisor tables
const HYPERVISORS_TABLE: &str = "hypervisors";
const HYPERVISORS_NAME_IDX: &str = "hypervisors_name_idx";

/// Generate a deterministic ID from a prefix and a name.
///
/// All Raft nodes produce the same ID for the same (prefix, name) pair,
/// which is essential for consistency during migration.
fn migrate_id(prefix: &str, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(b":");
    hasher.update(name.as_bytes());
    let hash = hasher.finalize();
    let hex: String = hash[..6].iter().map(|b| format!("{:02x}", b)).collect();
    format!("{prefix}-{hex}")
}

/// Returns true if a key looks like a generated ID (prefix-hexchars).
///
/// Generated IDs follow the pattern `{prefix}-{hex}` where hex is 12 chars
/// (random) or 12 chars (deterministic). Legacy keys are plain names or
/// composite keys like "org/project".
fn is_generated_id(key: &str, prefix: &str) -> bool {
    if let Some(rest) = key.strip_prefix(&format!("{prefix}-")) {
        // Accept both 12-char random and 12-char deterministic hex IDs
        rest.len() >= 12 && rest.chars().all(|c| c.is_ascii_hexdigit())
    } else {
        false
    }
}

/// Path to the migration sentinel file.
fn sentinel_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".syfrah")
        .join("id_migration_complete")
}

/// Run the full startup migration if the sentinel file is absent.
///
/// Call this after the org and hypervisor stores are opened but before
/// the daemon accepts requests.
pub fn run_startup_migration(org_store: &OrgStore, hv_store: Option<&syfrah_org::HypervisorStore>) {
    let sentinel = sentinel_path();

    if sentinel.exists() {
        debug!("migration: sentinel exists, skipping ID migration");
        return;
    }

    info!("migration: starting batch ID migration (first run after upgrade)");

    let mut total_migrated = 0u64;

    total_migrated += migrate_orgs(org_store);
    total_migrated += migrate_projects(org_store);
    total_migrated += migrate_environments(org_store);
    total_migrated += migrate_vpcs(org_store);
    total_migrated += migrate_subnets(org_store);
    total_migrated += migrate_security_groups(org_store);

    if let Some(store) = hv_store {
        total_migrated += migrate_hypervisors(store);
    }

    // Write sentinel — even if zero records were migrated (fresh install).
    if let Some(parent) = sentinel.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&sentinel, "1") {
        Ok(()) => info!(
            "migration: complete — {} records migrated, sentinel written",
            total_migrated
        ),
        Err(e) => warn!(
            "migration: {} records migrated but failed to write sentinel: {}",
            total_migrated, e
        ),
    }
}

// ── Per-table migration functions ───────────────────────────────────────

/// Migrate orgs from name-keyed to ID-keyed records.
fn migrate_orgs(store: &OrgStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::Org)> = match db.list(ORGS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list orgs table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut org) in entries {
        // Skip already-migrated records (keyed by generated ID)
        if is_generated_id(&key, "org") {
            continue;
        }

        let new_id = migrate_id("org", &key);

        // Skip if already migrated under the deterministic ID
        if db.exists(ORGS_TABLE, &new_id).unwrap_or(false) {
            // Just clean up old key and ensure index
            let _ = db.delete(ORGS_TABLE, &key);
            let _ = db.set(ORG_NAME_IDX, &key, &new_id);
            continue;
        }

        org.id = syfrah_org::OrgId(new_id.clone());

        if let Err(e) = db.set(ORGS_TABLE, &new_id, &org) {
            warn!("migration: failed to write org '{}' under new ID: {e}", key);
            continue;
        }
        if let Err(e) = db.set(ORG_NAME_IDX, &key, &new_id) {
            warn!(
                "migration: failed to write org name index for '{}': {e}",
                key
            );
        }
        if let Err(e) = db.delete(ORGS_TABLE, &key) {
            warn!("migration: failed to delete legacy org key '{}': {e}", key);
        }
        count += 1;
        debug!("migration: org '{}' -> {}", key, new_id);
    }

    if count > 0 {
        info!("migration: migrated {count} orgs");
    }
    count
}

/// Migrate projects from name-keyed to ID-keyed records.
fn migrate_projects(store: &OrgStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::Project)> = match db.list(PROJECTS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list projects table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut project) in entries {
        if is_generated_id(&key, "proj") {
            continue;
        }

        // Legacy key format: "org_name/project_name"
        let new_id = migrate_id("proj", &key);

        if db.exists(PROJECTS_TABLE, &new_id).unwrap_or(false) {
            let _ = db.delete(PROJECTS_TABLE, &key);
            let _ = db.set(PROJECT_NAME_IDX, &key, &new_id);
            continue;
        }

        project.id = syfrah_org::ProjectId(new_id.clone());

        if let Err(e) = db.set(PROJECTS_TABLE, &new_id, &project) {
            warn!(
                "migration: failed to write project '{}' under new ID: {e}",
                key
            );
            continue;
        }
        if let Err(e) = db.set(PROJECT_NAME_IDX, &key, &new_id) {
            warn!(
                "migration: failed to write project name index for '{}': {e}",
                key
            );
        }
        if let Err(e) = db.delete(PROJECTS_TABLE, &key) {
            warn!(
                "migration: failed to delete legacy project key '{}': {e}",
                key
            );
        }
        count += 1;
        debug!("migration: project '{}' -> {}", key, new_id);
    }

    if count > 0 {
        info!("migration: migrated {count} projects");
    }
    count
}

/// Migrate environments from name-keyed to ID-keyed records.
fn migrate_environments(store: &OrgStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::Environment)> = match db.list(ENVIRONMENTS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list environments table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut env) in entries {
        if is_generated_id(&key, "env") {
            continue;
        }

        let new_id = migrate_id("env", &key);

        if db.exists(ENVIRONMENTS_TABLE, &new_id).unwrap_or(false) {
            let _ = db.delete(ENVIRONMENTS_TABLE, &key);
            let _ = db.set(ENV_NAME_IDX, &key, &new_id);
            continue;
        }

        env.id = syfrah_org::EnvironmentId(new_id.clone());

        if let Err(e) = db.set(ENVIRONMENTS_TABLE, &new_id, &env) {
            warn!("migration: failed to write env '{}' under new ID: {e}", key);
            continue;
        }
        if let Err(e) = db.set(ENV_NAME_IDX, &key, &new_id) {
            warn!(
                "migration: failed to write env name index for '{}': {e}",
                key
            );
        }
        if let Err(e) = db.delete(ENVIRONMENTS_TABLE, &key) {
            warn!("migration: failed to delete legacy env key '{}': {e}", key);
        }
        count += 1;
        debug!("migration: env '{}' -> {}", key, new_id);
    }

    if count > 0 {
        info!("migration: migrated {count} environments");
    }
    count
}

/// Migrate VPCs from name-keyed to ID-keyed records.
///
/// Legacy VPCs are stored with key = name and id = "vpc-{name}".
/// New VPCs should be keyed by a deterministic ID.
fn migrate_vpcs(store: &OrgStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::Vpc)> = match db.list(VPCS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list vpcs table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut vpc) in entries {
        if is_generated_id(&key, "vpc") {
            continue;
        }

        let new_id = migrate_id("vpc", &key);

        if db.exists(VPCS_TABLE, &new_id).unwrap_or(false) {
            let _ = db.delete(VPCS_TABLE, &key);
            let _ = db.set(VPC_NAME_IDX, &key, &new_id);
            continue;
        }

        vpc.id = syfrah_org::VpcId(new_id.clone());

        if let Err(e) = db.set(VPCS_TABLE, &new_id, &vpc) {
            warn!("migration: failed to write vpc '{}' under new ID: {e}", key);
            continue;
        }
        if let Err(e) = db.set(VPC_NAME_IDX, &key, &new_id) {
            warn!(
                "migration: failed to write vpc name index for '{}': {e}",
                key
            );
        }
        if let Err(e) = db.delete(VPCS_TABLE, &key) {
            warn!("migration: failed to delete legacy vpc key '{}': {e}", key);
        }
        count += 1;
        debug!("migration: vpc '{}' -> {}", key, new_id);
    }

    if count > 0 {
        info!("migration: migrated {count} vpcs");
    }
    count
}

/// Migrate subnets from composite-keyed to ID-keyed records.
///
/// Legacy subnets are stored with key = "vpc_name/subnet_name" and
/// id = SubnetId(key). New subnets should be keyed by a deterministic ID.
fn migrate_subnets(store: &OrgStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::Subnet)> = match db.list(SUBNETS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list subnets table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut subnet) in entries {
        if is_generated_id(&key, "subnet") {
            continue;
        }

        let new_id = migrate_id("subnet", &key);

        if db.exists(SUBNETS_TABLE, &new_id).unwrap_or(false) {
            let _ = db.delete(SUBNETS_TABLE, &key);
            let _ = db.set(SUBNET_NAME_IDX, &key, &new_id);
            continue;
        }

        // Update the vpc_id reference if the VPC was also migrated.
        // The vpc_id in the subnet may be the old "vpc-{name}" format.
        let vpc_name = &subnet.vpc_id.0;
        if let Some(rest) = vpc_name.strip_prefix("vpc-") {
            // If this looks like a legacy "vpc-{name}" ID (not a hex ID),
            // update it to the deterministic migration ID.
            if !rest.chars().all(|c| c.is_ascii_hexdigit()) || rest.len() < 12 {
                subnet.vpc_id = syfrah_org::VpcId(migrate_id("vpc", rest));
            }
        }

        subnet.id = syfrah_org::SubnetId(new_id.clone());

        if let Err(e) = db.set(SUBNETS_TABLE, &new_id, &subnet) {
            warn!(
                "migration: failed to write subnet '{}' under new ID: {e}",
                key
            );
            continue;
        }
        if let Err(e) = db.set(SUBNET_NAME_IDX, &key, &new_id) {
            warn!(
                "migration: failed to write subnet name index for '{}': {e}",
                key
            );
        }
        if let Err(e) = db.delete(SUBNETS_TABLE, &key) {
            warn!(
                "migration: failed to delete legacy subnet key '{}': {e}",
                key
            );
        }
        count += 1;
        debug!("migration: subnet '{}' -> {}", key, new_id);
    }

    if count > 0 {
        info!("migration: migrated {count} subnets");
    }
    count
}

/// Migrate security groups from composite-keyed to ID-keyed records.
///
/// Legacy SGs are stored with key = "vpc_id/sg_name" and
/// id = SecurityGroupId("sg-{name}"). Re-key with deterministic IDs.
fn migrate_security_groups(store: &OrgStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::SecurityGroup)> = match db.list(SECURITY_GROUPS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list security_groups table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut sg) in entries {
        if is_generated_id(&key, "sg") {
            continue;
        }

        let new_id = migrate_id("sg", &key);

        if db.exists(SECURITY_GROUPS_TABLE, &new_id).unwrap_or(false) {
            let _ = db.delete(SECURITY_GROUPS_TABLE, &key);
            let _ = db.set(SG_NAME_IDX, &key, &new_id);
            continue;
        }

        // Update the vpc_id reference if the VPC was also migrated.
        let vpc_name = &sg.vpc_id.0;
        if let Some(rest) = vpc_name.strip_prefix("vpc-") {
            if !rest.chars().all(|c| c.is_ascii_hexdigit()) || rest.len() < 12 {
                sg.vpc_id = syfrah_org::VpcId(migrate_id("vpc", rest));
            }
        }

        sg.id = syfrah_org::SecurityGroupId(new_id.clone());

        if let Err(e) = db.set(SECURITY_GROUPS_TABLE, &new_id, &sg) {
            warn!("migration: failed to write sg '{}' under new ID: {e}", key);
            continue;
        }
        if let Err(e) = db.set(SG_NAME_IDX, &key, &new_id) {
            warn!(
                "migration: failed to write sg name index for '{}': {e}",
                key
            );
        }
        if let Err(e) = db.delete(SECURITY_GROUPS_TABLE, &key) {
            warn!("migration: failed to delete legacy sg key '{}': {e}", key);
        }
        count += 1;
        debug!("migration: sg '{}' -> {}", key, new_id);
    }

    if count > 0 {
        info!("migration: migrated {count} security groups");
    }
    count
}

/// Migrate hypervisors from name-keyed to ID-keyed records.
///
/// The HypervisorStore already has `migrate_legacy_record()` for individual
/// records, but this function batch-migrates all legacy records at once.
fn migrate_hypervisors(store: &syfrah_org::HypervisorStore) -> u64 {
    let db = store.db();
    let entries: Vec<(String, syfrah_org::Hypervisor)> = match db.list(HYPERVISORS_TABLE) {
        Ok(e) => e,
        Err(e) => {
            warn!("migration: failed to list hypervisors table: {e}");
            return 0;
        }
    };

    let mut count = 0u64;
    for (key, mut hv) in entries {
        // Skip records already keyed by a generated/valid ID.
        // The hypervisor ID pattern is "hv-{hex}" for generated IDs.
        // Legacy records are keyed by name.
        if is_generated_id(&key, "hv") {
            continue;
        }

        // Use the existing migrate_legacy_record method which handles the
        // name → ID re-keying and index creation.
        match store.migrate_legacy_record(&key) {
            Ok(()) => {
                count += 1;
                debug!("migration: hypervisor '{}' migrated via store method", key);
            }
            Err(e) => {
                // Fall back to manual deterministic migration
                let new_id = migrate_id("hv", &key);

                if db.exists(HYPERVISORS_TABLE, &new_id).unwrap_or(false) {
                    let _ = db.delete(HYPERVISORS_TABLE, &key);
                    let _ = db.set(HYPERVISORS_NAME_IDX, &key, &new_id);
                    continue;
                }

                hv.id = syfrah_org::HypervisorId(new_id.clone());

                if let Err(e2) = db.set(HYPERVISORS_TABLE, &new_id, &hv) {
                    warn!(
                        "migration: failed to write hv '{}' under new ID: {e2} (original: {e})",
                        key
                    );
                    continue;
                }
                let _ = db.set(HYPERVISORS_NAME_IDX, &key, &new_id);
                let _ = db.delete(HYPERVISORS_TABLE, &key);
                count += 1;
                debug!(
                    "migration: hypervisor '{}' -> {} (manual fallback)",
                    key, new_id
                );
            }
        }
    }

    if count > 0 {
        info!("migration: migrated {count} hypervisors");
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_id_generation() {
        // Same inputs always produce the same ID.
        let id1 = migrate_id("org", "acme");
        let id2 = migrate_id("org", "acme");
        assert_eq!(id1, id2);

        // Different names produce different IDs.
        let id3 = migrate_id("org", "globex");
        assert_ne!(id1, id3);

        // Different prefixes produce different IDs for the same name.
        let id4 = migrate_id("proj", "acme");
        assert_ne!(id1, id4);

        // Format: "{prefix}-{12hex}"
        assert!(id1.starts_with("org-"));
        assert_eq!(id1.len(), 4 + 12); // "org-" + 12 hex chars
    }

    #[test]
    fn is_generated_id_detection() {
        // Generated IDs (12+ hex chars after prefix)
        assert!(is_generated_id("org-a1b2c3d4e5f6", "org"));
        assert!(is_generated_id("vpc-aabbccddeeff", "vpc"));
        assert!(is_generated_id("org-abcdef012345", "org"));

        // Legacy name-keyed records
        assert!(!is_generated_id("acme", "org"));
        assert!(!is_generated_id("my-vpc", "vpc"));
        assert!(!is_generated_id("org/project", "proj"));
        assert!(!is_generated_id("vpc-my-vpc", "vpc")); // "my-vpc" isn't all hex

        // Edge: short hex suffix (not long enough)
        assert!(!is_generated_id("org-abc", "org"));
    }

    #[test]
    fn sentinel_path_is_valid() {
        let path = sentinel_path();
        assert!(path.to_str().unwrap().contains(".syfrah"));
        assert!(path.to_str().unwrap().ends_with("id_migration_complete"));
    }

    #[test]
    fn migrate_id_format() {
        let id = migrate_id("vpc", "production");
        assert!(id.starts_with("vpc-"));
        // 12 hex chars after "vpc-"
        let hex_part = &id[4..];
        assert_eq!(hex_part.len(), 12);
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn idempotent_migration_with_store() {
        // Create a temp store and verify that migration logic handles
        // the "already migrated" case gracefully.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();
        let store = syfrah_org::OrgStore::new(db);

        // Create an org via the store (this creates an ID-keyed record)
        let org = store.create("test-org").unwrap();
        assert!(org.id.0.starts_with("org-"));

        // Running migration should not touch it (already ID-keyed)
        let migrated = migrate_orgs(&store);
        assert_eq!(migrated, 0);
    }

    #[test]
    fn legacy_org_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();

        // Manually insert a legacy name-keyed org record
        let legacy_org = syfrah_org::Org {
            id: syfrah_org::OrgId("acme".to_string()),
            name: "acme".to_string(),
            created_at: 1000,
        };
        db.set(ORGS_TABLE, "acme", &legacy_org).unwrap();

        let store = syfrah_org::OrgStore::new(db);

        // Migrate
        let migrated = migrate_orgs(&store);
        assert_eq!(migrated, 1);

        // Verify old key is gone
        let old: Option<syfrah_org::Org> = store.db().get(ORGS_TABLE, "acme").unwrap();
        assert!(old.is_none());

        // Verify new ID-keyed record exists
        let expected_id = migrate_id("org", "acme");
        let new: Option<syfrah_org::Org> = store.db().get(ORGS_TABLE, &expected_id).unwrap();
        assert!(new.is_some());
        let new_org = new.unwrap();
        assert_eq!(new_org.name, "acme");
        assert_eq!(new_org.id.0, expected_id);

        // Verify name index
        let idx: Option<String> = store.db().get(ORG_NAME_IDX, "acme").unwrap();
        assert_eq!(idx.unwrap(), expected_id);

        // Running again should be a no-op
        let migrated2 = migrate_orgs(&store);
        assert_eq!(migrated2, 0);
    }

    #[test]
    fn legacy_vpc_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();

        let legacy_vpc = syfrah_org::Vpc {
            id: syfrah_org::VpcId("vpc-production".to_string()),
            name: "production".to_string(),
            cidr: "10.0.0.0/16".to_string(),
            vni: 100,
            owner: syfrah_org::VpcOwner::Org(syfrah_org::OrgId("org-acme".to_string())),
            shared: false,
            created_at: 1000,
        };
        db.set(VPCS_TABLE, "production", &legacy_vpc).unwrap();

        let store = syfrah_org::OrgStore::new(db);
        let migrated = migrate_vpcs(&store);
        assert_eq!(migrated, 1);

        let expected_id = migrate_id("vpc", "production");
        let new: Option<syfrah_org::Vpc> = store.db().get(VPCS_TABLE, &expected_id).unwrap();
        assert!(new.is_some());
        assert_eq!(new.unwrap().name, "production");

        // Index lookup
        let idx: Option<String> = store.db().get(VPC_NAME_IDX, "production").unwrap();
        assert_eq!(idx.unwrap(), expected_id);
    }

    #[test]
    fn legacy_subnet_migration_updates_vpc_ref() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();

        let legacy_subnet = syfrah_org::Subnet {
            id: syfrah_org::SubnetId("myvpc/web".to_string()),
            name: "web".to_string(),
            vpc_id: syfrah_org::VpcId("vpc-myvpc".to_string()),
            env_id: syfrah_org::EnvironmentId("env-staging".to_string()),
            cidr: "10.0.1.0/24".to_string(),
            gateway: "10.0.1.1".to_string(),
            created_at: 1000,
        };
        db.set(SUBNETS_TABLE, "myvpc/web", &legacy_subnet).unwrap();

        let store = syfrah_org::OrgStore::new(db);
        let migrated = migrate_subnets(&store);
        assert_eq!(migrated, 1);

        let expected_id = migrate_id("subnet", "myvpc/web");
        let new: Option<syfrah_org::Subnet> = store.db().get(SUBNETS_TABLE, &expected_id).unwrap();
        let subnet = new.unwrap();

        // VPC ID should have been updated to deterministic form
        let expected_vpc_id = migrate_id("vpc", "myvpc");
        assert_eq!(subnet.vpc_id.0, expected_vpc_id);
    }
}
