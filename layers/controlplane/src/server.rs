//! Raft HTTP server — Axum routes for Raft RPCs on port 7200.

use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::types::SyfrahRaftConfig;
use crate::SyfrahRaft;

/// Default maximum number of voters in the cluster.
/// Nodes beyond this limit join as learners automatically.
pub const DEFAULT_MAX_VOTERS: u32 = 5;

/// Shared state for the Raft HTTP server.
#[derive(Clone)]
pub struct RaftServerState {
    pub raft: SyfrahRaft,
    /// Maximum number of voters. Nodes beyond this join as learners.
    pub max_voters: u32,
}

/// The Raft HTTP server handling inter-node RPCs.
pub struct RaftServer;

impl RaftServer {
    /// Start the Raft HTTP server on the given address.
    pub async fn serve(
        bind_addr: SocketAddr,
        state: RaftServerState,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let app = raft_router(Arc::new(state));

        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        info!("raft HTTP server listening on {bind_addr}");

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.wait_for(|v| *v).await;
            })
            .await?;

        Ok(())
    }
}

/// Build the Axum router for Raft RPCs.
pub fn raft_router(state: Arc<RaftServerState>) -> Router {
    Router::new()
        .route("/raft/append_entries", post(append_entries_handler))
        .route("/raft/vote", post(vote_handler))
        .route("/raft/install_snapshot", post(install_snapshot_handler))
        .route("/raft/write", post(client_write_handler))
        .route("/raft/join", post(join_handler))
        .route("/raft/promote", post(promote_handler))
        .route("/raft/demote", post(demote_handler))
        .route("/raft/status", get(status_handler))
        .route("/raft/members", get(members_handler))
        .with_state(state)
}

/// Handle a forwarded client write from a follower node.
///
/// The leader applies the command via Raft and returns the state machine response.
async fn client_write_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(cmd): Json<crate::commands::StateMachineCommand>,
) -> Result<Json<crate::commands::StateMachineResponse>, StatusCode> {
    use tracing::debug;
    debug!("raft server: received forwarded write: {cmd}");

    let resp = state.raft.client_write(cmd).await.map_err(|e| {
        warn!("client_write error: {e:?}");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    Ok(Json(resp.response().clone()))
}

async fn append_entries_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(req): Json<AppendEntriesRequest<SyfrahRaftConfig>>,
) -> Result<Json<AppendEntriesResponse<SyfrahRaftConfig>>, StatusCode> {
    let resp = state
        .raft
        .append_entries(req)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(resp))
}

async fn vote_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(req): Json<VoteRequest<SyfrahRaftConfig>>,
) -> Result<Json<VoteResponse<SyfrahRaftConfig>>, StatusCode> {
    let resp = state
        .raft
        .vote(req)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct InstallSnapshotReq {
    vote: openraft::type_config::alias::VoteOf<SyfrahRaftConfig>,
    meta: openraft::alias::SnapshotMetaOf<SyfrahRaftConfig>,
    data: Vec<u8>,
}

async fn install_snapshot_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(req): Json<InstallSnapshotReq>,
) -> Result<Json<SnapshotResponse<SyfrahRaftConfig>>, StatusCode> {
    let snapshot = openraft::alias::SnapshotOf::<SyfrahRaftConfig> {
        meta: req.meta,
        snapshot: Cursor::new(req.data),
    };

    let resp = state
        .raft
        .install_full_snapshot(req.vote, snapshot)
        .await
        .map_err(|e| {
            warn!("install_snapshot error: {e:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(resp))
}

/// Request to join the Raft cluster as a learner.
#[derive(Deserialize)]
pub struct JoinRequest {
    pub node_id: u64,
    pub addr: String,
}

/// Request to promote a learner to voter.
#[derive(Deserialize)]
pub struct PromoteRequest {
    pub node_id: u64,
}

/// Handle a join request: add the node as a learner.
async fn join_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use crate::types::SyfrahNode;

    info!(
        "raft: join request from node {} at {}",
        req.node_id, req.addr
    );

    let node = SyfrahNode {
        addr: req.addr.clone(),
    };

    // Add as learner (non-blocking — replication starts immediately).
    state
        .raft
        .add_learner(req.node_id, node, false)
        .await
        .map_err(|e| {
            warn!("join (add_learner) failed: {e:?}");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    info!("raft: node {} added as learner", req.node_id);

    // Auto-promote to voter if under the max_voters limit.
    let current_voters = {
        use openraft::rt::watch::WatchReceiver;
        let metrics = state.raft.metrics().borrow_watched().clone();
        metrics.membership_config.membership().voter_ids().count() as u32
    };

    if current_voters < state.max_voters {
        use openraft::ChangeMembers;
        match state
            .raft
            .change_membership(
                ChangeMembers::AddVoterIds(std::collections::BTreeSet::from([req.node_id])),
                true,
            )
            .await
        {
            Ok(_) => {
                info!(
                    "raft: node {} auto-promoted to voter ({}/{} voters)",
                    req.node_id,
                    current_voters + 1,
                    state.max_voters
                );
                return Ok(Json(serde_json::json!({
                    "status": "joined_as_voter",
                    "node_id": req.node_id,
                    "role": "voter"
                })));
            }
            Err(e) => {
                warn!("raft: auto-promote failed (node stays as learner): {e:?}");
            }
        }
    } else {
        info!(
            "raft: voter limit reached ({}/{}), node {} stays as learner",
            current_voters, state.max_voters, req.node_id
        );
    }

    Ok(Json(serde_json::json!({
        "status": "joined",
        "node_id": req.node_id,
        "role": "learner"
    })))
}

/// Handle a promote request: promote learner to voter.
async fn promote_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(req): Json<PromoteRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use openraft::ChangeMembers;

    info!("raft: promote request for node {}", req.node_id);

    // Promote the learner to voter (retain=true keeps existing learners).
    state
        .raft
        .change_membership(
            ChangeMembers::AddVoterIds(std::collections::BTreeSet::from([req.node_id])),
            true,
        )
        .await
        .map_err(|e| {
            warn!("promote failed: {e:?}");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    info!("raft: node {} promoted to voter", req.node_id);
    Ok(Json(
        serde_json::json!({ "status": "promoted", "node_id": req.node_id }),
    ))
}

/// Request to demote a voter to learner.
#[derive(Deserialize)]
pub struct DemoteRequest {
    pub node_id: u64,
}

/// Handle a demote request: demote voter to learner.
///
/// Removes the node from the voter set. The node remains as a learner
/// and continues to receive replicated log entries.
async fn demote_handler(
    State(state): State<Arc<RaftServerState>>,
    Json(req): Json<DemoteRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use openraft::ChangeMembers;

    info!("raft: demote request for node {}", req.node_id);

    // Remove from voter set (retain=true keeps the node as a learner).
    state
        .raft
        .change_membership(
            ChangeMembers::RemoveVoters(std::collections::BTreeSet::from([req.node_id])),
            true,
        )
        .await
        .map_err(|e| {
            warn!("demote failed: {e:?}");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    info!("raft: node {} demoted to learner", req.node_id);
    Ok(Json(
        serde_json::json!({ "status": "demoted", "node_id": req.node_id }),
    ))
}

/// Member info in the members response.
#[derive(Serialize, Deserialize)]
pub struct MemberInfo {
    pub node_id: u64,
    pub addr: String,
    pub role: String, // "voter" or "learner"
}

/// Members response — list all Raft members with roles.
#[derive(Serialize, Deserialize)]
pub struct MembersResponse {
    pub members: Vec<MemberInfo>,
}

async fn members_handler(
    State(state): State<Arc<RaftServerState>>,
) -> Result<Json<MembersResponse>, StatusCode> {
    use openraft::rt::watch::WatchReceiver;
    let metrics = state.raft.metrics().borrow_watched().clone();

    let voter_ids: std::collections::HashSet<u64> =
        metrics.membership_config.membership().voter_ids().collect();

    let members: Vec<MemberInfo> = metrics
        .membership_config
        .membership()
        .nodes()
        .map(|(id, node)| MemberInfo {
            node_id: *id,
            addr: node.addr.clone(),
            role: if voter_ids.contains(id) {
                "voter".to_string()
            } else {
                "learner".to_string()
            },
        })
        .collect();

    Ok(Json(MembersResponse { members }))
}

/// Status response for the Raft cluster.
#[derive(Serialize, Deserialize)]
pub struct RaftStatusResponse {
    pub id: u64,
    pub state: String,
    pub current_leader: Option<u64>,
    pub current_term: u64,
    pub last_log_index: Option<u64>,
    pub last_applied_index: Option<u64>,
    pub members: Vec<u64>,
    /// Enhanced member details with roles and addresses.
    #[serde(default)]
    pub member_details: Vec<ClusterMemberDetail>,
    /// Commit index (same as last_applied_index for leader).
    #[serde(default)]
    pub commit_index: Option<u64>,
    /// Total number of log entries.
    #[serde(default)]
    pub log_entries: Option<u64>,
    /// Number of voters.
    #[serde(default)]
    pub voter_count: u32,
    /// Number of learners.
    #[serde(default)]
    pub learner_count: u32,
}

/// Detailed cluster member information.
#[derive(Serialize, Deserialize, Clone)]
pub struct ClusterMemberDetail {
    pub node_id: u64,
    pub addr: String,
    pub role: String, // "voter" or "learner"
    pub is_leader: bool,
}

async fn status_handler(
    State(state): State<Arc<RaftServerState>>,
) -> Result<Json<RaftStatusResponse>, StatusCode> {
    use openraft::rt::watch::WatchReceiver;
    let metrics = state.raft.metrics().borrow_watched().clone();

    let voter_ids: std::collections::HashSet<u64> =
        metrics.membership_config.membership().voter_ids().collect();
    let members: Vec<u64> = voter_ids.iter().copied().collect();
    let leader_id = metrics.current_leader;

    let member_details: Vec<ClusterMemberDetail> = metrics
        .membership_config
        .membership()
        .nodes()
        .map(|(id, node)| {
            let role = if voter_ids.contains(id) {
                "voter"
            } else {
                "learner"
            };
            ClusterMemberDetail {
                node_id: *id,
                addr: node.addr.clone(),
                role: role.to_string(),
                is_leader: leader_id == Some(*id),
            }
        })
        .collect();

    let voter_count = voter_ids.len() as u32;
    let learner_count = (member_details.len() as u32).saturating_sub(voter_count);

    let resp = RaftStatusResponse {
        id: metrics.id,
        state: format!("{:?}", metrics.state),
        current_leader: metrics.current_leader,
        current_term: metrics.current_term,
        last_log_index: metrics.last_log_index,
        last_applied_index: metrics
            .last_applied
            .map(|l: openraft::alias::LogIdOf<SyfrahRaftConfig>| l.index()),
        members,
        member_details,
        commit_index: metrics
            .last_applied
            .map(|l: openraft::alias::LogIdOf<SyfrahRaftConfig>| l.index()),
        log_entries: metrics.last_log_index,
        voter_count,
        learner_count,
    };
    Ok(Json(resp))
}
