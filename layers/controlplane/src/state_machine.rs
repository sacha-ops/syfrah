//! Raft state machine backed by the existing OrgStore (redb).

use std::io;
use std::io::Cursor;
use std::sync::Arc;

use futures::TryStreamExt;
use openraft::alias::{LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf};
use openraft::storage::{EntryResponder, RaftSnapshotBuilder, RaftStateMachine};
use openraft::type_config::alias::SnapshotDataOf;
use openraft::{EntryPayload, OptionalSend};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::commands::{StateMachineCommand, StateMachineResponse};
use crate::types::SyfrahRaftConfig;

/// Snapshot of the state machine — serialized state for transfer.
#[derive(Debug)]
pub struct SmSnapshot {
    pub meta: SnapshotMetaOf<SyfrahRaftConfig>,
    pub data: Vec<u8>,
}

/// Internal state tracking for the Raft state machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SmState {
    pub last_applied_log: Option<LogIdOf<SyfrahRaftConfig>>,
    pub last_membership: StoredMembershipOf<SyfrahRaftConfig>,
}

/// Raft state machine that dispatches commands to the OrgStore.
///
/// On apply, each `StateMachineCommand` is deserialized and dispatched
/// to the appropriate store method. The OrgStore is backed by redb,
/// so the state machine output IS the local redb state.
pub struct RedbStateMachine {
    pub org_store: Arc<syfrah_org::OrgStore>,
    pub sm_state: RwLock<SmState>,
    pub current_snapshot: RwLock<Option<SmSnapshot>>,
    snapshot_idx: std::sync::Mutex<u64>,
}

impl RedbStateMachine {
    /// Create a new state machine wrapping the given OrgStore.
    pub fn new(org_store: Arc<syfrah_org::OrgStore>) -> Self {
        Self {
            org_store,
            sm_state: RwLock::new(SmState::default()),
            current_snapshot: RwLock::new(None),
            snapshot_idx: std::sync::Mutex::new(0),
        }
    }

    /// Apply a single command to the state machine.
    fn apply_command(&self, cmd: &StateMachineCommand) -> StateMachineResponse {
        match cmd {
            StateMachineCommand::CreateOrg { name } => match self.org_store.create(name) {
                Ok(org) => StateMachineResponse::Created(org.id.0),
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::DeleteOrg { name } => match self.org_store.delete(name) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::CreateProject { name, org } => {
                match self.org_store.create_project(org, name) {
                    Ok(proj) => StateMachineResponse::Created(proj.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteProject { name, org } => {
                match self.org_store.delete_project(org, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            // For Phase 1, remaining commands return Ok.
            // They will be wired to actual store methods in Phase 2.
            _ => {
                warn!("unimplemented state machine command: {cmd}");
                StateMachineResponse::Ok
            }
        }
    }
}

impl RaftSnapshotBuilder<SyfrahRaftConfig> for Arc<RedbStateMachine> {
    async fn build_snapshot(&mut self) -> Result<SnapshotOf<SyfrahRaftConfig>, io::Error> {
        let sm_state = self.sm_state.read().await;
        let data = serde_json::to_vec(&*sm_state)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let snapshot_idx = {
            let mut idx = self.snapshot_idx.lock().unwrap();
            *idx += 1;
            *idx
        };

        let snapshot_id = if let Some(last) = sm_state.last_applied_log {
            format!(
                "{}-{}-{}",
                last.committed_leader_id(),
                last.index(),
                snapshot_idx
            )
        } else {
            format!("--{}", snapshot_idx)
        };

        let meta = SnapshotMetaOf::<SyfrahRaftConfig> {
            last_log_id: sm_state.last_applied_log,
            last_membership: sm_state.last_membership.clone(),
            snapshot_id,
        };

        let snapshot = SmSnapshot {
            meta: meta.clone(),
            data: data.clone(),
        };

        {
            let mut current = self.current_snapshot.write().await;
            *current = Some(snapshot);
        }

        info!(snapshot_size = data.len(), "snapshot built");

        Ok(SnapshotOf::<SyfrahRaftConfig> {
            meta,
            snapshot: Cursor::new(data),
        })
    }
}

impl RaftStateMachine<SyfrahRaftConfig> for Arc<RedbStateMachine> {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<
        (
            Option<LogIdOf<SyfrahRaftConfig>>,
            StoredMembershipOf<SyfrahRaftConfig>,
        ),
        io::Error,
    > {
        let sm = self.sm_state.read().await;
        Ok((sm.last_applied_log, sm.last_membership.clone()))
    }

    async fn apply<Strm>(&mut self, mut entries: Strm) -> Result<(), io::Error>
    where
        Strm: futures::Stream<Item = Result<EntryResponder<SyfrahRaftConfig>, io::Error>>
            + Unpin
            + OptionalSend,
    {
        let mut sm = self.sm_state.write().await;

        while let Some((entry, responder)) = entries.try_next().await? {
            sm.last_applied_log = Some(entry.log_id);

            let response = match entry.payload {
                EntryPayload::Blank => StateMachineResponse::Ok,
                EntryPayload::Normal(ref cmd) => self.apply_command(cmd),
                EntryPayload::Membership(ref mem) => {
                    sm.last_membership = StoredMembershipOf::<SyfrahRaftConfig>::new(
                        Some(entry.log_id),
                        mem.clone(),
                    );
                    StateMachineResponse::Ok
                }
            };

            if let Some(responder) = responder {
                responder.send(response);
            }
        }
        Ok(())
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<SnapshotDataOf<SyfrahRaftConfig>, io::Error> {
        Ok(Cursor::new(Vec::new()))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMetaOf<SyfrahRaftConfig>,
        snapshot: SnapshotDataOf<SyfrahRaftConfig>,
    ) -> Result<(), io::Error> {
        let data = snapshot.into_inner();
        let new_sm: SmState = serde_json::from_slice(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        {
            let mut sm = self.sm_state.write().await;
            *sm = new_sm;
        }

        let snap = SmSnapshot {
            meta: meta.clone(),
            data,
        };
        let mut current = self.current_snapshot.write().await;
        *current = Some(snap);

        info!("snapshot installed");
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<SnapshotOf<SyfrahRaftConfig>>, io::Error> {
        match &*self.current_snapshot.read().await {
            Some(snapshot) => Ok(Some(SnapshotOf::<SyfrahRaftConfig> {
                meta: snapshot.meta.clone(),
                snapshot: Cursor::new(snapshot.data.clone()),
            })),
            None => Ok(None),
        }
    }
}
