use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::store::OrgStore;
use crate::ttl::check_expired_envs;

/// Default interval between TTL enforcement sweeps.
const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Runs a periodic loop that checks for expired environments and destroys them.
///
/// The loop runs every `interval` (default 60s). For each expired environment:
/// 1. Log the expiration
/// 2. Delete the environment from the store
/// 3. (Future: destroy associated resources — VMs, subnets, VPCs)
///
/// Stops when `shutdown` signal is received.
pub async fn run_ttl_enforcement(
    store: Arc<OrgStore>,
    mut shutdown: watch::Receiver<bool>,
    interval: Option<Duration>,
) {
    let interval = interval.unwrap_or(DEFAULT_SWEEP_INTERVAL);
    let mut ticker = tokio::time::interval(interval);
    // Skip the first immediate tick
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                sweep_expired(&store);
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("TTL enforcement loop shutting down");
                    return;
                }
            }
        }
    }
}

/// Single sweep: find and destroy all expired environments.
fn sweep_expired(store: &OrgStore) {
    let expired = match check_expired_envs(store) {
        Ok(envs) => envs,
        Err(e) => {
            error!("TTL sweep failed to list environments: {e}");
            return;
        }
    };

    if expired.is_empty() {
        return;
    }

    info!(
        "TTL sweep: {} expired environment(s) detected",
        expired.len()
    );

    for env in &expired {
        if env.deletion_protection {
            warn!(
                "Environment '{}' (org={}, project={}) has expired but deletion_protection is on — skipping",
                env.name, env.org_id, env.project_id
            );
            continue;
        }

        // TODO: destroy associated resources (VMs, subnets, VPCs) before deleting
        match store.delete_env(&env.org_id, &env.project_id, &env.name) {
            Ok(true) => {
                info!(
                    "Destroyed expired environment '{}' (org={}, project={})",
                    env.name, env.org_id, env.project_id
                );
            }
            Ok(false) => {
                warn!(
                    "Environment '{}' already removed before TTL sweep could delete it",
                    env.name
                );
            }
            Err(e) => {
                error!("Failed to destroy expired environment '{}': {e}", env.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{now_epoch, Environment};
    use std::collections::HashMap;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-daemon.redb");
        let store = OrgStore::open_at(&path).unwrap();
        (dir, store)
    }

    #[test]
    fn sweep_removes_expired_envs() {
        let (_dir, store) = temp_store();
        let past = now_epoch() - 100;
        let env = Environment {
            id: "acme/backend/ci-run".into(),
            name: "ci-run".into(),
            project_id: "backend".into(),
            org_id: "acme".into(),
            ttl_secs: Some(3600),
            expires_at: Some(past),
            deletion_protection: false,
            labels: HashMap::new(),
            created_at: 1000,
        };
        store.create_env(&env).unwrap();

        sweep_expired(&store);

        assert!(store
            .get_env("acme", "backend", "ci-run")
            .unwrap()
            .is_none());
    }

    #[test]
    fn sweep_skips_deletion_protected() {
        let (_dir, store) = temp_store();
        let past = now_epoch() - 100;
        let env = Environment {
            id: "acme/backend/prod".into(),
            name: "prod".into(),
            project_id: "backend".into(),
            org_id: "acme".into(),
            ttl_secs: Some(3600),
            expires_at: Some(past),
            deletion_protection: true,
            labels: HashMap::new(),
            created_at: 1000,
        };
        store.create_env(&env).unwrap();

        sweep_expired(&store);

        // Should still exist because deletion_protection is on
        assert!(store.get_env("acme", "backend", "prod").unwrap().is_some());
    }
}
