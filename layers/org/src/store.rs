use std::collections::HashMap;

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{Environment, EnvironmentId, Org, OrgId, Project, ProjectId};

const ORGS_TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";

/// Persistence layer for the org hierarchy.
///
/// Keys follow the pattern:
/// - orgs: `{org_name}`
/// - projects: `{org_name}/{project_name}`
/// - environments: `{org_name}/{project_name}/{env_name}`
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Create a new store backed by the given database.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    // ── Org operations ──────────────────────────────────────────────

    /// Create an organization.
    pub fn create_org(&self, name: &str) -> Result<Org> {
        if self.db.exists(ORGS_TABLE, name)? {
            return Err(OrgError::OrgAlreadyExists(name.to_string()));
        }

        let org = Org {
            id: OrgId(name.to_string()),
            name: name.to_string(),
            created_at: now(),
        };

        self.db.set(ORGS_TABLE, name, &org)?;
        Ok(org)
    }

    /// Get an organization by name.
    pub fn get_org(&self, name: &str) -> Result<Org> {
        self.db
            .get::<Org>(ORGS_TABLE, name)?
            .ok_or_else(|| OrgError::OrgNotFound(name.to_string()))
    }

    /// List all organizations.
    pub fn list_orgs(&self) -> Result<Vec<Org>> {
        Ok(self
            .db
            .list::<Org>(ORGS_TABLE)?
            .into_iter()
            .map(|(_, v)| v)
            .collect())
    }

    /// Delete an organization.
    pub fn delete_org(&self, name: &str) -> Result<()> {
        if !self.db.delete(ORGS_TABLE, name)? {
            return Err(OrgError::OrgNotFound(name.to_string()));
        }
        Ok(())
    }

    // ── Project operations ──────────────────────────────────────────

    fn project_key(org: &str, project: &str) -> String {
        format!("{org}/{project}")
    }

    /// Create a project within an organization.
    pub fn create_project(&self, org: &str, name: &str) -> Result<Project> {
        // Verify org exists
        if !self.db.exists(ORGS_TABLE, org)? {
            return Err(OrgError::OrgNotFound(org.to_string()));
        }

        let key = Self::project_key(org, name);
        if self.db.exists(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectAlreadyExists(name.to_string()));
        }

        let project = Project {
            id: ProjectId(key.clone()),
            name: name.to_string(),
            org_id: OrgId(org.to_string()),
            created_at: now(),
        };

        self.db.set(PROJECTS_TABLE, &key, &project)?;
        Ok(project)
    }

    /// Get a project by org and name.
    pub fn get_project(&self, org: &str, name: &str) -> Result<Project> {
        let key = Self::project_key(org, name);
        self.db
            .get::<Project>(PROJECTS_TABLE, &key)?
            .ok_or_else(|| OrgError::ProjectNotFound(name.to_string()))
    }

    /// List all projects (optionally filtered by org).
    pub fn list_projects(&self, org: Option<&str>) -> Result<Vec<Project>> {
        let all: Vec<Project> = self
            .db
            .list::<Project>(PROJECTS_TABLE)?
            .into_iter()
            .map(|(_, v)| v)
            .collect();

        match org {
            Some(org_name) => Ok(all.into_iter().filter(|p| p.org_id.0 == org_name).collect()),
            None => Ok(all),
        }
    }

    /// Delete a project.
    pub fn delete_project(&self, org: &str, name: &str) -> Result<()> {
        let key = Self::project_key(org, name);
        if !self.db.delete(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectNotFound(name.to_string()));
        }
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
        // Verify project exists
        let project_key = Self::project_key(org, project);
        if !self.db.exists(PROJECTS_TABLE, &project_key)? {
            return Err(OrgError::ProjectNotFound(project.to_string()));
        }

        let env_key = Self::env_key(org, project, name);
        if self.db.exists(ENVIRONMENTS_TABLE, &env_key)? {
            return Err(OrgError::EnvAlreadyExists(name.to_string()));
        }

        let created_at = now();
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
}

/// Returns the current time as seconds since UNIX epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
        store.create_org("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
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
        assert_eq!(env.project_id, ProjectId("acme/backend".to_string()));
        assert!(!env.deletion_protection);
        assert!(env.ttl.is_none());
        assert!(env.expires_at.is_none());
        assert!(env.labels.is_empty());
    }

    #[test]
    fn duplicate_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        let err = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap_err();

        assert!(
            matches!(err, OrgError::EnvAlreadyExists(ref n) if n == "staging"),
            "expected EnvAlreadyExists, got: {err}"
        );
    }

    #[test]
    fn with_ttl() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let ttl_seconds = 3600; // 1 hour
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

        // Verify labels survive round-trip through persistence.
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

        // Verify it persists.
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
        assert!(
            matches!(err, OrgError::EnvProtected(ref n) if n == "production"),
            "expected EnvProtected, got: {err}"
        );
    }

    #[test]
    fn list_by_project() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        // Also create a second project.
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
        assert!(backend_envs
            .iter()
            .all(|e| e.project_id.0 == "acme/backend"));

        let frontend_envs = store.list_envs("acme", "frontend").unwrap();
        assert_eq!(frontend_envs.len(), 1);
        assert_eq!(frontend_envs[0].name, "staging");
    }

    #[test]
    fn create_env_requires_project() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        // No project created.

        let err = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap_err();
        assert!(
            matches!(err, OrgError::ProjectNotFound(ref n) if n == "backend"),
            "expected ProjectNotFound, got: {err}"
        );
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

    // ── Org and Project tests (minimal, scaffolded for #713/#715) ───

    #[test]
    fn create_and_get_org() {
        let (_dir, store) = temp_store();
        let org = store.create_org("acme").unwrap();
        assert_eq!(org.name, "acme");

        let retrieved = store.get_org("acme").unwrap();
        assert_eq!(retrieved.name, "acme");
    }

    #[test]
    fn create_and_get_project() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();

        let project = store.create_project("acme", "backend").unwrap();
        assert_eq!(project.name, "backend");
        assert_eq!(project.org_id, OrgId("acme".to_string()));

        let retrieved = store.get_project("acme", "backend").unwrap();
        assert_eq!(retrieved.name, "backend");
    }
}
