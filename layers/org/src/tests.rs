//! Full hierarchy integration test: Org -> Project -> Environment lifecycle.

use std::collections::HashMap;

use crate::error::OrgError;
use crate::store::OrgStore;

fn temp_store() -> (tempfile::TempDir, OrgStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("org-test.redb");
    let db = syfrah_state::LayerDb::open_at(&path).unwrap();
    (dir, OrgStore::new(db))
}

#[test]
fn org_project_env_lifecycle() {
    let (_dir, store) = temp_store();

    // --- Create org "acme" ---
    let acme = store.create("acme").unwrap();
    assert_eq!(acme.name, "acme");

    // --- Create project "backend" under acme ---
    let backend = store.create_project("acme", "backend").unwrap();
    assert_eq!(backend.name, "backend");

    // --- Create project "frontend" under acme ---
    let frontend = store.create_project("acme", "frontend").unwrap();
    assert_eq!(frontend.name, "frontend");

    // --- Create env "production" under backend (with deletion_protection) ---
    let production = store
        .create_env(
            "acme",
            "backend",
            "production",
            None,
            true, // deletion_protection
            HashMap::new(),
        )
        .unwrap();
    assert!(production.deletion_protection);

    // --- Create env "staging" under backend (with TTL 48h) ---
    let staging = store
        .create_env(
            "acme",
            "backend",
            "staging",
            Some(48 * 3600),
            false,
            HashMap::new(),
        )
        .unwrap();
    assert_eq!(staging.ttl, Some(48 * 3600));
    assert!(staging.expires_at.is_some());

    // --- Create env "dev" under frontend (with labels team=fe) ---
    let mut dev_labels = HashMap::new();
    dev_labels.insert("team".to_string(), "fe".to_string());
    let dev = store
        .create_env("acme", "frontend", "dev", None, false, dev_labels.clone())
        .unwrap();
    assert_eq!(dev.labels, dev_labels);

    // --- List all: verify 1 org, 2 projects, 3 envs ---
    let orgs = store.list().unwrap();
    assert_eq!(orgs.len(), 1, "expected 1 org");
    assert_eq!(orgs[0].name, "acme");

    let backend_projects = store.list_projects("acme").unwrap();
    assert_eq!(backend_projects.len(), 2, "expected 2 projects");

    let backend_envs = store.list_envs("acme", "backend").unwrap();
    assert_eq!(backend_envs.len(), 2, "expected 2 backend envs");

    let frontend_envs = store.list_envs("acme", "frontend").unwrap();
    assert_eq!(frontend_envs.len(), 1, "expected 1 frontend env");

    // --- Try delete project "backend" -> rejected (has envs) ---
    let err = store.delete_project("acme", "backend").unwrap_err();
    assert!(
        matches!(err, OrgError::ProjectHasEnvironments { .. }),
        "expected ProjectHasEnvironments, got: {err}"
    );

    // --- Delete env "staging" -> OK ---
    store.delete_env("acme", "backend", "staging").unwrap();

    // --- Try delete env "production" -> rejected (protected) ---
    let err = store
        .delete_env("acme", "backend", "production")
        .unwrap_err();
    assert!(
        matches!(err, OrgError::EnvProtected(_)),
        "expected EnvProtected, got: {err}"
    );

    // --- Unprotect "production", then delete -> OK ---
    store
        .update_env_protection("acme", "backend", "production", false)
        .unwrap();
    store.delete_env("acme", "backend", "production").unwrap();

    // --- Delete env "dev" -> OK ---
    store.delete_env("acme", "frontend", "dev").unwrap();

    // --- Delete project "backend" -> OK (no more envs) ---
    store.delete_project("acme", "backend").unwrap();

    // --- Delete project "frontend" -> OK ---
    store.delete_project("acme", "frontend").unwrap();

    // --- Delete org "acme" -> OK ---
    store.delete("acme").unwrap();

    // --- Verify all empty ---
    assert_eq!(store.list().unwrap().len(), 0, "orgs should be empty");
    assert_eq!(
        store.list_projects("acme").unwrap().len(),
        0,
        "projects should be empty"
    );
}
