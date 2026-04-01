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

/// Shared state for the Raft HTTP server.
#[derive(Clone)]
pub struct RaftServerState {
    pub raft: SyfrahRaft,
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
        .route("/raft/status", get(status_handler))
        .with_state(state)
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

/// Status response for the Raft cluster.
#[derive(Serialize)]
pub struct RaftStatusResponse {
    pub id: u64,
    pub state: String,
    pub current_leader: Option<u64>,
    pub current_term: u64,
    pub last_log_index: Option<u64>,
    pub last_applied_index: Option<u64>,
    pub members: Vec<u64>,
}

async fn status_handler(
    State(state): State<Arc<RaftServerState>>,
) -> Result<Json<RaftStatusResponse>, StatusCode> {
    use openraft::rt::watch::WatchReceiver;
    let metrics = state.raft.metrics().borrow_watched().clone();

    let members: Vec<u64> = metrics.membership_config.membership().voter_ids().collect();

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
    };
    Ok(Json(resp))
}
