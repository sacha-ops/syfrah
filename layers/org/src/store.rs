//! Persistence for orgs and projects via `syfrah-state`.
//!
//! Tables:
//! - `orgs`: key = org name, value = `Org`
//! - `projects`: key = "{org}/{project}", value = `Project`

use syfrah_state::LayerDb;

use crate::types::{Org, Project};

const TABLE_ORGS: &str = "orgs";
const TABLE_PROJECTS: &str = "projects";

/// Errors from org store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    State(#[from] syfrah_state::StateError),
    #[error("org '{0}' not found")]
    OrgNotFound(String),
    #[error("org '{0}' already exists")]
    OrgAlreadyExists(String),
    #[error("project '{0}' already exists in org '{1}'")]
    ProjectAlreadyExists(String, String),
    #[error("project '{0}' not found in org '{1}'")]
    ProjectNotFound(String, String),
    #[error("org '{0}' has {1} project(s) — delete them first")]
    OrgNotEmpty(String, usize),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Open the org layer database.
pub fn open() -> Result<LayerDb> {
    Ok(LayerDb::open("org")?)
}

fn project_key(org: &str, project: &str) -> String {
    format!("{org}/{project}")
}

// ── Org operations ──────────────────────────────────────────────

pub fn create_org(db: &LayerDb, name: &str) -> Result<Org> {
    if db.exists(TABLE_ORGS, name)? {
        return Err(StoreError::OrgAlreadyExists(name.to_string()));
    }
    let org = Org {
        name: name.to_string(),
        created_at: now(),
    };
    db.set(TABLE_ORGS, name, &org)?;
    Ok(org)
}

pub fn get_org(db: &LayerDb, name: &str) -> Result<Org> {
    db.get::<Org>(TABLE_ORGS, name)?
        .ok_or_else(|| StoreError::OrgNotFound(name.to_string()))
}

pub fn list_orgs(db: &LayerDb) -> Result<Vec<Org>> {
    let entries: Vec<(String, Org)> = db.list(TABLE_ORGS)?;
    Ok(entries.into_iter().map(|(_, o)| o).collect())
}

pub fn delete_org(db: &LayerDb, name: &str) -> Result<()> {
    if !db.exists(TABLE_ORGS, name)? {
        return Err(StoreError::OrgNotFound(name.to_string()));
    }
    // Check for projects
    let projects = list_projects_by_org(db, name)?;
    if !projects.is_empty() {
        return Err(StoreError::OrgNotEmpty(name.to_string(), projects.len()));
    }
    db.delete(TABLE_ORGS, name)?;
    Ok(())
}

// ── Project operations ──────────────────────────────────────────

pub fn create_project(db: &LayerDb, name: &str, org: &str) -> Result<Project> {
    // Verify org exists
    if !db.exists(TABLE_ORGS, org)? {
        return Err(StoreError::OrgNotFound(org.to_string()));
    }
    let key = project_key(org, name);
    if db.exists(TABLE_PROJECTS, &key)? {
        return Err(StoreError::ProjectAlreadyExists(
            name.to_string(),
            org.to_string(),
        ));
    }
    let project = Project {
        name: name.to_string(),
        org: org.to_string(),
        created_at: now(),
    };
    db.set(TABLE_PROJECTS, &key, &project)?;
    Ok(project)
}

pub fn list_projects(db: &LayerDb) -> Result<Vec<Project>> {
    let entries: Vec<(String, Project)> = db.list(TABLE_PROJECTS)?;
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

pub fn list_projects_by_org(db: &LayerDb, org: &str) -> Result<Vec<Project>> {
    let all = list_projects(db)?;
    Ok(all.into_iter().filter(|p| p.org == org).collect())
}

pub fn delete_project(db: &LayerDb, name: &str, org: &str) -> Result<()> {
    if !db.exists(TABLE_ORGS, org)? {
        return Err(StoreError::OrgNotFound(org.to_string()));
    }
    let key = project_key(org, name);
    if !db.exists(TABLE_PROJECTS, &key)? {
        return Err(StoreError::ProjectNotFound(
            name.to_string(),
            org.to_string(),
        ));
    }
    db.delete(TABLE_PROJECTS, &key)?;
    Ok(())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn create_and_get_org() {
        let (_dir, db) = temp_db();
        let org = create_org(&db, "acme").unwrap();
        assert_eq!(org.name, "acme");
        let fetched = get_org(&db, "acme").unwrap();
        assert_eq!(fetched.name, "acme");
    }

    #[test]
    fn duplicate_org_rejected() {
        let (_dir, db) = temp_db();
        create_org(&db, "acme").unwrap();
        let err = create_org(&db, "acme").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn delete_org_with_projects_rejected() {
        let (_dir, db) = temp_db();
        create_org(&db, "acme").unwrap();
        create_project(&db, "backend", "acme").unwrap();
        let err = delete_org(&db, "acme").unwrap_err();
        assert!(err.to_string().contains("1 project(s)"));
    }

    #[test]
    fn create_project_requires_org() {
        let (_dir, db) = temp_db();
        let err = create_project(&db, "backend", "nonexistent").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn create_and_list_projects() {
        let (_dir, db) = temp_db();
        create_org(&db, "acme").unwrap();
        create_project(&db, "backend", "acme").unwrap();
        create_project(&db, "frontend", "acme").unwrap();
        let projects = list_projects_by_org(&db, "acme").unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn duplicate_project_rejected() {
        let (_dir, db) = temp_db();
        create_org(&db, "acme").unwrap();
        create_project(&db, "backend", "acme").unwrap();
        let err = create_project(&db, "backend", "acme").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn delete_project_ok() {
        let (_dir, db) = temp_db();
        create_org(&db, "acme").unwrap();
        create_project(&db, "backend", "acme").unwrap();
        super::delete_project(&db, "backend", "acme").unwrap();
        let projects = list_projects_by_org(&db, "acme").unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn delete_nonexistent_project_err() {
        let (_dir, db) = temp_db();
        create_org(&db, "acme").unwrap();
        let err = super::delete_project(&db, "nope", "acme").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
