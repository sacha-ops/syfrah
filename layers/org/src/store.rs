use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{Environment, Org, Project};

const TABLE_ORGS: &str = "orgs";
const TABLE_PROJECTS: &str = "projects";
const TABLE_ENVIRONMENTS: &str = "environments";

/// Persistence layer for org/project/environment hierarchy.
///
/// Uses `LayerDb` (redb) under the hood. Keys are composite strings
/// to ensure uniqueness within parent scope.
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Open or create the org state database.
    pub fn open() -> Result<Self> {
        let db = LayerDb::open("org").map_err(OrgError::State)?;
        Ok(Self { db })
    }

    /// Open with a custom path (for testing).
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let db = LayerDb::open_at(path).map_err(OrgError::State)?;
        Ok(Self { db })
    }

    // --- Orgs ---

    pub fn create_org(&self, org: &Org) -> Result<()> {
        if self
            .db
            .exists(TABLE_ORGS, &org.name)
            .map_err(OrgError::State)?
        {
            return Err(OrgError::AlreadyExists(format!("org '{}'", org.name)));
        }
        self.db
            .set(TABLE_ORGS, &org.name, org)
            .map_err(OrgError::State)
    }

    pub fn get_org(&self, name: &str) -> Result<Option<Org>> {
        self.db.get(TABLE_ORGS, name).map_err(OrgError::State)
    }

    pub fn list_orgs(&self) -> Result<Vec<Org>> {
        let entries: Vec<(String, Org)> = self.db.list(TABLE_ORGS).map_err(OrgError::State)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    pub fn delete_org(&self, name: &str) -> Result<bool> {
        self.db.delete(TABLE_ORGS, name).map_err(OrgError::State)
    }

    // --- Projects ---

    fn project_key(org: &str, name: &str) -> String {
        format!("{org}/{name}")
    }

    pub fn create_project(&self, project: &Project) -> Result<()> {
        let key = Self::project_key(&project.org_id, &project.name);
        if self
            .db
            .exists(TABLE_PROJECTS, &key)
            .map_err(OrgError::State)?
        {
            return Err(OrgError::AlreadyExists(format!(
                "project '{}'",
                project.name
            )));
        }
        self.db
            .set(TABLE_PROJECTS, &key, project)
            .map_err(OrgError::State)
    }

    pub fn get_project(&self, org: &str, name: &str) -> Result<Option<Project>> {
        let key = Self::project_key(org, name);
        self.db.get(TABLE_PROJECTS, &key).map_err(OrgError::State)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let entries: Vec<(String, Project)> =
            self.db.list(TABLE_PROJECTS).map_err(OrgError::State)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    pub fn delete_project(&self, org: &str, name: &str) -> Result<bool> {
        let key = Self::project_key(org, name);
        self.db
            .delete(TABLE_PROJECTS, &key)
            .map_err(OrgError::State)
    }

    // --- Environments ---

    fn env_key(org: &str, project: &str, name: &str) -> String {
        format!("{org}/{project}/{name}")
    }

    pub fn create_env(&self, env: &Environment) -> Result<()> {
        let key = Self::env_key(&env.org_id, &env.project_id, &env.name);
        if self
            .db
            .exists(TABLE_ENVIRONMENTS, &key)
            .map_err(OrgError::State)?
        {
            return Err(OrgError::AlreadyExists(format!(
                "environment '{}'",
                env.name
            )));
        }
        self.db
            .set(TABLE_ENVIRONMENTS, &key, env)
            .map_err(OrgError::State)
    }

    pub fn get_env(&self, org: &str, project: &str, name: &str) -> Result<Option<Environment>> {
        let key = Self::env_key(org, project, name);
        self.db
            .get(TABLE_ENVIRONMENTS, &key)
            .map_err(OrgError::State)
    }

    pub fn update_env(&self, env: &Environment) -> Result<()> {
        let key = Self::env_key(&env.org_id, &env.project_id, &env.name);
        self.db
            .set(TABLE_ENVIRONMENTS, &key, env)
            .map_err(OrgError::State)
    }

    pub fn list_envs(&self) -> Result<Vec<Environment>> {
        let entries: Vec<(String, Environment)> =
            self.db.list(TABLE_ENVIRONMENTS).map_err(OrgError::State)?;
        Ok(entries.into_iter().map(|(_, v)| v).collect())
    }

    pub fn delete_env(&self, org: &str, project: &str, name: &str) -> Result<bool> {
        let key = Self::env_key(org, project, name);
        self.db
            .delete(TABLE_ENVIRONMENTS, &key)
            .map_err(OrgError::State)
    }
}
