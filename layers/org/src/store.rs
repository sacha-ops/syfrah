use std::time::{SystemTime, UNIX_EPOCH};

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{Org, OrgId, Project, ProjectId};
use crate::validation::validate_name;

const TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";

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
        validate_name(name)?;

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
        validate_name(name)?;

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
        let db = LayerDb::open_at(&path).unwrap();
        (dir, OrgStore::new(db))
    }

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

        // spaces
        assert!(matches!(
            store.create("my org").unwrap_err(),
            OrgError::InvalidName(_)
        ));
        // uppercase
        assert!(matches!(
            store.create("Acme").unwrap_err(),
            OrgError::InvalidName(_)
        ));
        // special chars
        assert!(matches!(
            store.create("org@1").unwrap_err(),
            OrgError::InvalidName(_)
        ));
        // too short
        assert!(matches!(
            store.create("ab").unwrap_err(),
            OrgError::InvalidName(_)
        ));
        // too long
        assert!(matches!(
            store.create(&"x".repeat(64)).unwrap_err(),
            OrgError::InvalidName(_)
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

        let names: Vec<&str> = orgs.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"gamma"));
    }

    #[test]
    fn get_nonexistent() {
        let (_dir, store) = temp_store();
        assert!(store.get("does-not-exist").unwrap().is_none());
    }

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
        assert!(
            matches!(err, OrgError::ProjectAlreadyExists { .. }),
            "expected ProjectAlreadyExists, got: {err}"
        );
    }

    #[test]
    fn project_invalid_name_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        let err = store.create_project("acme", "ab").unwrap_err();
        assert!(matches!(err, OrgError::InvalidName(_)));

        let err = store.create_project("acme", "MyProject").unwrap_err();
        assert!(matches!(err, OrgError::InvalidName(_)));
    }

    #[test]
    fn delete_project_succeeds_when_empty() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        store.delete_project("acme", "backend").unwrap();

        let fetched = store.get_project("acme", "backend").unwrap();
        assert!(fetched.is_none());
    }

    #[test]
    fn delete_project_with_envs_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        // Simulate an environment existing
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
    fn list_projects_by_org() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create("globex").unwrap();

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
    }

    #[test]
    fn create_project_fails_without_org() {
        let (_dir, store) = temp_store();

        let err = store.create_project("nonexistent", "backend").unwrap_err();
        assert!(
            matches!(err, OrgError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn org_with_projects_cannot_be_deleted() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.delete("acme").unwrap_err();
        assert!(
            matches!(err, OrgError::OrgHasProjects(_)),
            "expected OrgHasProjects, got: {err}"
        );
    }

    #[test]
    fn same_project_name_different_orgs() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create("globex").unwrap();

        store.create_project("acme", "backend").unwrap();
        store.create_project("globex", "backend").unwrap();

        let acme_proj = store.get_project("acme", "backend").unwrap().unwrap();
        let globex_proj = store.get_project("globex", "backend").unwrap().unwrap();

        assert_eq!(acme_proj.org_id, OrgId("acme".to_string()));
        assert_eq!(globex_proj.org_id, OrgId("globex".to_string()));
    }
}
