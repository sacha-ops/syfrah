//! Persistence layer for organizations, projects, and environments.
//!
//! Uses `syfrah-state` (redb) with three tables:
//! - `orgs`: key = org name
//! - `projects`: key = "org/project"
//! - `environments`: key = "org/project/env"

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use syfrah_state::LayerDb;

use crate::error::OrgError;
use crate::types::{Environment, Org, Project};

const ORGS_TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVS_TABLE: &str = "environments";

/// Composite key for a project: "org/project".
fn project_key(org: &str, project: &str) -> String {
    format!("{org}/{project}")
}

/// Composite key for an environment: "org/project/env".
fn env_key(org: &str, project: &str, env: &str) -> String {
    format!("{org}/{project}/{env}")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Validate a name: lowercase alphanumeric + hyphens + forward slashes, 3-63 chars.
pub fn validate_name(name: &str) -> Result<(), OrgError> {
    if name.len() < 3 || name.len() > 63 {
        return Err(OrgError::InvalidName(
            name.to_string(),
            "must be 3-63 characters".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/')
    {
        return Err(OrgError::InvalidName(
            name.to_string(),
            "only lowercase alphanumeric, hyphens, and forward slashes allowed".to_string(),
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(OrgError::InvalidName(
            name.to_string(),
            "must not start or end with a hyphen".to_string(),
        ));
    }
    Ok(())
}

/// The org store backed by redb.
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Open the org store (creates the database if needed).
    pub fn open() -> Result<Self, OrgError> {
        let db = LayerDb::open("org")?;
        Ok(Self { db })
    }

    /// Open with a custom path (for testing).
    pub fn open_at(path: &std::path::Path) -> Result<Self, OrgError> {
        let db = LayerDb::open_at(path)?;
        Ok(Self { db })
    }

    // ── Organizations ───────────────────────────────────────────

    pub fn create_org(&self, name: &str) -> Result<Org, OrgError> {
        validate_name(name)?;
        if self.db.exists(ORGS_TABLE, name)? {
            return Err(OrgError::OrgExists(name.to_string()));
        }
        let org = Org {
            name: name.to_string(),
            created_at: now_secs(),
        };
        self.db.set(ORGS_TABLE, name, &org)?;
        Ok(org)
    }

    pub fn get_org(&self, name: &str) -> Result<Org, OrgError> {
        self.db
            .get::<Org>(ORGS_TABLE, name)?
            .ok_or_else(|| OrgError::OrgNotFound(name.to_string()))
    }

    pub fn list_orgs(&self) -> Result<Vec<Org>, OrgError> {
        let entries = self.db.list::<Org>(ORGS_TABLE)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    pub fn delete_org(&self, name: &str) -> Result<(), OrgError> {
        // Check existence
        self.get_org(name)?;
        // Check no child projects
        let projects = self.list_projects(Some(name))?;
        if !projects.is_empty() {
            return Err(OrgError::OrgNotEmpty(name.to_string(), projects.len()));
        }
        self.db.delete(ORGS_TABLE, name)?;
        Ok(())
    }

    // ── Projects ────────────────────────────────────────────────

    pub fn create_project(&self, name: &str, org: &str) -> Result<Project, OrgError> {
        validate_name(name)?;
        // Verify org exists
        self.get_org(org)?;
        let key = project_key(org, name);
        if self.db.exists(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectExists(name.to_string(), org.to_string()));
        }
        let project = Project {
            name: name.to_string(),
            org: org.to_string(),
            created_at: now_secs(),
        };
        self.db.set(PROJECTS_TABLE, &key, &project)?;
        Ok(project)
    }

    pub fn get_project(&self, name: &str, org: &str) -> Result<Project, OrgError> {
        let key = project_key(org, name);
        self.db
            .get::<Project>(PROJECTS_TABLE, &key)?
            .ok_or_else(|| OrgError::ProjectNotFound(name.to_string(), org.to_string()))
    }

    pub fn list_projects(&self, org: Option<&str>) -> Result<Vec<Project>, OrgError> {
        let entries = self.db.list::<Project>(PROJECTS_TABLE)?;
        let projects: Vec<Project> = entries
            .into_iter()
            .map(|(_, v)| v)
            .filter(|p| org.is_none() || org == Some(p.org.as_str()))
            .collect();
        Ok(projects)
    }

    pub fn delete_project(&self, name: &str, org: &str) -> Result<(), OrgError> {
        self.get_project(name, org)?;
        let envs = self.list_envs(Some(name), Some(org))?;
        if !envs.is_empty() {
            return Err(OrgError::ProjectNotEmpty(name.to_string(), envs.len()));
        }
        let key = project_key(org, name);
        self.db.delete(PROJECTS_TABLE, &key)?;
        Ok(())
    }

    // ── Environments ────────────────────────────────────────────

    pub fn create_env(
        &self,
        name: &str,
        project: &str,
        org: &str,
        ttl: Option<u64>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    ) -> Result<Environment, OrgError> {
        validate_name(name)?;
        // Verify project exists
        self.get_project(project, org)?;
        let key = env_key(org, project, name);
        if self.db.exists(ENVS_TABLE, &key)? {
            return Err(OrgError::EnvExists(name.to_string(), project.to_string()));
        }
        let env = Environment {
            name: name.to_string(),
            project: project.to_string(),
            org: org.to_string(),
            ttl,
            deletion_protection,
            labels,
            created_at: now_secs(),
        };
        self.db.set(ENVS_TABLE, &key, &env)?;
        Ok(env)
    }

    pub fn get_env(&self, name: &str, project: &str, org: &str) -> Result<Environment, OrgError> {
        let key = env_key(org, project, name);
        self.db
            .get::<Environment>(ENVS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string(), project.to_string()))
    }

    pub fn list_envs(
        &self,
        project: Option<&str>,
        org: Option<&str>,
    ) -> Result<Vec<Environment>, OrgError> {
        let entries = self.db.list::<Environment>(ENVS_TABLE)?;
        let envs: Vec<Environment> = entries
            .into_iter()
            .map(|(_, v)| v)
            .filter(|e| project.is_none() || project == Some(e.project.as_str()))
            .filter(|e| org.is_none() || org == Some(e.org.as_str()))
            .collect();
        Ok(envs)
    }

    /// Delete an environment. Returns `EnvProtected` error if deletion protection is enabled.
    pub fn delete_env(&self, name: &str, project: &str, org: &str) -> Result<(), OrgError> {
        let env = self.get_env(name, project, org)?;
        if env.deletion_protection {
            return Err(OrgError::EnvProtected(
                name.to_string(),
                project.to_string(),
                org.to_string(),
            ));
        }
        let key = env_key(org, project, name);
        self.db.delete(ENVS_TABLE, &key)?;
        Ok(())
    }

    /// Toggle deletion protection on an environment.
    pub fn update_env_protection(
        &self,
        name: &str,
        project: &str,
        org: &str,
        enabled: bool,
    ) -> Result<Environment, OrgError> {
        let mut env = self.get_env(name, project, org)?;
        env.deletion_protection = enabled;
        let key = env_key(org, project, name);
        self.db.set(ENVS_TABLE, &key, &env)?;
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org-test.redb");
        let store = OrgStore::open_at(&path).unwrap();
        (dir, store)
    }

    // ── Name validation ─────────────────────────────────────────

    #[test]
    fn valid_names() {
        assert!(validate_name("acme").is_ok());
        assert!(validate_name("my-org").is_ok());
        assert!(validate_name("feat/auth-v2").is_ok());
        assert!(validate_name("ci/pr-247").is_ok());
    }

    #[test]
    fn invalid_names() {
        assert!(validate_name("ab").is_err()); // too short
        assert!(validate_name(&"a".repeat(64)).is_err()); // too long
        assert!(validate_name("My-Org").is_err()); // uppercase
        assert!(validate_name("-bad").is_err()); // starts with hyphen
        assert!(validate_name("bad-").is_err()); // ends with hyphen
    }

    // ── Org CRUD ────────────────────────────────────────────────

    #[test]
    fn create_and_get_org() {
        let (_dir, store) = temp_store();
        let org = store.create_org("acme").unwrap();
        assert_eq!(org.name, "acme");

        let fetched = store.get_org("acme").unwrap();
        assert_eq!(fetched.name, "acme");
    }

    #[test]
    fn duplicate_org_rejected() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        let err = store.create_org("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgExists(_)));
    }

    #[test]
    fn delete_org() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.delete_org("acme").unwrap();
        assert!(matches!(
            store.get_org("acme").unwrap_err(),
            OrgError::OrgNotFound(_)
        ));
    }

    #[test]
    fn delete_org_with_projects_rejected() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        let err = store.delete_org("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgNotEmpty(_, 1)));
    }

    // ── Project CRUD ────────────────────────────────────────────

    #[test]
    fn create_and_get_project() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        let project = store.create_project("backend", "acme").unwrap();
        assert_eq!(project.name, "backend");
        assert_eq!(project.org, "acme");
    }

    #[test]
    fn project_requires_org() {
        let (_dir, store) = temp_store();
        let err = store.create_project("backend", "nonexistent").unwrap_err();
        assert!(matches!(err, OrgError::OrgNotFound(_)));
    }

    // ── Environment CRUD ────────────────────────────────────────

    #[test]
    fn create_env_with_protection() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        let env = store
            .create_env("production", "backend", "acme", None, true, HashMap::new())
            .unwrap();
        assert!(env.deletion_protection);
    }

    #[test]
    fn protected_env_cannot_delete() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        store
            .create_env("production", "backend", "acme", None, true, HashMap::new())
            .unwrap();

        let err = store
            .delete_env("production", "backend", "acme")
            .unwrap_err();
        match &err {
            OrgError::EnvProtected(name, project, org) => {
                assert_eq!(name, "production");
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                // Verify the error message contains actionable instructions
                let msg = err.to_string();
                assert!(msg.contains("deletion protection enabled"));
                assert!(msg.contains("syfrah env update production"));
                assert!(msg.contains("--no-deletion-protection"));
            }
            other => panic!("expected EnvProtected, got: {other:?}"),
        }
    }

    #[test]
    fn unprotect_then_delete() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        store
            .create_env("production", "backend", "acme", None, true, HashMap::new())
            .unwrap();

        // Cannot delete while protected
        assert!(store.delete_env("production", "backend", "acme").is_err());

        // Disable protection
        let env = store
            .update_env_protection("production", "backend", "acme", false)
            .unwrap();
        assert!(!env.deletion_protection);

        // Now delete succeeds
        store.delete_env("production", "backend", "acme").unwrap();

        // Verify it's gone
        assert!(matches!(
            store.get_env("production", "backend", "acme").unwrap_err(),
            OrgError::EnvNotFound(_, _)
        ));
    }

    #[test]
    fn unprotected_env_deletes_directly() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        store
            .create_env("staging", "backend", "acme", None, false, HashMap::new())
            .unwrap();
        store.delete_env("staging", "backend", "acme").unwrap();
    }

    #[test]
    fn toggle_protection() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        store
            .create_env("staging", "backend", "acme", None, false, HashMap::new())
            .unwrap();

        // Enable
        let env = store
            .update_env_protection("staging", "backend", "acme", true)
            .unwrap();
        assert!(env.deletion_protection);

        // Disable
        let env = store
            .update_env_protection("staging", "backend", "acme", false)
            .unwrap();
        assert!(!env.deletion_protection);
    }

    #[test]
    fn delete_project_with_envs_rejected() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        store
            .create_env("staging", "backend", "acme", None, false, HashMap::new())
            .unwrap();
        let err = store.delete_project("backend", "acme").unwrap_err();
        assert!(matches!(err, OrgError::ProjectNotEmpty(_, 1)));
    }

    #[test]
    fn env_with_labels() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        let mut labels = HashMap::new();
        labels.insert("region".to_string(), "eu-west".to_string());
        labels.insert("team".to_string(), "payments".to_string());
        let env = store
            .create_env("production", "backend", "acme", None, false, labels)
            .unwrap();
        assert_eq!(env.labels.get("region"), Some(&"eu-west".to_string()));
        assert_eq!(env.labels.get("team"), Some(&"payments".to_string()));
    }

    #[test]
    fn env_with_ttl() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        let env = store
            .create_env(
                "feat/auth-v2",
                "backend",
                "acme",
                Some(172800),
                false,
                HashMap::new(),
            )
            .unwrap();
        assert_eq!(env.ttl, Some(172800));
    }

    #[test]
    fn list_envs_filtered() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("backend", "acme").unwrap();
        store.create_project("frontend", "acme").unwrap();
        store
            .create_env("staging", "backend", "acme", None, false, HashMap::new())
            .unwrap();
        store
            .create_env("prod", "backend", "acme", None, false, HashMap::new())
            .unwrap();
        store
            .create_env("staging", "frontend", "acme", None, false, HashMap::new())
            .unwrap();

        let backend_envs = store.list_envs(Some("backend"), Some("acme")).unwrap();
        assert_eq!(backend_envs.len(), 2);

        let all_envs = store.list_envs(None, None).unwrap();
        assert_eq!(all_envs.len(), 3);
    }
}
