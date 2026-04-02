//! Raft state machine backed by the existing OrgStore (redb).
//!
//! Snapshots serialize the full redb state (all org/ipam/placement tables)
//! so new members joining the cluster get the complete state without
//! replaying the entire Raft log.

use std::collections::HashMap;
use std::io;
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Default number of log entries between snapshots.
pub const DEFAULT_SNAPSHOT_THRESHOLD: u64 = 10_000;

/// Event emitted by the state machine when a VM placement changes.
///
/// The daemon subscribes to these events to update FDB + ARP proxy
/// entries incrementally (O(1) per placement change).
#[derive(Debug, Clone)]
pub enum PlacementEvent {
    /// A VM was placed on a hypervisor.
    Added {
        vpc_id: String,
        vm_id: String,
        vm_mac: String,
        vm_ip: String,
        subnet_id: String,
        hypervisor_id: String,
    },
    /// A VM was removed from a hypervisor.
    Removed {
        vpc_id: String,
        vm_id: String,
        vm_mac: String,
        vm_ip: String,
        hypervisor_id: String,
    },
}

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

/// Full snapshot data including store tables for new member catch-up.
///
/// When serialized, this contains the complete redb state so a joining
/// node can restore without replaying the entire Raft log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FullSnapshotData {
    /// Raft metadata (last applied log, membership).
    pub sm_state: SmState,
    /// Raw table data: table_name -> Vec<(key, json_bytes)>.
    /// Uses base64-encoded bytes for JSON compatibility.
    pub tables: HashMap<String, Vec<(String, Vec<u8>)>>,
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
    pub sg_rule_store: Option<Arc<syfrah_org::SgRuleStore>>,
    pub hypervisor_store: Option<Arc<syfrah_org::HypervisorStore>>,
    pub sm_state: RwLock<SmState>,
    pub current_snapshot: RwLock<Option<SmSnapshot>>,
    snapshot_idx: std::sync::Mutex<u64>,
    /// Broadcast channel for placement events (FDB incremental updates).
    /// Subscribers receive events when PlaceVm/RemoveVm commands are applied.
    placement_tx: tokio::sync::broadcast::Sender<PlacementEvent>,
    /// Counter for total snapshots built (for metrics).
    pub snapshot_count: AtomicU64,
    /// Counter tracking entries applied since last snapshot.
    pub entries_since_snapshot: AtomicU64,
    /// Configurable snapshot threshold (default: 10,000 log entries).
    pub snapshot_threshold: u64,
}

impl RedbStateMachine {
    /// Create a new state machine wrapping the given OrgStore.
    pub fn new(org_store: Arc<syfrah_org::OrgStore>) -> Self {
        let (placement_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            org_store,
            ipam_store: None,
            placement_store: None,
            sg_rule_store: None,
            hypervisor_store: None,
            sm_state: RwLock::new(SmState::default()),
            current_snapshot: RwLock::new(None),
            snapshot_idx: std::sync::Mutex::new(0),
            placement_tx,
            snapshot_count: AtomicU64::new(0),
            entries_since_snapshot: AtomicU64::new(0),
            snapshot_threshold: DEFAULT_SNAPSHOT_THRESHOLD,
        }
    }

    /// Create a new state machine with a custom snapshot threshold.
    pub fn with_snapshot_threshold(mut self, threshold: u64) -> Self {
        self.snapshot_threshold = threshold;
        self
    }

    /// Export all store tables as raw data for snapshot serialization.
    ///
    /// Collects data from org, IPAM, and placement stores into a
    /// HashMap of table_name -> entries.
    fn export_store_tables(&self) -> HashMap<String, Vec<(String, Vec<u8>)>> {
        let mut tables = HashMap::new();

        // Export org store tables.
        for table_name in syfrah_org::OrgStore::table_names() {
            match self.org_store.db().export_table_raw(table_name) {
                Ok(entries) if !entries.is_empty() => {
                    tables.insert(table_name.to_string(), entries);
                }
                Ok(_) => {} // empty table, skip
                Err(e) => {
                    warn!("snapshot: failed to export org table {table_name}: {e}");
                }
            }
        }

        // Export IPAM store tables.
        if let Some(ref ipam) = self.ipam_store {
            for table_name in syfrah_org::IpamStore::table_names() {
                match ipam.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export IPAM table {table_name}: {e}");
                    }
                }
            }
        }

        // Export placement store tables.
        if let Some(ref placement) = self.placement_store {
            for table_name in syfrah_org::PlacementStore::table_names() {
                match placement.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export placement table {table_name}: {e}");
                    }
                }
            }
        }

        // Export SG rule store tables.
        if let Some(ref sg_rules) = self.sg_rule_store {
            for table_name in syfrah_org::SgRuleStore::table_names() {
                match sg_rules.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export SG rule table {table_name}: {e}");
                    }
                }
            }
        }

        // Export hypervisor store tables.
        if let Some(ref hv_store) = self.hypervisor_store {
            for table_name in syfrah_org::HypervisorStore::table_names() {
                match hv_store.db().export_table_raw(table_name) {
                    Ok(entries) if !entries.is_empty() => {
                        tables.insert(table_name.to_string(), entries);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("snapshot: failed to export hypervisor table {table_name}: {e}");
                    }
                }
            }
        }

        tables
    }

    /// Import store tables from snapshot data, replacing existing state.
    fn import_store_tables(&self, tables: &HashMap<String, Vec<(String, Vec<u8>)>>) {
        // Import org store tables.
        for table_name in syfrah_org::OrgStore::table_names() {
            if let Some(entries) = tables.get(*table_name) {
                if let Err(e) = self.org_store.db().import_table_raw(table_name, entries) {
                    warn!("snapshot: failed to import org table {table_name}: {e}");
                }
            }
        }

        // Import IPAM store tables.
        if let Some(ref ipam) = self.ipam_store {
            for table_name in syfrah_org::IpamStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = ipam.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import IPAM table {table_name}: {e}");
                    }
                }
            }
        }

        // Import placement store tables.
        if let Some(ref placement) = self.placement_store {
            for table_name in syfrah_org::PlacementStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = placement.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import placement table {table_name}: {e}");
                    }
                }
            }
        }

        // Import SG rule store tables.
        if let Some(ref sg_rules) = self.sg_rule_store {
            for table_name in syfrah_org::SgRuleStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = sg_rules.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import SG rule table {table_name}: {e}");
                    }
                }
            }
        }

        // Import hypervisor store tables.
        if let Some(ref hv_store) = self.hypervisor_store {
            for table_name in syfrah_org::HypervisorStore::table_names() {
                if let Some(entries) = tables.get(*table_name) {
                    if let Err(e) = hv_store.db().import_table_raw(table_name, entries) {
                        warn!("snapshot: failed to import hypervisor table {table_name}: {e}");
                    }
                }
            }
        }
    }

    /// Subscribe to placement events for incremental FDB updates.
    ///
    /// Returns a broadcast receiver that yields `PlacementEvent`s whenever
    /// the state machine applies a PlaceVm or RemoveVm command.
    pub fn subscribe_placement_events(&self) -> tokio::sync::broadcast::Receiver<PlacementEvent> {
        self.placement_tx.subscribe()
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

    /// Set the SG rule store for security group rule replication.
    pub fn with_sg_rule_store(mut self, store: Arc<syfrah_org::SgRuleStore>) -> Self {
        self.sg_rule_store = Some(store);
        self
    }

    /// Set the hypervisor store for Raft-replicated hypervisor registration.
    pub fn with_hypervisor_store(mut self, store: Arc<syfrah_org::HypervisorStore>) -> Self {
        self.hypervisor_store = Some(store);
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
                use syfrah_org::types::{OrgId, ProjectId, VpcOwner};
                // Reconstruct VpcOwner from the string representation.
                // If owner contains '/', it's a project (org/project). Otherwise, it's an org.
                let vpc_owner = if owner.contains('/') {
                    VpcOwner::Project(ProjectId(owner.clone()))
                } else {
                    VpcOwner::Org(OrgId(owner.clone()))
                };
                match self.org_store.create_vpc(name, cidr, vpc_owner, *shared) {
                    Ok(vpc) => StateMachineResponse::Created(vpc.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteVpc { name } => match self.org_store.delete_vpc(name) {
                Ok(()) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::AttachVpc { vpc, project } => {
                match self.org_store.attach_vpc(vpc, project) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DetachVpc { vpc, project } => {
                match self.org_store.detach_vpc(vpc, project) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
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

            // -- Environment mutations --
            StateMachineCommand::ExtendEnv {
                name,
                project,
                org,
                ttl_seconds,
            } => match self.org_store.extend_env(org, project, name, *ttl_seconds) {
                Ok(_env) => StateMachineResponse::Ok,
                Err(e) => StateMachineResponse::Error(e.to_string()),
            },
            StateMachineCommand::UpdateEnv {
                name,
                project,
                org,
                deletion_protection,
            } => {
                if let Some(dp) = deletion_protection {
                    match self
                        .org_store
                        .update_env_protection(org, project, name, *dp)
                    {
                        Ok(_env) => StateMachineResponse::Ok,
                        Err(e) => StateMachineResponse::Error(e.to_string()),
                    }
                } else {
                    StateMachineResponse::Error("no update specified".to_string())
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
                // Auto-create default VPC if it doesn't exist (deterministic on all nodes).
                if vpc.ends_with("-default") {
                    if let Ok(None) = self.org_store.get_vpc(vpc) {
                        // Extract org/project from the VPC name pattern: "{org}-{project}-default"
                        let parts: Vec<&str> = vpc
                            .strip_suffix("-default")
                            .unwrap_or(vpc)
                            .splitn(2, '-')
                            .collect();
                        if parts.len() == 2 {
                            use syfrah_org::types::{ProjectId, VpcOwner};
                            let org = parts[0];
                            let project = parts[1];
                            let owner = VpcOwner::Project(ProjectId(format!("{org}/{project}")));
                            if let Err(e) =
                                self.org_store.create_vpc(vpc, "10.1.0.0/16", owner, false)
                            {
                                return StateMachineResponse::Error(format!(
                                    "failed to auto-create default VPC '{vpc}': {e}"
                                ));
                            }
                        }
                    }
                }
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
            StateMachineCommand::AddSgRule {
                sg,
                direction,
                protocol,
                port,
                source,
            } => {
                let sg_rule_store = match &self.sg_rule_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "SG rule store not available in state machine".to_string(),
                        )
                    }
                };
                // Resolve SG record to get its ID.
                let sg_record = match self.org_store.find_sg_by_name(sg) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "security group not found: {sg}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Parse direction, protocol, port, source.
                use syfrah_org::types::{Direction, PortRange, Protocol, RuleId, RuleSource};
                let dir = match direction.as_str() {
                    "ingress" => Direction::Ingress,
                    "egress" => Direction::Egress,
                    other => {
                        return StateMachineResponse::Error(format!("invalid direction: '{other}'"))
                    }
                };
                let proto = match protocol.as_str() {
                    "tcp" => Protocol::Tcp,
                    "udp" => Protocol::Udp,
                    "icmp" => Protocol::Icmp,
                    "all" => Protocol::All,
                    other => {
                        return StateMachineResponse::Error(format!("invalid protocol: '{other}'"))
                    }
                };
                let port_range = port.as_ref().and_then(|p| {
                    if let Some((from, to)) = p.split_once('-') {
                        Some(PortRange {
                            from: from.parse().unwrap_or(0),
                            to: to.parse().unwrap_or(0),
                        })
                    } else {
                        let n: u16 = p.parse().ok()?;
                        Some(PortRange { from: n, to: n })
                    }
                });
                let rule_source = RuleSource::Cidr(source.clone());
                // Generate deterministic rule ID from content hash.
                let rule_id = {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    sg_record.id.0.hash(&mut hasher);
                    dir.hash(&mut hasher);
                    proto.hash(&mut hasher);
                    port_range.hash(&mut hasher);
                    rule_source.hash(&mut hasher);
                    RuleId(format!("rule-{:016x}", hasher.finish()))
                };
                let rule = syfrah_org::types::SecurityGroupRule {
                    id: rule_id,
                    sg_id: sg_record.id.clone(),
                    direction: dir,
                    protocol: proto,
                    port_range,
                    source: rule_source,
                    priority: 100,
                    description: None,
                };
                match sg_rule_store.add_rule(&rule) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::RemoveSgRule { sg: _, rule_id } => {
                let sg_rule_store = match &self.sg_rule_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "SG rule store not available in state machine".to_string(),
                        )
                    }
                };
                match sg_rule_store.remove_rule(rule_id) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
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
            StateMachineCommand::CreateNatGw { name, vpc, subnet } => {
                // Resolve VPC.
                let vpc_obj = match self.org_store.get_vpc(vpc) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("VPC not found: {vpc}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Resolve subnet in VPC.
                let sub = match self.org_store.find_subnets_by_name(subnet) {
                    Ok(matches) => {
                        let in_vpc: Vec<_> = matches
                            .into_iter()
                            .filter(|(_, s)| s.vpc_id == vpc_obj.id)
                            .collect();
                        match in_vpc.len() {
                            0 => {
                                return StateMachineResponse::Error(format!(
                                    "subnet '{subnet}' not found in VPC '{vpc}'"
                                ))
                            }
                            1 => in_vpc.into_iter().next().unwrap().1,
                            _ => {
                                return StateMachineResponse::Error(format!(
                                    "ambiguous subnet '{subnet}' in VPC '{vpc}'"
                                ))
                            }
                        }
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Use a deterministic placeholder for public IP — the actual nftables
                // setup runs on the leader after Raft apply returns.
                let public_ip = "0.0.0.0";
                match self
                    .org_store
                    .create_nat_gw(name, &vpc_obj.id, &sub.id, public_ip)
                {
                    Ok(gw) => StateMachineResponse::Created(gw.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteNatGw { name } => {
                let gw = match self.org_store.get_nat_gw_by_name(name) {
                    Ok(Some(g)) => g,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("nat-gw not found: {name}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.delete_nat_gw(&gw.vpc_id, name) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Route Table --
            StateMachineCommand::CreateRouteTable { name, vpc } => {
                match self.org_store.create_route_table_by_vpc_name(name, vpc) {
                    Ok(table) => StateMachineResponse::Created(table.id.0),
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteRouteTable { name, vpc } => {
                let result = if let Some(vname) = vpc {
                    self.org_store.delete_route_table_by_vpc_name(vname, name)
                } else {
                    // Scan all tables.
                    match self.org_store.list_route_tables() {
                        Ok(tables) => {
                            let matching: Vec<_> =
                                tables.iter().filter(|t| t.name == *name).collect();
                            match matching.len() {
                                0 => Err(syfrah_org::OrgError::RouteTableNotFound(name.clone())),
                                1 => self.org_store.delete_route_table(&matching[0].vpc_id, name),
                                _ => Err(syfrah_org::OrgError::Ambiguous(format!(
                                    "route table '{name}' exists in multiple VPCs"
                                ))),
                            }
                        }
                        Err(e) => Err(e),
                    }
                };
                match result {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::AssociateRouteTable { table, subnet } => {
                // Resolve subnet -> VPC -> route table.
                let sub = match self.org_store.find_subnets_by_name(subnet) {
                    Ok(m) if m.len() == 1 => m.into_iter().next().unwrap().1,
                    Ok(m) if m.is_empty() => {
                        return StateMachineResponse::Error(format!("subnet not found: {subnet}"))
                    }
                    Ok(_) => {
                        return StateMachineResponse::Error(format!(
                            "subnet '{subnet}' exists in multiple VPCs"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                let rt = match self.org_store.get_route_table(&sub.vpc_id, table) {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "route table not found: {table}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.associate_subnet_route_table(&sub.id, &rt.id) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DisassociateRouteTable { subnet } => {
                let sub = match self.org_store.find_subnets_by_name(subnet) {
                    Ok(m) if m.len() == 1 => m.into_iter().next().unwrap().1,
                    Ok(m) if m.is_empty() => {
                        return StateMachineResponse::Error(format!("subnet not found: {subnet}"))
                    }
                    Ok(_) => {
                        return StateMachineResponse::Error(format!(
                            "subnet '{subnet}' exists in multiple VPCs"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.disassociate_subnet_route_table(&sub.id) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Routes --
            StateMachineCommand::AddRoute {
                vpc,
                table,
                destination,
                target,
                priority,
            } => {
                let vpc_obj = match self.org_store.get_vpc(vpc) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("VPC not found: {vpc}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                let table_name = table.as_deref().unwrap_or("default");
                let rt = match self.org_store.get_route_table(&vpc_obj.id, table_name) {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "route table not found: {table_name}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                // Parse the route target string.
                use syfrah_org::types::RouteTarget;
                let route_target = if target.eq_ignore_ascii_case("local") {
                    RouteTarget::Local
                } else if target.eq_ignore_ascii_case("blackhole") {
                    RouteTarget::Blackhole
                } else if let Some(name) = target.strip_prefix("nat-gw:") {
                    RouteTarget::NatGateway(name.to_string())
                } else if let Some(name) = target.strip_prefix("peering:") {
                    RouteTarget::VpcPeering(name.to_string())
                } else {
                    return StateMachineResponse::Error(format!(
                        "invalid route target: '{target}'"
                    ));
                };
                match self
                    .org_store
                    .add_route(&rt.id, destination, route_target, *priority)
                {
                    Ok(_route) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DeleteRoute {
                vpc,
                table,
                destination,
            } => {
                let vpc_obj = match self.org_store.get_vpc(vpc) {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!("VPC not found: {vpc}"))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                let table_name = table.as_deref().unwrap_or("default");
                let rt = match self.org_store.get_route_table(&vpc_obj.id, table_name) {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        return StateMachineResponse::Error(format!(
                            "route table not found: {table_name}"
                        ))
                    }
                    Err(e) => return StateMachineResponse::Error(e.to_string()),
                };
                match self.org_store.remove_route(&rt.id, destination) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
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
            // All hypervisor mutations go through Raft so every node gets the
            // same set of hypervisor records. The scheduler reads from this
            // store (strongly consistent) for placement decisions.
            StateMachineCommand::RegisterHypervisor {
                name,
                region,
                zone,
                fabric_ipv6,
            } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                // If already exists, update region/zone/fabric_ipv6.
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.region = region.clone();
                        hv.zone = zone.clone();
                        if !fabric_ipv6.is_empty() {
                            hv.fabric_ipv6 = fabric_ipv6.clone();
                        }
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let hv = syfrah_org::Hypervisor {
                            id: syfrah_org::HypervisorId(format!("hv-{name}")),
                            name: name.clone(),
                            region: region.clone(),
                            zone: zone.clone(),
                            state: syfrah_org::HypervisorState::NotReady,
                            fabric_node_id: name.clone(),
                            public_ip: String::new(),
                            fabric_ipv6: fabric_ipv6.clone(),
                            hardware: syfrah_org::HardwareSpec {
                                cpu_model: String::new(),
                                cpu_cores_physical: 0,
                                cpu_threads_logical: 0,
                                memory_gb: 0,
                                local_disk_type: syfrah_org::DiskType::SSD,
                                local_disk_gb: 0,
                                gpu: None,
                                network_bandwidth_gbps: 0,
                                architecture: syfrah_org::CpuArchitecture::X86_64,
                            },
                            capacity: syfrah_org::AllocatableCapacity::default(),
                            labels: std::collections::HashMap::new(),
                            taints: Vec::new(),
                            created_at: now,
                        };
                        match hv_store.create(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::EnableHypervisor { name } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.update_state(name, syfrah_org::HypervisorState::Available) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DrainHypervisor { name } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.update_state(name, syfrah_org::HypervisorState::Draining) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::DecommissionHypervisor { name } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.update_state(name, syfrah_org::HypervisorState::Decommissioned) {
                    Ok(()) => StateMachineResponse::Ok,
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UpdateHypervisorLabels { name, labels } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.labels = labels.clone();
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        StateMachineResponse::Error(format!("hypervisor '{name}' not found"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UpdateHypervisorTaints { name, taints } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.taints = taints
                            .iter()
                            .map(|t| {
                                // Parse taint string "key=value:Effect"
                                let (kv, effect_str) =
                                    t.rsplit_once(':').unwrap_or((t, "NoSchedule"));
                                let effect = match effect_str {
                                    "NoExecute" => syfrah_org::TaintEffect::NoExecute,
                                    _ => syfrah_org::TaintEffect::NoSchedule,
                                };
                                let (key, value) = if let Some((k, v)) = kv.split_once('=') {
                                    (k.to_string(), Some(v.to_string()))
                                } else {
                                    (kv.to_string(), None)
                                };
                                syfrah_org::Taint { key, value, effect }
                            })
                            .collect();
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        StateMachineResponse::Error(format!("hypervisor '{name}' not found"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::UpdateHypervisorCapacity {
                name,
                allocatable_vcpus,
                allocatable_memory_mb,
                used_vcpus,
                used_memory_mb,
            } => {
                let hv_store = match &self.hypervisor_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "hypervisor store not available in state machine".to_string(),
                        )
                    }
                };
                match hv_store.get(name) {
                    Ok(Some(mut hv)) => {
                        hv.capacity.allocatable_vcpus = *allocatable_vcpus;
                        hv.capacity.allocatable_memory_mb = *allocatable_memory_mb;
                        hv.capacity.used_vcpus = *used_vcpus;
                        hv.capacity.used_memory_mb = *used_memory_mb;
                        hv.capacity.available_vcpus = allocatable_vcpus.saturating_sub(*used_vcpus);
                        hv.capacity.available_memory_mb =
                            allocatable_memory_mb.saturating_sub(*used_memory_mb);
                        match hv_store.update(&hv) {
                            Ok(()) => StateMachineResponse::Ok,
                            Err(e) => StateMachineResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        // Hypervisor not registered yet — skip silently.
                        // This can happen during startup when capacity updates arrive
                        // before registration is replicated.
                        StateMachineResponse::Ok
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- VM Placement --
            StateMachineCommand::PlaceVm {
                vm_id,
                hypervisor_id,
                subnet_id,
                ip,
                mac,
                generation,
            } => {
                let placement_store = match &self.placement_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "placement store not available in state machine".to_string(),
                        )
                    }
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // Resolve the VPC ID from the subnet.
                let vpc_id = match self.org_store.get_subnet_by_id(subnet_id) {
                    Ok(Some(s)) => s.vpc_id.0,
                    Ok(None) => subnet_id.split('/').next().unwrap_or("unknown").to_string(),
                    Err(_) => subnet_id.split('/').next().unwrap_or("unknown").to_string(),
                };
                let placement = syfrah_org::types::VmPlacement {
                    vpc_id,
                    vm_id: vm_id.clone(),
                    vm_mac: mac.clone(),
                    vm_ip: ip.clone(),
                    subnet_id: subnet_id.clone(),
                    hypervisor_id: hypervisor_id.clone(),
                    action: syfrah_org::types::PlacementAction::Add,
                    created_at: now,
                    placement_generation: *generation,
                };
                match placement_store.add_placement(&placement) {
                    Ok(()) => {
                        // Emit placement event for incremental FDB update.
                        let _ = self.placement_tx.send(PlacementEvent::Added {
                            vpc_id: placement.vpc_id,
                            vm_id: placement.vm_id,
                            vm_mac: placement.vm_mac,
                            vm_ip: placement.vm_ip,
                            subnet_id: placement.subnet_id,
                            hypervisor_id: placement.hypervisor_id,
                        });
                        StateMachineResponse::Ok
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::RemoveVm { vm_id } => {
                let placement_store = match &self.placement_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "placement store not available in state machine".to_string(),
                        )
                    }
                };
                // List all placements to find the one matching this VM.
                match placement_store.list_all() {
                    Ok(placements) => {
                        for p in &placements {
                            if p.vm_id == *vm_id {
                                // Capture info before removal for the event.
                                let event = PlacementEvent::Removed {
                                    vpc_id: p.vpc_id.clone(),
                                    vm_id: p.vm_id.clone(),
                                    vm_mac: p.vm_mac.clone(),
                                    vm_ip: p.vm_ip.clone(),
                                    hypervisor_id: p.hypervisor_id.clone(),
                                };
                                let _ = placement_store.remove_placement(&p.vpc_id, vm_id);
                                let _ = self.placement_tx.send(event);
                                return StateMachineResponse::Ok;
                            }
                        }
                        StateMachineResponse::Error(format!("placement not found for VM: {vm_id}"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }
            StateMachineCommand::RescheduleVm {
                vm_id,
                from: _,
                to,
                generation,
            } => {
                let placement_store = match &self.placement_store {
                    Some(s) => s,
                    None => {
                        return StateMachineResponse::Error(
                            "placement store not available in state machine".to_string(),
                        )
                    }
                };
                // Update the placement in-place: change hypervisor_id and generation.
                match placement_store.list_all() {
                    Ok(placements) => {
                        for p in &placements {
                            if p.vm_id == *vm_id {
                                let mut updated = p.clone();
                                updated.hypervisor_id = to.clone();
                                updated.placement_generation = *generation;
                                return match placement_store.add_placement(&updated) {
                                    Ok(()) => StateMachineResponse::Ok,
                                    Err(e) => StateMachineResponse::Error(e.to_string()),
                                };
                            }
                        }
                        StateMachineResponse::Error(format!("placement not found for VM: {vm_id}"))
                    }
                    Err(e) => StateMachineResponse::Error(e.to_string()),
                }
            }

            // -- Composite Transaction --
            StateMachineCommand::Composite { commands } => {
                // Apply all sub-commands atomically. If any fails, report the error
                // but continue (the Raft log is append-only, we can't undo committed entries).
                // In practice, the caller should validate before submitting.
                let mut results = Vec::with_capacity(commands.len());
                for sub_cmd in commands {
                    let resp = self.apply_command(sub_cmd);
                    let failed = matches!(resp, StateMachineResponse::Error(_));
                    results.push(resp);
                    if failed {
                        // On first error, stop applying remaining commands.
                        break;
                    }
                }
                // Check if any sub-command failed.
                let any_error = results
                    .iter()
                    .any(|r| matches!(r, StateMachineResponse::Error(_)));
                if any_error {
                    // Return the first error.
                    for r in &results {
                        if let StateMachineResponse::Error(msg) = r {
                            return StateMachineResponse::Error(format!(
                                "composite transaction failed: {msg}"
                            ));
                        }
                    }
                }
                StateMachineResponse::Composite(results)
            }
        }
    }
}

impl RaftSnapshotBuilder<SyfrahRaftConfig> for Arc<RedbStateMachine> {
    async fn build_snapshot(&mut self) -> Result<SnapshotOf<SyfrahRaftConfig>, io::Error> {
        let sm_state = self.sm_state.read().await;

        // Export all store tables for the full snapshot.
        let tables = self.export_store_tables();
        let table_count: usize = tables.values().map(|v| v.len()).sum();

        let full_data = FullSnapshotData {
            sm_state: (*sm_state).clone(),
            tables,
        };

        let data = serde_json::to_vec(&full_data)
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

        // Update counters.
        self.snapshot_count.fetch_add(1, Ordering::Relaxed);
        self.entries_since_snapshot.store(0, Ordering::Relaxed);

        info!(
            snapshot_size = data.len(),
            table_entries = table_count,
            "snapshot built (full store export)"
        );

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

            // Track entries applied since last snapshot.
            self.entries_since_snapshot.fetch_add(1, Ordering::Relaxed);

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

        // Try to deserialize as FullSnapshotData first (new format),
        // fall back to SmState-only (legacy format) for compatibility.
        let new_sm = if let Ok(full) = serde_json::from_slice::<FullSnapshotData>(&data) {
            // Restore all store tables from the snapshot.
            let table_count: usize = full.tables.values().map(|v| v.len()).sum();
            self.import_store_tables(&full.tables);
            info!(
                table_entries = table_count,
                "snapshot: restored store tables from full snapshot"
            );
            full.sm_state
        } else {
            // Legacy format: SmState only (no store data).
            serde_json::from_slice::<SmState>(&data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
        };

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

        // Reset entries counter since we just installed a snapshot.
        self.entries_since_snapshot.store(0, Ordering::Relaxed);

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
    fn apply_hypervisor_commands_with_store() {
        let (_dir, store) = make_org_store();
        // Create a hypervisor store and wire it into the state machine.
        let hv_db = syfrah_state::LayerDb::open_at(&_dir.path().join("hv.redb")).unwrap();
        let hv_store = std::sync::Arc::new(syfrah_org::HypervisorStore::new(hv_db));
        let sm = RedbStateMachine::new(store).with_hypervisor_store(hv_store.clone());

        // RegisterHypervisor should create a record in the store.
        let resp = sm.apply_command(&StateMachineCommand::RegisterHypervisor {
            name: "hv1".to_string(),
            region: "eu".to_string(),
            zone: "az1".to_string(),
            fabric_ipv6: "fd00::1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));

        // Verify the hypervisor was created.
        let hv = hv_store.get("hv1").unwrap().unwrap();
        assert_eq!(hv.region, "eu");
        assert_eq!(hv.zone, "az1");
        assert_eq!(hv.fabric_ipv6, "fd00::1");

        // EnableHypervisor should update state to Available.
        let resp = sm.apply_command(&StateMachineCommand::EnableHypervisor {
            name: "hv1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        let hv = hv_store.get("hv1").unwrap().unwrap();
        assert_eq!(hv.state, syfrah_org::HypervisorState::Available);

        // UpdateHypervisorCapacity should update capacity fields.
        let resp = sm.apply_command(&StateMachineCommand::UpdateHypervisorCapacity {
            name: "hv1".to_string(),
            allocatable_vcpus: 16,
            allocatable_memory_mb: 65536,
            used_vcpus: 4,
            used_memory_mb: 8192,
        });
        assert!(matches!(resp, StateMachineResponse::Ok));
        let hv = hv_store.get("hv1").unwrap().unwrap();
        assert_eq!(hv.capacity.allocatable_vcpus, 16);
        assert_eq!(hv.capacity.used_vcpus, 4);
    }

    #[test]
    fn apply_hypervisor_commands_without_store_returns_error() {
        let (_dir, store) = make_org_store();
        let sm = RedbStateMachine::new(store);
        // Without hypervisor store wired, commands should return error.
        let resp = sm.apply_command(&StateMachineCommand::RegisterHypervisor {
            name: "hv1".to_string(),
            region: "eu".to_string(),
            zone: "az1".to_string(),
            fabric_ipv6: "fd00::1".to_string(),
        });
        assert!(matches!(resp, StateMachineResponse::Error(_)));
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
