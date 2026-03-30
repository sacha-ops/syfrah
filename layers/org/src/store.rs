//! Persistence layer for organizations using redb via syfrah-state.
//!
//! Orgs are stored in the `orgs` table, keyed by name.

use syfrah_state::LayerDb;

use crate::types::Org;

/// The redb table name for organizations.
const TABLE: &str = "orgs";

/// The layer name for the org database file (~/.syfrah/org.redb).
const LAYER: &str = "org";

/// Org store backed by redb.
#[derive(Clone)]
pub struct OrgStore {
    db: LayerDb,
}

/// Errors from the org store.
#[derive(Debug, thiserror::Error)]
pub enum OrgStoreError {
    #[error("org '{0}' already exists")]
    AlreadyExists(String),
    #[error("org '{0}' not found")]
    NotFound(String),
    #[error("storage error: {0}")]
    State(#[from] syfrah_state::StateError),
}

pub type Result<T> = std::result::Result<T, OrgStoreError>;

impl OrgStore {
    /// Open the org store (creates the database file if needed).
    pub fn open() -> Result<Self> {
        let db = LayerDb::open(LAYER)?;
        Ok(Self { db })
    }

    /// Open with a custom database path (for testing).
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let db = LayerDb::open_at(path)?;
        Ok(Self { db })
    }

    /// Create a new org. Returns error if it already exists.
    pub fn create(&self, org: &Org) -> Result<()> {
        if self.db.exists(TABLE, &org.name)? {
            return Err(OrgStoreError::AlreadyExists(org.name.clone()));
        }
        self.db.set(TABLE, &org.name, org)?;
        Ok(())
    }

    /// Get an org by name. Returns None if not found.
    pub fn get(&self, name: &str) -> Result<Option<Org>> {
        Ok(self.db.get(TABLE, name)?)
    }

    /// List all orgs, sorted by name.
    pub fn list(&self) -> Result<Vec<Org>> {
        let entries: Vec<(String, Org)> = self.db.list(TABLE)?;
        let mut orgs: Vec<Org> = entries.into_iter().map(|(_, org)| org).collect();
        orgs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(orgs)
    }

    /// Delete an org by name. Returns error if not found.
    pub fn delete(&self, name: &str) -> Result<()> {
        if !self.db.exists(TABLE, name)? {
            return Err(OrgStoreError::NotFound(name.to_string()));
        }
        self.db.delete(TABLE, name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-org.redb");
        let store = OrgStore::open_at(&path).unwrap();
        (dir, store)
    }

    #[test]
    fn create_and_get() {
        let (_dir, store) = temp_store();
        let org = Org {
            name: "acme".to_string(),
            created_at: 1000,
        };
        store.create(&org).unwrap();
        let got = store.get("acme").unwrap();
        assert_eq!(got, Some(org));
    }

    #[test]
    fn create_duplicate_fails() {
        let (_dir, store) = temp_store();
        let org = Org {
            name: "acme".to_string(),
            created_at: 1000,
        };
        store.create(&org).unwrap();
        let err = store.create(&org).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn get_missing_returns_none() {
        let (_dir, store) = temp_store();
        assert_eq!(store.get("nope").unwrap(), None);
    }

    #[test]
    fn list_orgs() {
        let (_dir, store) = temp_store();
        store
            .create(&Org {
                name: "beta".to_string(),
                created_at: 2000,
            })
            .unwrap();
        store
            .create(&Org {
                name: "alpha".to_string(),
                created_at: 1000,
            })
            .unwrap();
        let orgs = store.list().unwrap();
        assert_eq!(orgs.len(), 2);
        assert_eq!(orgs[0].name, "alpha");
        assert_eq!(orgs[1].name, "beta");
    }

    #[test]
    fn list_empty() {
        let (_dir, store) = temp_store();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn delete_org() {
        let (_dir, store) = temp_store();
        let org = Org {
            name: "acme".to_string(),
            created_at: 1000,
        };
        store.create(&org).unwrap();
        store.delete("acme").unwrap();
        assert_eq!(store.get("acme").unwrap(), None);
    }

    #[test]
    fn delete_missing_fails() {
        let (_dir, store) = temp_store();
        let err = store.delete("nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
