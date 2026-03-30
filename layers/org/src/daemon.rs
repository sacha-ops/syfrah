//! TTL enforcement daemon — periodically sweeps for expired environments.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::store::OrgStore;
use crate::ttl::find_expired_envs;

/// Default interval between TTL enforcement sweeps.
const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Runs a periodic loop that checks for expired environments and destroys them.
///
/// The loop runs every `interval` (default 60s). For each expired environment:
/// 1. Log the expiration
/// 2. Delete the environment from the store (respecting deletion protection)
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
    let expired = match find_expired_envs(store) {
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

    for (org, project, env) in &expired {
        if env.deletion_protection {
            warn!(
                "Environment '{}' (org={org}, project={project}) has expired but deletion_protection is on — skipping",
                env.name
            );
            continue;
        }

        match store.delete_env(org, project, &env.name) {
            Ok(()) => {
                info!(
                    "Destroyed expired environment '{}' (org={org}, project={project})",
                    env.name
                );
            }
            Err(e) => {
                error!("Failed to destroy expired environment '{}': {e}", env.name);
            }
        }
    }
}
