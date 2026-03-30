use crate::error::{OrgError, Result};
use crate::store::OrgStore;
use crate::types::{now_epoch, Environment};

/// Returns all environments whose TTL has expired.
///
/// An environment is expired when `now > expires_at`.
/// Environments without an `expires_at` are permanent and never expire.
pub fn check_expired_envs(store: &OrgStore) -> Result<Vec<Environment>> {
    let now = now_epoch();
    let all_envs = store.list_envs()?;
    Ok(all_envs.into_iter().filter(|e| e.is_expired(now)).collect())
}

/// Extend an environment's TTL by adding `additional_secs` to its `expires_at`.
///
/// If the environment has no current `expires_at`, the new expiry is computed
/// from the current time plus `additional_secs`.
pub fn extend_env(
    store: &OrgStore,
    org: &str,
    project: &str,
    name: &str,
    additional_secs: u64,
) -> Result<Environment> {
    let mut env = store
        .get_env(org, project, name)?
        .ok_or_else(|| OrgError::NotFound(format!("environment '{name}'")))?;

    let now = now_epoch();
    let base = env.expires_at.unwrap_or(now);
    // If the current expiry is in the past, extend from now instead.
    let effective_base = if base < now { now } else { base };
    env.expires_at = Some(effective_base + additional_secs);
    env.ttl_secs = Some(additional_secs);

    store.update_env(&env)?;
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Environment;
    use std::collections::HashMap;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-org.redb");
        let store = OrgStore::open_at(&path).unwrap();
        (dir, store)
    }

    fn make_env(org: &str, project: &str, name: &str, expires_at: Option<u64>) -> Environment {
        Environment {
            id: format!("{org}/{project}/{name}"),
            name: name.to_string(),
            project_id: project.to_string(),
            org_id: org.to_string(),
            ttl_secs: expires_at.map(|_| 3600),
            expires_at,
            deletion_protection: false,
            labels: HashMap::new(),
            created_at: 1000,
        }
    }

    #[test]
    fn ttl_expired_detected() {
        let (_dir, store) = temp_store();
        // Create an env that expired 100 seconds ago
        let past = now_epoch() - 100;
        let env = make_env("acme", "backend", "ci-run", Some(past));
        store.create_env(&env).unwrap();

        let expired = check_expired_envs(&store).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].name, "ci-run");
    }

    #[test]
    fn ttl_not_expired_kept() {
        let (_dir, store) = temp_store();
        // Create an env that expires 1 hour from now
        let future = now_epoch() + 3600;
        let env = make_env("acme", "backend", "staging", Some(future));
        store.create_env(&env).unwrap();

        // Also create a permanent env (no TTL)
        let permanent = make_env("acme", "backend", "production", None);
        store.create_env(&permanent).unwrap();

        let expired = check_expired_envs(&store).unwrap();
        assert!(expired.is_empty());
    }

    #[test]
    fn extend_resets_ttl() {
        let (_dir, store) = temp_store();
        let now = now_epoch();
        // Env expires in 10 seconds
        let env = make_env("acme", "backend", "feat-branch", Some(now + 10));
        store.create_env(&env).unwrap();

        // Extend by 1 hour
        let updated = extend_env(&store, "acme", "backend", "feat-branch", 3600).unwrap();

        // New expires_at should be original expiry + 3600
        assert_eq!(updated.expires_at, Some(now + 10 + 3600));

        // Verify it's persisted
        let loaded = store
            .get_env("acme", "backend", "feat-branch")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.expires_at, updated.expires_at);
    }
}
