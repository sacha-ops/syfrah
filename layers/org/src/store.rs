use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{
    Environment, EnvironmentId, Org, OrgId, Project, ProjectId, Vpc, VpcId, VpcOwner,
};
use crate::validation::validate_name;

const TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";
const VPCS_TABLE: &str = "vpcs";
const VNI_COUNTER_TABLE: &str = "vni_counter";
const VNI_COUNTER_KEY: &str = "counter";
const VNI_START: u32 = 100;

/// Persistent store for organizations backed by redb.
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Create a new `OrgStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    // ── Org operations ───────────────────────────────────────────────

    /// Create a new organization. Validates the name, checks for duplicates.
    pub fn create(&self, name: &str) -> Result<Org> {
        validate_name(name, "org")?;

        if self.db.exists(TABLE, name)? {
            return Err(OrgError::AlreadyExists(name.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let org = Org {
            id: OrgId(format!("org-{name}")),
            name: name.to_string(),
            created_at: now,
        };

        self.db.set(TABLE, name, &org)?;
        Ok(org)
    }

    /// Get an organization by name. Returns `None` if it doesn't exist.
    pub fn get(&self, name: &str) -> Result<Option<Org>> {
        Ok(self.db.get(TABLE, name)?)
    }

    /// List all organizations.
    pub fn list(&self) -> Result<Vec<Org>> {
        let entries: Vec<(String, Org)> = self.db.list(TABLE)?;
        Ok(entries.into_iter().map(|(_, org)| org).collect())
    }

    /// Delete an organization by name. Fails if it has projects.
    pub fn delete(&self, name: &str) -> Result<()> {
        if !self.db.exists(TABLE, name)? {
            return Err(OrgError::NotFound(name.to_string()));
        }

        // Check for child projects
        let projects = self.list_projects(name)?;
        if !projects.is_empty() {
            return Err(OrgError::OrgHasProjects(name.to_string()));
        }

        self.db.delete(TABLE, name)?;
        Ok(())
    }

    // ── Project operations ───────────────────────────────────────────

    /// Build the redb key for a project: "org_name/project_name".
    fn project_key(org: &str, project: &str) -> String {
        format!("{}/{}", org, project)
    }

    /// Create a new project within an organization.
    pub fn create_project(&self, org: &str, name: &str) -> Result<Project> {
        validate_name(name, "project")?;

        // Verify org exists
        if !self.db.exists(TABLE, org)? {
            return Err(OrgError::NotFound(org.to_string()));
        }

        let key = Self::project_key(org, name);
        if self.db.exists(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectAlreadyExists {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let project = Project {
            id: ProjectId(key.clone()),
            name: name.to_string(),
            org_id: OrgId(org.to_string()),
            created_at: now,
        };

        self.db.set(PROJECTS_TABLE, &key, &project)?;
        Ok(project)
    }

    /// Get a project by org and project name.
    pub fn get_project(&self, org: &str, name: &str) -> Result<Option<Project>> {
        let key = Self::project_key(org, name);
        Ok(self.db.get(PROJECTS_TABLE, &key)?)
    }

    /// List all projects in an organization.
    pub fn list_projects(&self, org: &str) -> Result<Vec<Project>> {
        let all: Vec<(String, Project)> = self.db.list(PROJECTS_TABLE)?;
        let prefix = format!("{}/", org);
        Ok(all
            .into_iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, project)| project)
            .collect())
    }

    /// Delete a project. Fails if it has any environments.
    pub fn delete_project(&self, org: &str, name: &str) -> Result<()> {
        let key = Self::project_key(org, name);

        if !self.db.exists(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectNotFound {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        // Check for child environments
        let envs = self.list_envs(org, name)?;
        if !envs.is_empty() {
            return Err(OrgError::ProjectHasEnvironments {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        self.db.delete(PROJECTS_TABLE, &key)?;
        Ok(())
    }

    // ── Environment operations ──────────────────────────────────────

    fn env_key(org: &str, project: &str, env: &str) -> String {
        format!("{org}/{project}/{env}")
    }

    /// Create an environment within a project.
    pub fn create_env(
        &self,
        org: &str,
        project: &str,
        name: &str,
        ttl: Option<u64>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    ) -> Result<Environment> {
        validate_name(name, "environment")?;

        // Verify project exists
        let project_key = Self::project_key(org, project);
        if !self.db.exists(PROJECTS_TABLE, &project_key)? {
            return Err(OrgError::ProjectNotFound {
                org: org.to_string(),
                project: project.to_string(),
            });
        }

        let env_key = Self::env_key(org, project, name);
        if self.db.exists(ENVIRONMENTS_TABLE, &env_key)? {
            return Err(OrgError::EnvAlreadyExists(name.to_string()));
        }

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expires_at = ttl.map(|t| created_at + t);

        let env = Environment {
            id: EnvironmentId(env_key.clone()),
            name: name.to_string(),
            project_id: ProjectId(project_key),
            ttl,
            deletion_protection,
            labels,
            created_at,
            expires_at,
        };

        self.db.set(ENVIRONMENTS_TABLE, &env_key, &env)?;
        Ok(env)
    }

    /// Get an environment by org, project, and name.
    pub fn get_env(&self, org: &str, project: &str, name: &str) -> Result<Environment> {
        let key = Self::env_key(org, project, name);
        self.db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))
    }

    /// List environments for a given org and project.
    pub fn list_envs(&self, org: &str, project: &str) -> Result<Vec<Environment>> {
        let prefix = format!("{org}/{project}/");
        Ok(self
            .db
            .list::<Environment>(ENVIRONMENTS_TABLE)?
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect())
    }

    /// Extend (or set) the TTL of an environment.
    ///
    /// The new TTL is measured from **now**, not from the original creation
    /// time, so `extend_env("acme", "backend", "ci", 7200)` always gives
    /// 2 hours from the current moment.
    pub fn extend_env(
        &self,
        org: &str,
        project: &str,
        name: &str,
        ttl: u64,
    ) -> Result<Environment> {
        let key = Self::env_key(org, project, name);

        let mut env = self
            .db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        env.ttl = Some(ttl);
        env.expires_at = Some(now + ttl);

        self.db.set(ENVIRONMENTS_TABLE, &key, &env)?;
        Ok(env)
    }

    /// Toggle deletion protection on an environment.
    pub fn update_env_protection(
        &self,
        org: &str,
        project: &str,
        name: &str,
        enabled: bool,
    ) -> Result<Environment> {
        let key = Self::env_key(org, project, name);

        let mut env = self
            .db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))?;

        env.deletion_protection = enabled;
        self.db.set(ENVIRONMENTS_TABLE, &key, &env)?;
        Ok(env)
    }

    /// Delete an environment. Fails if deletion protection is enabled.
    pub fn delete_env(&self, org: &str, project: &str, name: &str) -> Result<()> {
        let key = Self::env_key(org, project, name);

        let env = self
            .db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))?;

        if env.deletion_protection {
            return Err(OrgError::EnvProtected(name.to_string()));
        }

        self.db.delete(ENVIRONMENTS_TABLE, &key)?;
        Ok(())
    }

    // ── VPC operations ──────────────────────────────────────────────

    /// Validate a CIDR string. Accepts patterns like "10.0.0.0/16", "10.1.0.0/24".
    fn validate_cidr(cidr: &str) -> Result<()> {
        let parts: Vec<&str> = cidr.split('/').collect();
        if parts.len() != 2 {
            return Err(OrgError::InvalidCidr(format!(
                "expected format A.B.C.D/N, got: {cidr}"
            )));
        }

        let octets: Vec<&str> = parts[0].split('.').collect();
        if octets.len() != 4 {
            return Err(OrgError::InvalidCidr(format!(
                "expected 4 octets in network address, got: {}",
                parts[0]
            )));
        }

        for octet in &octets {
            let val: u8 = octet
                .parse()
                .map_err(|_| OrgError::InvalidCidr(format!("invalid octet: {octet}")))?;
            // Allow any valid octet (0-255), parse already checks this
            let _ = val;
        }

        let prefix_len: u8 = parts[1]
            .parse()
            .map_err(|_| OrgError::InvalidCidr(format!("invalid prefix length: {}", parts[1])))?;
        if prefix_len > 32 {
            return Err(OrgError::InvalidCidr(format!(
                "prefix length must be 0-32, got: {prefix_len}"
            )));
        }

        Ok(())
    }

    /// Allocate the next VNI. Starts at 100, monotonically increasing.
    fn next_vni(&self) -> Result<u32> {
        let current: Option<u32> = self.db.get(VNI_COUNTER_TABLE, VNI_COUNTER_KEY)?;
        let vni = current.unwrap_or(VNI_START);
        self.db
            .set(VNI_COUNTER_TABLE, VNI_COUNTER_KEY, &(vni + 1))?;
        Ok(vni)
    }

    /// Create a VPC. Validates the name and CIDR, allocates a VNI, persists.
    pub fn create_vpc(&self, name: &str, cidr: &str, owner: VpcOwner, shared: bool) -> Result<Vpc> {
        validate_name(name, "vpc")?;
        Self::validate_cidr(cidr)?;

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
            cidr: cidr.to_string(),
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

    /// List VPCs owned by a specific project.
    pub fn list_vpcs_by_project(&self, project_id: &ProjectId) -> Result<Vec<Vpc>> {
        let all = self.list_vpcs()?;
        Ok(all
            .into_iter()
            .filter(|vpc| matches!(&vpc.owner, VpcOwner::Project(pid) if pid == project_id))
            .collect())
    }

    /// List VPCs owned by a specific org.
    pub fn list_vpcs_by_org(&self, org_id: &OrgId) -> Result<Vec<Vpc>> {
        let all = self.list_vpcs()?;
        Ok(all
            .into_iter()
            .filter(|vpc| matches!(&vpc.owner, VpcOwner::Org(oid) if oid == org_id))
            .collect())
    }

    /// Delete a VPC by name.
    pub fn delete_vpc(&self, name: &str) -> Result<()> {
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

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, OrgStore::new(db))
    }

    /// Helper: create an org and project so env tests can focus on environments.
    fn setup_org_and_project(store: &OrgStore) {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
    }

    // ── Org tests ───────────────────────────────────────────────────

    #[test]
    fn create_org() {
        let (_dir, store) = temp_store();
        let org = store.create("acme").unwrap();
        assert_eq!(org.name, "acme");
        assert_eq!(org.id.0, "org-acme");
        assert!(org.created_at > 0);
    }

    #[test]
    fn duplicate_name_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        let err = store.create("acme").unwrap_err();
        assert!(matches!(err, OrgError::AlreadyExists(_)));
    }

    #[test]
    fn invalid_name_rejected() {
        let (_dir, store) = temp_store();

        assert!(matches!(
            store.create("my org").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create("Acme").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create("org@1").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create("ab").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create(&"x".repeat(64)).unwrap_err(),
            OrgError::InvalidName { .. }
        ));
    }

    #[test]
    fn delete_org() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.delete("acme").unwrap();
        assert!(store.get("acme").unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_fails() {
        let (_dir, store) = temp_store();
        let err = store.delete("ghost").unwrap_err();
        assert!(matches!(err, OrgError::NotFound(_)));
    }

    #[test]
    fn list_orgs() {
        let (_dir, store) = temp_store();
        store.create("alpha").unwrap();
        store.create("beta").unwrap();
        store.create("gamma").unwrap();

        let orgs = store.list().unwrap();
        assert_eq!(orgs.len(), 3);
    }

    #[test]
    fn get_nonexistent() {
        let (_dir, store) = temp_store();
        assert!(store.get("does-not-exist").unwrap().is_none());
    }

    // ── Project tests ───────────────────────────────────────────────

    #[test]
    fn create_project_succeeds_with_valid_org() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        let project = store.create_project("acme", "backend").unwrap();
        assert_eq!(project.name, "backend");
        assert_eq!(project.org_id, OrgId("acme".to_string()));

        let fetched = store.get_project("acme", "backend").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "backend");
    }

    #[test]
    fn duplicate_project_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.create_project("acme", "backend").unwrap_err();
        assert!(matches!(err, OrgError::ProjectAlreadyExists { .. }));
    }

    #[test]
    fn delete_project_succeeds_when_empty() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        store.delete_project("acme", "backend").unwrap();
        assert!(store.get_project("acme", "backend").unwrap().is_none());
    }

    #[test]
    fn org_with_projects_cannot_be_deleted() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.delete("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgHasProjects(_)));
    }

    #[test]
    fn create_project_fails_without_org() {
        let (_dir, store) = temp_store();
        let err = store.create_project("nonexistent", "backend").unwrap_err();
        assert!(matches!(err, OrgError::NotFound(_)));
    }

    // ── Environment tests ───────────────────────────────────────────

    #[test]
    fn create_env_basic() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let env = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        assert_eq!(env.name, "staging");
        assert_eq!(env.project_id.0, "acme/backend");
        assert!(env.created_at > 0);
        assert_eq!(env.ttl, None);
        assert!(!env.deletion_protection);
        assert!(env.labels.is_empty());
        assert_eq!(env.expires_at, None);
    }

    #[test]
    fn duplicate_env_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        let err = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap_err();

        assert!(matches!(err, OrgError::EnvAlreadyExists(ref n) if n == "staging"));
    }

    #[test]
    fn with_ttl() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let ttl_seconds = 3600;
        let env = store
            .create_env(
                "acme",
                "backend",
                "ephemeral",
                Some(ttl_seconds),
                false,
                HashMap::new(),
            )
            .unwrap();

        assert_eq!(env.ttl, Some(ttl_seconds));
        assert!(env.expires_at.is_some());
        assert_eq!(env.expires_at.unwrap(), env.created_at + ttl_seconds);
    }

    #[test]
    fn with_labels() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let mut labels = HashMap::new();
        labels.insert("region".to_string(), "eu-west".to_string());
        labels.insert("team".to_string(), "payments".to_string());

        let env = store
            .create_env("acme", "backend", "production", None, false, labels.clone())
            .unwrap();

        assert_eq!(env.labels, labels);

        let retrieved = store.get_env("acme", "backend", "production").unwrap();
        assert_eq!(retrieved.labels, labels);
    }

    #[test]
    fn with_deletion_protection() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let env = store
            .create_env("acme", "backend", "production", None, true, HashMap::new())
            .unwrap();

        assert!(env.deletion_protection);

        let retrieved = store.get_env("acme", "backend", "production").unwrap();
        assert!(retrieved.deletion_protection);
    }

    #[test]
    fn delete_env_succeeds_when_not_protected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        store.delete_env("acme", "backend", "staging").unwrap();

        let err = store.get_env("acme", "backend", "staging").unwrap_err();
        assert!(matches!(err, OrgError::EnvNotFound(_)));
    }

    #[test]
    fn delete_env_fails_when_protected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "production", None, true, HashMap::new())
            .unwrap();

        let err = store
            .delete_env("acme", "backend", "production")
            .unwrap_err();
        assert!(matches!(err, OrgError::EnvProtected(ref n) if n == "production"));
    }

    #[test]
    fn list_by_project() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store.create_project("acme", "frontend").unwrap();

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();
        store
            .create_env("acme", "backend", "production", None, true, HashMap::new())
            .unwrap();
        store
            .create_env("acme", "frontend", "staging", None, false, HashMap::new())
            .unwrap();

        let backend_envs = store.list_envs("acme", "backend").unwrap();
        assert_eq!(backend_envs.len(), 2);

        let frontend_envs = store.list_envs("acme", "frontend").unwrap();
        assert_eq!(frontend_envs.len(), 1);
        assert_eq!(frontend_envs[0].name, "staging");
    }

    #[test]
    fn create_env_requires_project() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        let err = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap_err();
        assert!(matches!(err, OrgError::ProjectNotFound { .. }));
    }

    #[test]
    fn delete_env_not_found() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let err = store
            .delete_env("acme", "backend", "nonexistent")
            .unwrap_err();
        assert!(matches!(err, OrgError::EnvNotFound(_)));
    }

    #[test]
    fn delete_project_with_envs_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        let err = store.delete_project("acme", "backend").unwrap_err();
        assert!(matches!(err, OrgError::ProjectHasEnvironments { .. }));
    }

    // ── VPC tests ───────────────────────────────────────────────────

    #[test]
    fn create_vpc_succeeds() {
        let (_dir, store) = temp_store();
        let vpc = store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        assert_eq!(vpc.name, "default");
        assert_eq!(vpc.id.0, "vpc-default");
        assert_eq!(vpc.cidr, "10.1.0.0/16");
        assert_eq!(vpc.vni, 100);
        assert!(!vpc.shared);
        assert!(vpc.created_at > 0);

        let fetched = store.get_vpc("default").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "default");
    }

    #[test]
    fn vni_increments() {
        let (_dir, store) = temp_store();
        let vpc1 = store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        let vpc2 = store
            .create_vpc(
                "vpc-two",
                "10.2.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        assert_eq!(vpc1.vni, 100);
        assert_eq!(vpc2.vni, 101);
    }

    #[test]
    fn duplicate_vpc_name_rejected() {
        let (_dir, store) = temp_store();
        store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let err = store
            .create_vpc(
                "default",
                "10.2.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::VpcAlreadyExists(_)));
    }

    #[test]
    fn delete_vpc_succeeds() {
        let (_dir, store) = temp_store();
        store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store.delete_vpc("default").unwrap();
        assert!(store.get_vpc("default").unwrap().is_none());
    }

    #[test]
    fn delete_vpc_not_found() {
        let (_dir, store) = temp_store();
        let err = store.delete_vpc("ghost").unwrap_err();
        assert!(matches!(err, OrgError::VpcNotFound(_)));
    }

    #[test]
    fn list_vpcs_returns_all() {
        let (_dir, store) = temp_store();
        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-two",
                "10.2.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-three",
                "10.3.0.0/16",
                VpcOwner::Project(ProjectId("acme/frontend".to_string())),
                false,
            )
            .unwrap();

        let all = store.list_vpcs().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn list_vpcs_by_project_filters() {
        let (_dir, store) = temp_store();
        let pid = ProjectId("acme/backend".to_string());

        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(pid.clone()),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-two",
                "10.2.0.0/16",
                VpcOwner::Project(ProjectId("acme/frontend".to_string())),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-shared",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        let by_project = store.list_vpcs_by_project(&pid).unwrap();
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].name, "vpc-one");
    }

    #[test]
    fn list_vpcs_by_org_filters() {
        let (_dir, store) = temp_store();
        let oid = OrgId("acme".to_string());

        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-shared",
                "10.100.0.0/16",
                VpcOwner::Org(oid.clone()),
                true,
            )
            .unwrap();

        let by_org = store.list_vpcs_by_org(&oid).unwrap();
        assert_eq!(by_org.len(), 1);
        assert_eq!(by_org[0].name, "vpc-shared");
    }

    #[test]
    fn invalid_cidr_rejected() {
        let (_dir, store) = temp_store();

        let err = store
            .create_vpc(
                "bad-cidr",
                "not-a-cidr",
                VpcOwner::Project(ProjectId("x".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));

        let err = store
            .create_vpc(
                "bad-cidr",
                "10.0.0/16",
                VpcOwner::Project(ProjectId("x".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));

        let err = store
            .create_vpc(
                "bad-cidr",
                "10.0.0.0/33",
                VpcOwner::Project(ProjectId("x".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));
    }
}
