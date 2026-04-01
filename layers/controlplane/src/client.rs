//! Raft client — routes write commands through the Raft cluster.
//!
//! If this node is the leader, submits directly via `raft.client_write()`.
//! If this node is a follower, forwards to the leader's Raft HTTP endpoint.
//! If Raft is not initialized, returns `None` so the caller can fall back
//! to direct writes.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::commands::{StateMachineCommand, StateMachineResponse};
use crate::SyfrahRaft;

/// Plain Raft metrics snapshot for Prometheus export.
///
/// This avoids requiring downstream crates to depend on openraft types.
#[derive(Debug, Clone)]
pub struct RaftMetricsSnapshot {
    /// Raft state: 0=follower, 1=candidate, 2=leader.
    pub state: u8,
    /// Current Raft term.
    pub term: u64,
    /// Commit index.
    pub commit_index: u64,
    /// Last applied log index.
    pub last_applied: u64,
    /// Current number of log entries.
    pub log_entries: u64,
    /// Number of snapshots (1 if snapshot exists, 0 otherwise).
    pub snapshot_count: u64,
}

/// A handle for submitting commands to the Raft cluster.
///
/// Cloneable and cheaply shareable across threads.
#[derive(Clone)]
pub struct RaftClient {
    raft: Arc<SyfrahRaft>,
    http_client: reqwest::Client,
}

impl RaftClient {
    /// Create a new Raft client wrapping the given Raft node.
    pub fn new(raft: SyfrahRaft) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build reqwest client");
        Self {
            raft: Arc::new(raft),
            http_client,
        }
    }

    /// Submit a command to the Raft cluster.
    ///
    /// - If this node is the leader: applies directly.
    /// - If this node is a follower: forwards to the leader.
    ///
    /// Returns the state machine response, or an error string.
    pub async fn write(&self, cmd: StateMachineCommand) -> Result<StateMachineResponse, String> {
        debug!("raft client: submitting {cmd}");

        // Try direct write first (works if we are the leader).
        match self.raft.client_write(cmd.clone()).await {
            Ok(resp) => {
                let sm_resp = resp.response().clone();
                debug!("raft client: command applied locally (leader)");
                Ok(sm_resp)
            }
            Err(e) => {
                // Check if the error indicates we are not the leader.
                let err_str = format!("{e:?}");
                if err_str.contains("ForwardToLeader") {
                    // Extract leader info and forward.
                    if let Some((leader_id, leader_addr)) = self.find_leader().await {
                        debug!("raft client: forwarding to leader {leader_id} at {leader_addr}");
                        self.forward_to_leader(&leader_addr, &cmd).await
                    } else {
                        Err("no leader available — cluster may be electing".to_string())
                    }
                } else {
                    Err(format!("raft write error: {e}"))
                }
            }
        }
    }

    /// Find the current Raft leader's address.
    async fn find_leader(&self) -> Option<(u64, String)> {
        use openraft::rt::watch::WatchReceiver;
        let metrics = self.raft.metrics().borrow_watched().clone();
        let leader_id = metrics.current_leader?;

        // Look up the leader's address from the membership config.
        let membership = &metrics.membership_config;
        for (node_id, node) in membership.membership().nodes() {
            if *node_id == leader_id {
                return Some((leader_id, node.addr.clone()));
            }
        }

        warn!("raft client: leader {leader_id} not found in membership");
        None
    }

    /// Forward a command to the leader via its Raft HTTP endpoint.
    async fn forward_to_leader(
        &self,
        leader_addr: &str,
        cmd: &StateMachineCommand,
    ) -> Result<StateMachineResponse, String> {
        let url = format!("http://{leader_addr}/raft/write");

        let resp = self
            .http_client
            .post(&url)
            .json(cmd)
            .send()
            .await
            .map_err(|e| format!("forward to leader failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("leader returned {status}: {body}"));
        }

        let sm_resp: StateMachineResponse = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse leader response: {e}"))?;

        info!("raft client: command forwarded to leader successfully");
        Ok(sm_resp)
    }

    /// Get a reference to the underlying Raft node.
    pub fn raft(&self) -> &SyfrahRaft {
        &self.raft
    }

    /// Get a snapshot of Raft metrics for Prometheus export.
    pub fn metrics_snapshot(&self) -> RaftMetricsSnapshot {
        use openraft::rt::watch::WatchReceiver;
        let metrics = self.raft.metrics().borrow_watched().clone();

        let state = match format!("{:?}", metrics.state).as_str() {
            "Follower" => 0,
            "Candidate" => 1,
            "Leader" => 2,
            _ => 0,
        };

        let last_applied = metrics
            .last_applied
            .map(|l: openraft::alias::LogIdOf<crate::types::SyfrahRaftConfig>| l.index())
            .unwrap_or(0);

        RaftMetricsSnapshot {
            state,
            term: metrics.current_term,
            commit_index: last_applied,
            last_applied,
            log_entries: metrics.last_log_index.unwrap_or(0),
            snapshot_count: if metrics.snapshot.is_some() { 1 } else { 0 },
        }
    }

    /// Check if this node is the Raft leader.
    pub fn is_leader(&self) -> bool {
        use openraft::rt::watch::WatchReceiver;
        let metrics = self.raft.metrics().borrow_watched().clone();
        metrics.current_leader == Some(metrics.id)
    }

    /// Get the leader's address from the membership config.
    /// Returns `None` if no leader is known.
    pub fn leader_addr(&self) -> Option<String> {
        use openraft::rt::watch::WatchReceiver;
        let metrics = self.raft.metrics().borrow_watched().clone();
        let leader_id = metrics.current_leader?;

        for (node_id, node) in metrics.membership_config.membership().nodes() {
            if *node_id == leader_id {
                return Some(node.addr.clone());
            }
        }
        None
    }
}
