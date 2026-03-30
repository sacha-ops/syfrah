use std::collections::HashMap;
use std::time::Duration;

use crate::store::{OrgStore, OrgStoreError};
use crate::types::{Environment, EnvironmentId, Org, OrgId, Project, ProjectId};

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
    let acme = Org::new("acme".to_string()).unwrap();
    store.create_org(&acme).unwrap();

    // --- Create project "backend" under acme ---
    let backend = Project::new("backend".to_string(), OrgId("acme".to_string())).unwrap();
    store.create_project(&backend).unwrap();

    // --- Create project "frontend" under acme ---
    let frontend = Project::new("frontend".to_string(), OrgId("acme".to_string())).unwrap();
    store.create_project(&frontend).unwrap();

    // --- Create env "production" under backend (with deletion_protection) ---
    let production = Environment::new(
        "production".to_string(),
        ProjectId("acme/backend".to_string()),
        None,
        true, // deletion_protection
        HashMap::new(),
    )
    .unwrap();
    store.create_env(&production).unwrap();

    // --- Create env "staging" under backend (with TTL 48h) ---
    let staging = Environment::new(
        "staging".to_string(),
        ProjectId("acme/backend".to_string()),
        Some(Duration::from_secs(48 * 3600)),
        false,
        HashMap::new(),
    )
    .unwrap();
    store.create_env(&staging).unwrap();

    // --- Create env "dev" under frontend (with labels team=fe) ---
    let mut dev_labels = HashMap::new();
    dev_labels.insert("team".to_string(), "fe".to_string());
    let dev = Environment::new(
        "dev".to_string(),
        ProjectId("acme/frontend".to_string()),
        None,
        false,
        dev_labels,
    )
    .unwrap();
    store.create_env(&dev).unwrap();

    // --- List all: verify 1 org, 2 projects, 3 envs ---
    let orgs = store.list_orgs().unwrap();
    assert_eq!(orgs.len(), 1, "expected 1 org");
    assert_eq!(orgs[0].name, "acme");

    let projects = store.list_projects().unwrap();
    assert_eq!(projects.len(), 2, "expected 2 projects");

    let envs = store.list_envs().unwrap();
    assert_eq!(envs.len(), 3, "expected 3 envs");

    // --- Try delete project "backend" -> rejected (has envs) ---
    let err = store
        .delete_project(&ProjectId("acme/backend".to_string()))
        .unwrap_err();
    assert!(
        matches!(err, OrgStoreError::HasChildren(_)),
        "expected HasChildren, got: {err}"
    );

    // --- Delete env "staging" -> OK ---
    store
        .delete_env(&EnvironmentId("acme/backend/staging".to_string()))
        .unwrap();

    // --- Try delete env "production" -> rejected (protected) ---
    let err = store
        .delete_env(&EnvironmentId("acme/backend/production".to_string()))
        .unwrap_err();
    assert!(
        matches!(err, OrgStoreError::DeletionProtected(_)),
        "expected DeletionProtected, got: {err}"
    );

    // --- Unprotect "production", then delete -> OK ---
    store
        .set_deletion_protection(&EnvironmentId("acme/backend/production".to_string()), false)
        .unwrap();
    store
        .delete_env(&EnvironmentId("acme/backend/production".to_string()))
        .unwrap();

    // --- Delete env "dev" -> OK ---
    store
        .delete_env(&EnvironmentId("acme/frontend/dev".to_string()))
        .unwrap();

    // --- Delete project "backend" -> OK (no more envs) ---
    store
        .delete_project(&ProjectId("acme/backend".to_string()))
        .unwrap();

    // --- Delete project "frontend" -> OK ---
    store
        .delete_project(&ProjectId("acme/frontend".to_string()))
        .unwrap();

    // --- Delete org "acme" -> OK ---
    store.delete_org(&OrgId("acme".to_string())).unwrap();

    // --- Verify all tables empty ---
    assert_eq!(store.list_orgs().unwrap().len(), 0, "orgs should be empty");
    assert_eq!(
        store.list_projects().unwrap().len(),
        0,
        "projects should be empty"
    );
    assert_eq!(store.list_envs().unwrap().len(), 0, "envs should be empty");
}
