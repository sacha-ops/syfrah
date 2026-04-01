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
    pub ipam_store: Option<Arc<syfrah_org::IpamStore>>,
    pub placement_store: Option<Arc<syfrah_org::PlacementStore>>,
    pub sm_state: RwLock<SmState>,
    pub current_snapshot: RwLock<Option<SmSnapshot>>,
    snapshot_idx: std::sync::Mutex<u64>,
}

impl RedbStateMachine {
    /// Create a new state machine wrapping the given OrgStore.
    pub fn new(org_store: Arc<syfrah_org::OrgStore>) -> Self {
        Self {
            org_store,
            ipam_store: None,
            placement_store: None,
            sm_state: RwLock::new(SmState::default()),
            current_snapshot: RwLock::new(None),
            snapshot_idx: std::sync::Mutex::new(0),
        }
    }

    /// Set the IPAM store for distributed IP allocation.
    pub fn with_ipam_store(mut self, store: Arc<syfrah_org::IpamStore>) -> Self {
        self.ipam_store = Some(store);
        self
    }

    /// Set the placement store for VM placement tracking.
    pub fn with_placement_store(mut self, store: Arc<syfrah_org::PlacementStore>) -> Self {
        self.placement_store = Some(store);
        self
    }

    /// Apply a single command to the state machine.
    ///
    /// Every mutation goes through here on ALL Raft nodes, producing identical redb state.
    pub fn apply_command(&self, cmd: &StateMachineCommand) -> StateMachineResponse {
        match cmd {
            // -- Org --
            StateMachineCommand::CreateOrg { name } => match self.org_store.create(name) {
                Ok(org) => StateMachineResponse::Created(org.id.0),
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::DeleteOrg { name } => match self.org_store.delete(name) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },

            // -- Project --
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

            // -- Environment --
            StateMachineCommand::CreateEnv {
                name,
                project,
                org,
                ttl,
                deletion_protection,
                labels,
            } => {
                match self.org_store.create_env(
                    org,
                    project,
                    name,
                    *ttl,
                    *deletion_protection,
                    labels.clone(),
                ) {
                    Ok(env) => StateMachineResponse::Created(env.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteEnv { name, project, org } => {
                match self.org_store.delete_env(org, project, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- VPC --
            StateMachineCommand::CreateVpc {
                name,
                cidr,
                owner,
                shared,
            } => {
                use syfrah_org::types::{OrgId, VpcOwner};
                // Reconstruct VpcOwner from the string representation.
                let vpc_owner = VpcOwner::Org(OrgId(owner.clone()));
                match self.org_store.create_vpc(name, cidr, vpc_owner, *shared) {
                    Ok(vpc) => StateMachineResponse::Created(vpc.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteVpc { name } => match self.org_store.delete_vpc(name) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::PeerVpc { vpc_a, vpc_b } => {
                match self.org_store.create_peering(vpc_a, vpc_b) {
                    Ok(_) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UnpeerVpc { vpc_a, vpc_b } => {
                match self.org_store.delete_peering(vpc_a, vpc_b) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Subnet --
            StateMachineCommand::CreateSubnet {
                name,
                vpc,
                env_id,
                cidr,
            } => {
                use syfrah_org::types::EnvironmentId;
                let eid = EnvironmentId(env_id.clone());
                match self
                    .org_store
                    .create_subnet(vpc, &eid, name, cidr.as_deref())
                {
                    Ok(subnet) => StateMachineResponse::Created(subnet.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteSubnet { name, vpc } => {
                match self.org_store.delete_subnet(vpc, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Security Groups --
            StateMachineCommand::CreateSg { name, vpc } => {
                match self.org_store.create_security_group(name, vpc, "") {
                    Ok(sg) => StateMachineResponse::Created(sg.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteSg { name } => {
                match self.org_store.delete_security_group(name, None) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::AddSgRule { .. } => {
                // SG rules are handled by the SgRuleStore, not OrgStore.
                // For now, return Ok — SG rule store integration will be added.
                warn!("SG rule commands not yet wired to SgRuleStore in state machine");
                StateMachineResponse::Ok
            }
            StateMachineCommand::RemoveSgRule { .. } => {
                warn!("SG rule commands not yet wired to SgRuleStore in state machine");
                StateMachineResponse::Ok
            }
            StateMachineCommand::AttachSg { sg, nic_id } => {
                match self.org_store.attach_sg_to_nic(sg, nic_id) {
                    Ok(_) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DetachSg { sg, nic_id } => {
                match self.org_store.detach_sg_from_nic(sg, nic_id) {
                    Ok(_) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- NAT Gateway --
            StateMachineCommand::CreateNatGw { .. } => {
                // NAT GW creation requires VPC ID resolution, which is complex.
                // For now, log and return Ok.
                warn!("NAT GW create not yet fully wired in state machine");
                StateMachineResponse::Ok
            }
            StateMachineCommand::DeleteNatGw { .. } => {
                warn!("NAT GW delete not yet fully wired in state machine");
                StateMachineResponse::Ok
            }

            // -- Routes --
            StateMachineCommand::AddRoute { .. } => {
                warn!("Route add not yet fully wired in state machine");
                StateMachineResponse::Ok
            }
            StateMachineCommand::DeleteRoute { .. } => {
                warn!("Route delete not yet fully wired in state machine");
                StateMachineResponse::Ok
            }

            // -- IPAM --
            StateMachineCommand::AllocateIp { subnet_id } => {
                let ipam = match &self.ipam_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "IPAM store not available in state machine".to_string(),
                        )
                    }
                };
                // Look up the subnet CIDR from the org store.
                let subnet = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "subnet not found: {subnet_id}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match ipam.reserve_ip(subnet_id, &subnet.cidr) {
                    Ok(alloc) => StateMachineResponse::AllocatedIp {
                        ip: alloc.ip,
                        mac: alloc.mac,
                    },
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::ReleaseIp { subnet_id, ip } => {
                let ipam = match &self.ipam_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "IPAM store not available in state machine".to_string(),
                        )
                    }
                };
                let subnet = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "subnet not found: {subnet_id}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match ipam.release_ip(subnet_id, &subnet.cidr, ip) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- NIC --
            StateMachineCommand::CreateNic { .. } => {
                warn!("NIC commands will be wired in later issues");
                StateMachineResponse::Ok
            }
            StateMachineCommand::DeleteNic { nic_id } => match self.org_store.delete_nic(nic_id) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },

            // -- Hypervisor --
            StateMachineCommand::RegisterHypervisor { .. } => {
                warn!("Hypervisor commands use separate HypervisorStore — pass-through");
                StateMachineResponse::Ok
            }
            StateMachineCommand::EnableHypervisor { .. }
            | StateMachineCommand::DrainHypervisor { .. }
            | StateMachineCommand::DecommissionHypervisor { .. } => {
                warn!("Hypervisor state commands use separate HypervisorStore — pass-through");
                StateMachineResponse::Ok
            }
            StateMachineCommand::UpdateHypervisorLabels { .. }
            | StateMachineCommand::UpdateHypervisorTaints { .. } => {
                warn!("Hypervisor metadata commands use separate HypervisorStore — pass-through");
                StateMachineResponse::Ok
            }

            // -- VM Placement --
            StateMachineCommand::PlaceVm { .. } => {
                warn!("PlaceVm will be wired in issue #1049");
                StateMachineResponse::Ok
            }
            StateMachineCommand::RemoveVm { .. } => {
                warn!("RemoveVm will be wired in issue #1049");
                StateMachineResponse::Ok
            }
            StateMachineCommand::RescheduleVm { .. } => {
                warn!("RescheduleVm will be wired in later issues");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_org_store() -> (tempfile::TempDir, Arc<syfrah_org::OrgStore>) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_org.redb");
        let db = syfrah_state::LayerDb::open_at(&db_path).unwrap();
        (dir, Arc::new(syfrah_org::OrgStore::new(db)))
    }

    #[test]
    fn apply_create_org() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let cmd = StateMachineCommand::CreateOrg {
            name: "testorg".to_string(),
        };
        let resp = sm.apply_command(&cmd);
        match resp {
            StateMachineResponse::Created(id) => assert!(id.contains("testorg")),
            other => panic!("expected Created, got {other:?}"),
        }
        // Verify it was actually created.
        let org = store.get("testorg").unwrap();
        assert!(org.is_some());
    }

    #[test]
    fn apply_create_org_duplicate() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let cmd = StateMachineCommand::CreateOrg {
            name: "dup".to_string(),
        };
        let _ = sm.apply_command(&cmd);
        let resp = sm.apply_command(&cmd);
        match resp {
            StateMachineResponse::Error(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn apply_delete_org() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let _ = sm.apply_command(&StateMachineCommand::CreateOrg {
            name: "del".to_string(),
        });
        let resp = sm.apply_command(&StateMachineCommand::DeleteOrg {
            name: "del".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        assert!(store.get("del").unwrap().is_none());
    }

    #[test]
    fn apply_create_project() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store.clone());
        let _ = sm.apply_command(&StateMachineCommand::CreateOrg {
            name: "acme".to_string(),
        });
        let resp = sm.apply_command(&StateMachineCommand::CreateProject {
            name: "backend".to_string(),
            org: "acme".to_string(),
        });
        match resp {
            StateMachineResponse::Created(id) => assert!(id.contains("backend")),
            other => panic!("expected Created, got {other:?}"),
        }
    }

    #[test]
    fn apply_allocate_ip_without_ipam_store_returns_error() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        let resp = sm.apply_command(&StateMachineCommand::AllocateIp {
            subnet_id: "sub-1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
    }

    #[test]
    fn apply_passthrough_commands_return_ok() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        // Hypervisor commands use a separate store and are pass-through.
        let resp = sm.apply_command(&StateMachineCommand::RegisterHypervisor {
            name: "hv1".to_string(),
            region: "eu".to_string(),
            zone: "az1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
    }

    #[tokio::test]
    async fn snapshot_roundtrip() {
        let (_dir, store) = make_org_store();
        let mut sm = Arc::new(RedbStateMachine::new(store));

        // Build a snapshot.
        use openraft::storage::RaftSnapshotBuilder;
        let snap = sm.build_snapshot().await.unwrap();
        assert!(!snap.snapshot.into_inner().is_empty());

        // Get current snapshot.
        use openraft::storage::RaftStateMachine;
        let current = sm.get_current_snapshot().await.unwrap();
        assert!(current.is_some());
    }

    #[tokio::test]
    async fn applied_state_default() {
        let (_dir, store) = make_org_store();
        let mut sm = Arc::new(RedbStateMachine::new(store));
        use openraft::storage::RaftStateMachine;
        let (last, membership) = sm.applied_state().await.unwrap();
        assert!(last.is_none());
        // Default membership should be empty.
        assert_eq!(membership.membership().voter_ids().count(), 0);
    }
}
