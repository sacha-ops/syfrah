use std::time::{SystemTime, UNIX_EPOCH};

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{validate_name, Org, OrgId, Project, ProjectId};

const ORGS_TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";

/// Persistence layer for organizations and projects.
///
/// Uses `syfrah-state` (redb) for storage. Orgs are keyed by name.
/// Projects are keyed by "org_name/project_name".
#[derive(Clone, Debug)]
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Open (or create) the org store.
    pub fn open() -> Result<Self> {
        let db = LayerDb::open("org")?;
        Ok(Self { db })
    }

    /// Open with a custom path (for testing).
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let db = LayerDb::open_at(path)?;
        Ok(Self { db })
    }

    // ── Org operations ───────────────────────────────────────────────

    /// Create a new organization.
    pub fn create_org(&self, name: &str) -> Result<Org> {
        validate_name(name).map_err(OrgError::InvalidName)?;

        if self.db.exists(ORGS_TABLE, name)? {
            return Err(OrgError::OrgAlreadyExists(name.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let org = Org {
            id: OrgId(name.to_string()),
            name: name.to_string(),
            created_at: now,
        };

        self.db.set(ORGS_TABLE, name, &org)?;
        Ok(org)
    }

    /// Get an organization by name.
    pub fn get_org(&self, name: &str) -> Result<Option<Org>> {
        Ok(self.db.get(ORGS_TABLE, name)?)
    }

    /// List all organizations.
    pub fn list_orgs(&self) -> Result<Vec<Org>> {
        let entries: Vec<(String, Org)> = self.db.list(ORGS_TABLE)?;
        Ok(entries.into_iter().map(|(_, org)| org).collect())
    }

    /// Delete an organization. Fails if it has any projects.
    pub fn delete_org(&self, name: &str) -> Result<()> {
        if !self.db.exists(ORGS_TABLE, name)? {
            return Err(OrgError::OrgNotFound(name.to_string()));
        }

        // Check for child projects
        let projects = self.list_projects(name)?;
        if !projects.is_empty() {
            return Err(OrgError::OrgHasProjects(name.to_string()));
        }

        self.db.delete(ORGS_TABLE, name)?;
        Ok(())
    }

    // ── Project operations ───────────────────────────────────────────

    /// Build the redb key for a project: "org_name/project_name".
    fn project_key(org: &str, project: &str) -> String {
        format!("{}/{}", org, project)
    }

    /// Create a new project within an organization.
    pub fn create_project(&self, org: &str, name: &str) -> Result<Project> {
        validate_name(name).map_err(OrgError::InvalidName)?;

        // Verify org exists
        if !self.db.exists(ORGS_TABLE, org)? {
            return Err(OrgError::OrgNotFound(org.to_string()));
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
        let env_prefix = format!("{}/{}/", org, name);
        let all_envs: Vec<(String, serde_json::Value)> =
            self.db.list(ENVIRONMENTS_TABLE).unwrap_or_default();
        let has_envs = all_envs.iter().any(|(k, _)| k.starts_with(&env_prefix));

        if has_envs {
            return Err(OrgError::ProjectHasEnvironments {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        self.db.delete(PROJECTS_TABLE, &key)?;
        Ok(())
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

    #[test]
    fn create_project_succeeds_with_valid_org() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();

        let project = store.create_project("acme", "backend").unwrap();
        assert_eq!(project.name, "backend");
        assert_eq!(project.org_id, OrgId("acme".to_string()));

        let fetched = store.get_project("acme", "backend").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "backend");
    }

    #[test]
    fn duplicate_rejected() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.create_project("acme", "backend").unwrap_err();
        assert!(
            matches!(err, OrgError::ProjectAlreadyExists { .. }),
            "expected ProjectAlreadyExists, got: {err}"
        );
    }

    #[test]
    fn invalid_name_rejected() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();

        // Too short
        let err = store.create_project("acme", "ab").unwrap_err();
        assert!(
            matches!(err, OrgError::InvalidName(_)),
            "expected InvalidName, got: {err}"
        );

        // Uppercase
        let err = store.create_project("acme", "MyProject").unwrap_err();
        assert!(
            matches!(err, OrgError::InvalidName(_)),
            "expected InvalidName, got: {err}"
        );

        // Special characters
        let err = store.create_project("acme", "my project").unwrap_err();
        assert!(
            matches!(err, OrgError::InvalidName(_)),
            "expected InvalidName, got: {err}"
        );
    }

    #[test]
    fn delete_project_succeeds_when_empty() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        store.delete_project("acme", "backend").unwrap();

        let fetched = store.get_project("acme", "backend").unwrap();
        assert!(fetched.is_none());
    }

    #[test]
    fn delete_with_envs_rejected() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        // Simulate an environment existing by writing directly to the environments table
        store
            .db
            .set(
                ENVIRONMENTS_TABLE,
                "acme/backend/production",
                &serde_json::json!({"name": "production"}),
            )
            .unwrap();

        let err = store.delete_project("acme", "backend").unwrap_err();
        assert!(
            matches!(err, OrgError::ProjectHasEnvironments { .. }),
            "expected ProjectHasEnvironments, got: {err}"
        );
    }

    #[test]
    fn list_by_org() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_org("globex").unwrap();

        store.create_project("acme", "backend").unwrap();
        store.create_project("acme", "frontend").unwrap();
        store.create_project("globex", "api").unwrap();

        let acme_projects = store.list_projects("acme").unwrap();
        assert_eq!(acme_projects.len(), 2);
        let names: Vec<&str> = acme_projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"backend"));
        assert!(names.contains(&"frontend"));

        let globex_projects = store.list_projects("globex").unwrap();
        assert_eq!(globex_projects.len(), 1);
        assert_eq!(globex_projects[0].name, "api");

        // Empty org
        store.create_org("empty-org").unwrap();
        let empty = store.list_projects("empty-org").unwrap();
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn create_project_fails_without_org() {
        let (_dir, store) = temp_store();

        let err = store.create_project("nonexistent", "backend").unwrap_err();
        assert!(
            matches!(err, OrgError::OrgNotFound(_)),
            "expected OrgNotFound, got: {err}"
        );
    }

    #[test]
    fn delete_nonexistent_project_fails() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();

        let err = store.delete_project("acme", "nonexistent").unwrap_err();
        assert!(
            matches!(err, OrgError::ProjectNotFound { .. }),
            "expected ProjectNotFound, got: {err}"
        );
    }

    #[test]
    fn same_project_name_different_orgs() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_org("globex").unwrap();

        store.create_project("acme", "backend").unwrap();
        store.create_project("globex", "backend").unwrap();

        let acme_proj = store.get_project("acme", "backend").unwrap().unwrap();
        let globex_proj = store.get_project("globex", "backend").unwrap().unwrap();

        assert_eq!(acme_proj.org_id, OrgId("acme".to_string()));
        assert_eq!(globex_proj.org_id, OrgId("globex".to_string()));
    }

    #[test]
    fn org_with_projects_cannot_be_deleted() {
        let (_dir, store) = temp_store();
        store.create_org("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.delete_org("acme").unwrap_err();
        assert!(
            matches!(err, OrgError::OrgHasProjects(_)),
            "expected OrgHasProjects, got: {err}"
        );
    }

    #[test]
    fn org_crud() {
        let (_dir, store) = temp_store();

        // Create
        let org = store.create_org("acme").unwrap();
        assert_eq!(org.name, "acme");

        // Get
        let fetched = store.get_org("acme").unwrap();
        assert!(fetched.is_some());

        // List
        store.create_org("globex").unwrap();
        let orgs = store.list_orgs().unwrap();
        assert_eq!(orgs.len(), 2);

        // Delete
        store.delete_org("globex").unwrap();
        let orgs = store.list_orgs().unwrap();
        assert_eq!(orgs.len(), 1);

        // Duplicate
        let err = store.create_org("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgAlreadyExists(_)));
    }
}
