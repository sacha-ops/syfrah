//! TTL enforcement logic for ephemeral environments.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::store::OrgStore;
use crate::types::Environment;

/// Returns the current Unix epoch in seconds.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns all environments whose TTL has expired across all orgs and projects.
///
/// An environment is expired when `now > expires_at`.
/// Environments without an `expires_at` are permanent and never expire.
pub fn find_expired_envs(store: &OrgStore) -> Result<Vec<(String, String, Environment)>> {
    let now = now_epoch();
    let mut expired = Vec::new();

    // Iterate all orgs, then all projects, then all envs
    let orgs = store.list()?;
    for org in &orgs {
        let projects = store.list_projects(&org.name)?;
        for project in &projects {
            let envs = store.list_envs(&org.name, &project.name)?;
            for env in envs {
                if let Some(expires_at) = env.expires_at {
                    if now > expires_at {
                        expired.push((org.name.clone(), project.name.clone(), env));
                    }
                }
            }
        }
    }

    Ok(expired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-ttl.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();
        (dir, OrgStore::new(db))
    }

    #[test]
    fn expired_env_detected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        // Create env with TTL of 1 second (will expire immediately)
        store
            .create_env("acme", "backend", "ci-run", Some(1), false, HashMap::new())
            .unwrap();

        // Wait a moment for it to expire
        std::thread::sleep(std::time::Duration::from_secs(2));

        let expired = find_expired_envs(&store).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].2.name, "ci-run");
    }

    #[test]
    fn non_expired_env_not_returned() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        // Create env with long TTL
        store
            .create_env(
                "acme",
                "backend",
                "staging",
                Some(86400),
                false,
                HashMap::new(),
            )
            .unwrap();

        // Also create a permanent env (no TTL)
        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();

        let expired = find_expired_envs(&store).unwrap();
        assert!(expired.is_empty());
    }
}
