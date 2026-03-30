use syfrah_state::LayerDb;

use crate::types::{Environment, EnvironmentId, Org, OrgId, Project, ProjectId};

/// Table names in the org redb database.
const ORGS_TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";

/// Errors from the org store.
#[derive(Debug, thiserror::Error)]
pub enum OrgStoreError {
    #[error("state error: {0}")]
    State(#[from] syfrah_state::StateError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("has children: {0}")]
    HasChildren(String),
    #[error("deletion protected: {0}")]
    DeletionProtected(String),
    #[error("validation error: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, OrgStoreError>;

/// Persistence layer for the org hierarchy (Org -> Project -> Environment).
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Create a new OrgStore backed by the given LayerDb.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    // --- Org ---

    pub fn create_org(&self, org: &Org) -> Result<()> {
        if self.db.exists(ORGS_TABLE, &org.id.0)? {
            return Err(OrgStoreError::AlreadyExists(format!("org '{}'", org.name)));
        }
        self.db.set(ORGS_TABLE, &org.id.0, org)?;
        Ok(())
    }

    pub fn get_org(&self, id: &OrgId) -> Result<Org> {
        self.db
            .get::<Org>(ORGS_TABLE, &id.0)?
            .ok_or_else(|| OrgStoreError::NotFound(format!("org '{}'", id.0)))
    }

    pub fn list_orgs(&self) -> Result<Vec<Org>> {
        Ok(self
            .db
            .list::<Org>(ORGS_TABLE)?
            .into_iter()
            .map(|(_, v)| v)
            .collect())
    }

    pub fn delete_org(&self, id: &OrgId) -> Result<()> {
        // Check org exists.
        let _ = self.get_org(id)?;

        // Reject if org has projects.
        let projects = self.list_projects_by_org(id)?;
        if !projects.is_empty() {
            return Err(OrgStoreError::HasChildren(format!(
                "org '{}' has {} project(s)",
                id.0,
                projects.len()
            )));
        }

        self.db.delete(ORGS_TABLE, &id.0)?;
        Ok(())
    }

    // --- Project ---

    pub fn create_project(&self, project: &Project) -> Result<()> {
        // Verify parent org exists.
        let _ = self.get_org(&project.org_id)?;

        if self.db.exists(PROJECTS_TABLE, &project.id.0)? {
            return Err(OrgStoreError::AlreadyExists(format!(
                "project '{}'",
                project.name
            )));
        }
        self.db.set(PROJECTS_TABLE, &project.id.0, project)?;
        Ok(())
    }

    pub fn get_project(&self, id: &ProjectId) -> Result<Project> {
        self.db
            .get::<Project>(PROJECTS_TABLE, &id.0)?
            .ok_or_else(|| OrgStoreError::NotFound(format!("project '{}'", id.0)))
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        Ok(self
            .db
            .list::<Project>(PROJECTS_TABLE)?
            .into_iter()
            .map(|(_, v)| v)
            .collect())
    }

    pub fn list_projects_by_org(&self, org_id: &OrgId) -> Result<Vec<Project>> {
        Ok(self
            .list_projects()?
            .into_iter()
            .filter(|p| p.org_id == *org_id)
            .collect())
    }

    pub fn delete_project(&self, id: &ProjectId) -> Result<()> {
        // Check project exists.
        let _ = self.get_project(id)?;

        // Reject if project has environments.
        let envs = self.list_envs_by_project(id)?;
        if !envs.is_empty() {
            return Err(OrgStoreError::HasChildren(format!(
                "project '{}' has {} environment(s)",
                id.0,
                envs.len()
            )));
        }

        self.db.delete(PROJECTS_TABLE, &id.0)?;
        Ok(())
    }

    // --- Environment ---

    pub fn create_env(&self, env: &Environment) -> Result<()> {
        // Verify parent project exists.
        let _ = self.get_project(&env.project_id)?;

        if self.db.exists(ENVIRONMENTS_TABLE, &env.id.0)? {
            return Err(OrgStoreError::AlreadyExists(format!("env '{}'", env.name)));
        }
        self.db.set(ENVIRONMENTS_TABLE, &env.id.0, env)?;
        Ok(())
    }

    pub fn get_env(&self, id: &EnvironmentId) -> Result<Environment> {
        self.db
            .get::<Environment>(ENVIRONMENTS_TABLE, &id.0)?
            .ok_or_else(|| OrgStoreError::NotFound(format!("env '{}'", id.0)))
    }

    pub fn list_envs(&self) -> Result<Vec<Environment>> {
        Ok(self
            .db
            .list::<Environment>(ENVIRONMENTS_TABLE)?
            .into_iter()
            .map(|(_, v)| v)
            .collect())
    }

    pub fn list_envs_by_project(&self, project_id: &ProjectId) -> Result<Vec<Environment>> {
        Ok(self
            .list_envs()?
            .into_iter()
            .filter(|e| e.project_id == *project_id)
            .collect())
    }

    pub fn delete_env(&self, id: &EnvironmentId) -> Result<()> {
        let env = self.get_env(id)?;

        if env.deletion_protection {
            return Err(OrgStoreError::DeletionProtected(format!(
                "env '{}' has deletion protection enabled",
                env.name
            )));
        }

        self.db.delete(ENVIRONMENTS_TABLE, &id.0)?;
        Ok(())
    }

    /// Update an environment's deletion_protection flag.
    pub fn set_deletion_protection(&self, id: &EnvironmentId, protected: bool) -> Result<()> {
        let mut env = self.get_env(id)?;
        env.deletion_protection = protected;
        self.db.set(ENVIRONMENTS_TABLE, &id.0, &env)?;
        Ok(())
    }
}
