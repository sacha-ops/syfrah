//! Raft-aware compute handler — routes VM mutations through the scheduler and Raft.
//!
//! Architecture:
//! - If Raft is NOT initialized: pass through to inner handler (direct local create).
//! - If Raft IS initialized AND this is NOT the leader: forward to leader.
//! - If Raft IS initialized AND this IS the leader:
//!   1. Run scheduler (filter by zone, score by capacity from gossip)
//!   2. If scheduler picks REMOTE hypervisor: call remote Forge API
//!   3. If scheduler picks THIS hypervisor: create locally via inner handler
//!   4. Record PlaceVm in Raft

use std::collections::HashMap;
use std::sync::Arc;

use syfrah_api::handler::LayerHandler;
use syfrah_compute::control::{ComputeRequest, ComputeResponse};
use syfrah_controlplane::{
    GossipCluster, HypervisorGossipReport, PlacementConstraints, RaftClient, RemoteCreateVmRequest,
    Scheduler, StateMachineCommand, StateMachineResponse,
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Compute layer handler that routes VM creation through the scheduler when Raft is active.
pub struct RaftComputeHandler {
    /// The inner handler for direct local compute operations.
    inner: Arc<dyn LayerHandler>,
    /// Optional Raft client — set when controlplane is initialized.
    raft_client: RwLock<Option<RaftClient>>,
    /// Gossip cluster state (for scheduler capacity data).
    gossip_cluster: GossipCluster,
    /// Scheduler instance (set when Raft is initialized).
    scheduler: RwLock<Option<Scheduler>>,
    /// Local node name (for determining if scheduler picked us).
    local_node_name: String,
    /// Hypervisor store — used to populate scheduler candidates from
    /// registered hypervisors when gossip reports are not yet available.
    hypervisor_store: RwLock<Option<Arc<syfrah_org::HypervisorStore>>>,
    /// Org store — used to resolve subnets for Raft-based IP allocation.
    org_store: RwLock<Option<Arc<syfrah_org::OrgStore>>>,
}

impl RaftComputeHandler {
    /// Create a new Raft-aware compute handler.
    pub fn new(inner: Arc<dyn LayerHandler>, local_node_name: String) -> Self {
        Self {
            inner,
            raft_client: RwLock::new(None),
            gossip_cluster: GossipCluster::new(),
            scheduler: RwLock::new(None),
            local_node_name,
            hypervisor_store: RwLock::new(None),
            org_store: RwLock::new(None),
        }
    }

    /// Set the Raft client (called when controlplane is initialized).
    pub async fn set_raft_client(&self, client: RaftClient) {
        let mut guard = self.raft_client.write().await;
        *guard = Some(client);
    }

    /// Set the scheduler (called when controlplane is initialized).
    pub async fn set_scheduler(&self, scheduler: Scheduler) {
        let mut guard = self.scheduler.write().await;
        *guard = Some(scheduler);
    }

    /// Set the hypervisor store (called during daemon init).
    pub async fn set_hypervisor_store(&self, store: Arc<syfrah_org::HypervisorStore>) {
        let mut guard = self.hypervisor_store.write().await;
        *guard = Some(store);
    }

    /// Set the org store (called during daemon init for Raft IP allocation).
    pub async fn set_org_store(&self, store: Arc<syfrah_org::OrgStore>) {
        let mut guard = self.org_store.write().await;
        *guard = Some(store);
    }

    /// Get a reference to the gossip cluster state.
    /// The daemon wires this into the gossip agent.
    pub fn gossip_cluster(&self) -> &GossipCluster {
        &self.gossip_cluster
    }

    /// Ensure the gossip cluster has reports from all fabric peers.
    /// This bridges the gap when gossip data exchange hasn't converged yet.
    /// Uses fabric peer records (which have region/zone from mesh setup) and
    /// the local hypervisor store for the local node's data.
    fn populate_cluster_from_fabric_peers(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Load fabric state to get peer information.
        let state = match crate::store::load() {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to load fabric state for scheduler: {e}");
                return;
            }
        };

        // Add local node as a candidate.
        if self.gossip_cluster.get_report(&state.node_name).is_none() {
            let report = HypervisorGossipReport {
                hypervisor_id: state.mesh_ipv6.to_string(),
                node_name: state.node_name.clone(),
                region: state
                    .region
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                zone: state.zone.clone().unwrap_or_else(|| "default".to_string()),
                state: "Available".to_string(),
                allocatable_vcpus: 64,
                allocatable_memory_mb: 128 * 1024,
                used_vcpus: 0,
                used_memory_mb: 0,
                instance_count: 0,
                drain_status: false,
                timestamp: now,
            };
            debug!(
                "populated local scheduler candidate: {} (zone={})",
                state.node_name, report.zone
            );
            self.gossip_cluster.update_report(report);
        }

        // Add each fabric peer as a candidate.
        for peer in &state.peers {
            if self.gossip_cluster.get_report(&peer.name).is_some() {
                continue;
            }
            let region = peer.region.clone().unwrap_or_else(|| "default".to_string());
            let zone = peer.zone.clone().unwrap_or_else(|| "default".to_string());
            let report = HypervisorGossipReport {
                hypervisor_id: peer.mesh_ipv6.to_string(),
                node_name: peer.name.clone(),
                region: region.clone(),
                zone: zone.clone(),
                state: "Available".to_string(),
                allocatable_vcpus: 64,
                allocatable_memory_mb: 128 * 1024,
                used_vcpus: 0,
                used_memory_mb: 0,
                instance_count: 0,
                drain_status: false,
                timestamp: now,
            };
            debug!(
                "populated peer scheduler candidate: {} (zone={})",
                peer.name, zone
            );
            self.gossip_cluster.update_report(report);
        }
    }

    /// Handle a CreateVm request with scheduler integration.
    #[allow(clippy::too_many_arguments)]
    async fn handle_create_vm(
        &self,
        request: &[u8],
        caller_uid: Option<u32>,
        name: String,
        vcpus: u32,
        memory_mb: u32,
        image: String,
        ssh_key: Option<String>,
        disk_size_mb: Option<u32>,
        subnet: Option<syfrah_compute::types::SubnetInfo>,
        security_groups: Vec<String>,
        zone: Option<String>,
        node_selector: Vec<String>,
        anti_affinity: Option<String>,
        spread_topology: Option<String>,
    ) -> Vec<u8> {
        // Check if Raft is available.
        let raft_client = self.raft_client.read().await;
        let client = match raft_client.as_ref() {
            Some(c) => c,
            None => {
                // No Raft — direct local create (backward compatible).
                debug!("raft compute: no raft, falling back to direct create");
                return self.inner.handle(request.to_vec(), caller_uid).await;
            }
        };

        // If not leader, forward the request to the leader.
        // Include the zone constraint so the leader's scheduler places
        // the VM in the correct zone (bug #1127 fix).
        if !client.is_leader() {
            debug!("raft compute: not leader, forwarding CreateVm to leader");
            return self
                .forward_create_to_leader(
                    client,
                    &name,
                    &image,
                    vcpus,
                    memory_mb,
                    ssh_key.as_deref(),
                    disk_size_mb,
                    subnet.as_ref(),
                    &security_groups,
                    zone.as_deref(),
                )
                .await;
        }

        // Run the scheduler using Raft-replicated hypervisor records.
        // This is strongly consistent: every node has the same set of hypervisors
        // via Raft state machine replication.
        let constraints = PlacementConstraints::from_cli(
            zone.clone(),
            &node_selector,
            anti_affinity.clone(),
            spread_topology.clone(),
        );

        let scheduler_guard = self.scheduler.read().await;
        let scheduler = match scheduler_guard.as_ref() {
            Some(s) => s,
            None => {
                // Scheduler not initialized — create locally.
                debug!("raft compute: scheduler not initialized, creating locally");
                return self.inner.handle(request.to_vec(), caller_uid).await;
            }
        };

        // Prefer the Raft-replicated HypervisorStore for scheduling.
        // Falls back to gossip cluster if store is not wired.
        let hv_store_guard = self.hypervisor_store.read().await;
        let existing_placements: HashMap<String, u32> = HashMap::new();
        let schedule_result = if let Some(ref hv_store) = *hv_store_guard {
            scheduler.schedule_from_store(
                vcpus,
                memory_mb as u64,
                &constraints,
                hv_store,
                &[],
                &existing_placements,
            )
        } else {
            // Fallback: populate gossip cluster from fabric peers.
            self.populate_cluster_from_fabric_peers();
            scheduler.schedule(
                vcpus,
                memory_mb as u64,
                &constraints,
                &self.gossip_cluster,
                &[],
                &existing_placements,
            )
        };
        drop(hv_store_guard);

        match schedule_result {
            Ok(decision) => {
                info!(
                    "scheduler: placed VM '{}' on '{}' (score={:.2}, local_fallback={})",
                    name, decision.hypervisor_id, decision.score, decision.is_local_fallback
                );

                if decision.hypervisor_id == self.local_node_name || decision.is_local_fallback {
                    // Create locally.
                    debug!("raft compute: creating VM locally (scheduler picked this node)");
                    let result = self.inner.handle(request.to_vec(), caller_uid).await;

                    // After local create succeeds, submit NIC record to Raft so all
                    // nodes can resolve VM -> NIC for sg check and other queries.
                    if let Ok(resp) = serde_json::from_slice::<ComputeResponse>(&result) {
                        if !matches!(resp, ComputeResponse::Error(_)) {
                            if let Some(subnet_info) = subnet.as_ref() {
                                // Resolve subnet ID for NIC.
                                let org_guard = self.org_store.read().await;
                                if let Some(ref store) = *org_guard {
                                    if let Ok(matches) =
                                        store.find_subnets_by_name(&subnet_info.name)
                                    {
                                        if let Some((vpc_name, sub)) = matches.into_iter().next() {
                                            let sid = format!("{vpc_name}/{}", sub.name);
                                            // Find the NIC that was just created locally.
                                            if let Ok(Some(nic)) = store.find_nic_by_vm(&name) {
                                                let cmd = StateMachineCommand::CreateNic {
                                                    vm_id: name.clone(),
                                                    subnet_id: sid,
                                                    ip: nic.private_ip.clone(),
                                                    mac: nic.mac.clone(),
                                                };
                                                if let Err(e) = client.write(cmd).await {
                                                    debug!("NIC replication via Raft failed: {e}");
                                                }
                                            }
                                        }
                                    }
                                }
                                drop(org_guard);
                            }
                        }
                    }

                    result
                } else {
                    // Create on remote hypervisor.
                    // Step 1: Allocate IP through Raft BEFORE dispatching to remote.
                    // This ensures globally unique IPs — the remote Forge uses the
                    // pre-allocated IP instead of allocating from its local IPAM.
                    debug!(
                        "raft compute: creating VM on remote hypervisor '{}'",
                        decision.hypervisor_id
                    );

                    let mut pre_ip: Option<String> = None;
                    let mut pre_mac: Option<String> = None;

                    if let Some(subnet_info) = subnet.as_ref() {
                        // Resolve subnet ID from the org store.
                        let org_guard = self.org_store.read().await;
                        let subnet_id = if let Some(ref store) = *org_guard {
                            store
                                .find_subnets_by_name(&subnet_info.name)
                                .ok()
                                .and_then(|matches| matches.into_iter().next().map(|(vpc_name, sub)| format!("{vpc_name}/{}", sub.name)))
                        } else {
                            None
                        };
                        drop(org_guard);

                        if let Some(sid) = subnet_id {
                            // Allocate IP through Raft (globally consistent).
                            let alloc_cmd = StateMachineCommand::AllocateIp {
                                subnet_id: sid.clone(),
                            };
                            match client.write(alloc_cmd).await {
                                Ok(StateMachineResponse::AllocatedIp { ip, mac }) => {
                                    info!(
                                        "raft compute: pre-allocated IP {ip} (mac {mac}) via Raft for remote VM '{}'",
                                        name
                                    );
                                    pre_ip = Some(ip);
                                    pre_mac = Some(mac);
                                }
                                Ok(StateMachineResponse::Error(e)) => {
                                    warn!("raft compute: Raft IP allocation failed: {e}");
                                    let compute_resp = ComputeResponse::Error(format!(
                                        "IP allocation via Raft failed: {e}"
                                    ));
                                    return serde_json::to_vec(&compute_resp).unwrap_or_default();
                                }
                                Ok(_) => {
                                    warn!("raft compute: unexpected Raft response for AllocateIp");
                                }
                                Err(e) => {
                                    warn!("raft compute: Raft write failed for IP allocation: {e}");
                                    let compute_resp = ComputeResponse::Error(format!(
                                        "Raft IP allocation write failed: {e}"
                                    ));
                                    return serde_json::to_vec(&compute_resp).unwrap_or_default();
                                }
                            }
                        }
                    }

                    let forge_addr =
                        syfrah_controlplane::forge_addr_from_fabric_ipv6(&decision.hypervisor_addr);

                    let remote_req = RemoteCreateVmRequest {
                        name: name.clone(),
                        image: image.clone(),
                        vcpus,
                        memory_mb,
                        subnet: subnet.as_ref().map(|s| s.name.clone()),
                        project: None,
                        org: None,
                        ssh_key: ssh_key.clone(),
                        disk_size_mb,
                        security_groups: security_groups.clone(),
                        zone: None, // Don't pass zone to target — it creates locally.
                        pre_allocated_ip: pre_ip.clone(),
                        pre_allocated_mac: pre_mac.clone(),
                    };

                    match syfrah_controlplane::create_vm_on_remote(&forge_addr, &remote_req).await {
                        Ok(resp) if resp.success => {
                            info!(
                                "remote create succeeded: VM '{}' on '{}' (ip={:?})",
                                name, decision.hypervisor_id, resp.ip
                            );

                            // Submit NIC record to Raft so all nodes can resolve
                            // VM -> NIC for sg check and other cross-node queries.
                            if let (Some(ref ip), Some(ref mac)) = (&pre_ip, &pre_mac) {
                                if let Some(subnet_info) = subnet.as_ref() {
                                    let org_guard = self.org_store.read().await;
                                    if let Some(ref store) = *org_guard {
                                        if let Ok(matches) =
                                            store.find_subnets_by_name(&subnet_info.name)
                                        {
                                            if let Some((vpc_name, sub)) = matches.into_iter().next() {
                                                let sid = format!("{vpc_name}/{}", sub.name);
                                                let cmd = StateMachineCommand::CreateNic {
                                                    vm_id: name.clone(),
                                                    subnet_id: sid,
                                                    ip: ip.clone(),
                                                    mac: mac.clone(),
                                                };
                                                if let Err(e) = client.write(cmd).await {
                                                    debug!("NIC replication via Raft failed: {e}");
                                                }
                                            }
                                        }
                                    }
                                    drop(org_guard);
                                }
                            }

                            // Build a compute response mimicking local create.
                            let vm_json = serde_json::json!({
                                "id": resp.vm_id.unwrap_or_else(|| name.clone()),
                                "name": name,
                                "image": image,
                                "vcpus": vcpus,
                                "memory_mb": memory_mb,
                                "ip": resp.ip.as_deref().or(pre_ip.as_deref()),
                                "hypervisor": decision.hypervisor_id,
                                "zone": zone,
                                "status": "Created",
                            });
                            let compute_resp = ComputeResponse::Vm(vm_json);
                            serde_json::to_vec(&compute_resp).unwrap_or_default()
                        }
                        Ok(resp) => {
                            warn!(
                                "remote create failed on '{}': {:?}",
                                decision.hypervisor_id, resp.error
                            );
                            let error_msg =
                                resp.error.unwrap_or_else(|| "unknown error".to_string());
                            let compute_resp = ComputeResponse::Error(format!(
                                "scheduler placed VM on '{}' but creation failed: {}",
                                decision.hypervisor_id, error_msg
                            ));
                            serde_json::to_vec(&compute_resp).unwrap_or_default()
                        }
                        Err(e) => {
                            warn!(
                                "failed to reach remote hypervisor '{}': {}",
                                decision.hypervisor_id, e
                            );
                            let compute_resp = ComputeResponse::Error(format!(
                                "scheduler placed VM on '{}' but node unreachable: {}",
                                decision.hypervisor_id, e
                            ));
                            serde_json::to_vec(&compute_resp).unwrap_or_default()
                        }
                    }
                }
            }
            Err(e) => {
                warn!("scheduler error: {e}");
                let compute_resp = ComputeResponse::Error(format!("scheduler error: {e}"));
                serde_json::to_vec(&compute_resp).unwrap_or_default()
            }
        }
    }

    /// Forward a CreateVm request to the leader by calling its Forge HTTP API.
    ///
    /// The zone constraint is forwarded so the leader's Forge API can route
    /// through the scheduler to place the VM in the correct zone.
    #[allow(clippy::too_many_arguments)]
    async fn forward_create_to_leader(
        &self,
        client: &RaftClient,
        name: &str,
        image: &str,
        vcpus: u32,
        memory_mb: u32,
        ssh_key: Option<&str>,
        disk_size_mb: Option<u32>,
        subnet: Option<&syfrah_compute::types::SubnetInfo>,
        security_groups: &[String],
        zone: Option<&str>,
    ) -> Vec<u8> {
        let leader_raft_addr = match client.leader_addr() {
            Some(addr) => addr,
            None => {
                let resp = ComputeResponse::Error(
                    "no Raft leader available — cluster may be electing".to_string(),
                );
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
        };

        // Derive Forge address from Raft address (port 7100 instead of 7200).
        let leader_forge_addr = leader_raft_addr.replace(":7200", ":7100");

        let remote_req = RemoteCreateVmRequest {
            name: name.to_string(),
            image: image.to_string(),
            vcpus,
            memory_mb,
            subnet: subnet.map(|s| s.name.clone()),
            project: None,
            org: None,
            ssh_key: ssh_key.map(|s| s.to_string()),
            disk_size_mb,
            security_groups: security_groups.to_vec(),
            zone: zone.map(|s| s.to_string()),
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };

        // Forward to leader WITHOUT ?direct=true so the leader runs the
        // scheduler with the zone constraint (vs create_vm_on_remote which
        // uses ?direct=true for placement on a specific target hypervisor).
        match syfrah_controlplane::forward_create_to_leader(&leader_forge_addr, &remote_req).await {
            Ok(resp) if resp.success => {
                info!(
                    "forwarded CreateVm '{}' to leader at {}: success",
                    name, leader_forge_addr
                );
                let vm_json = serde_json::json!({
                    "id": resp.vm_id.unwrap_or_else(|| name.to_string()),
                    "name": name,
                    "image": image,
                    "vcpus": vcpus,
                    "memory_mb": memory_mb,
                    "ip": resp.ip,
                    "status": "Created",
                });
                let compute_resp = ComputeResponse::Vm(vm_json);
                serde_json::to_vec(&compute_resp).unwrap_or_default()
            }
            Ok(resp) => {
                let error_msg = resp.error.unwrap_or_else(|| "unknown error".to_string());
                warn!(
                    "leader returned error for CreateVm '{}': {}",
                    name, error_msg
                );
                let compute_resp =
                    ComputeResponse::Error(format!("leader returned error: {error_msg}"));
                serde_json::to_vec(&compute_resp).unwrap_or_default()
            }
            Err(e) => {
                warn!("failed to reach leader at {}: {}", leader_forge_addr, e);
                let compute_resp = ComputeResponse::Error(format!("failed to reach leader: {e}"));
                serde_json::to_vec(&compute_resp).unwrap_or_default()
            }
        }
    }
}

#[async_trait::async_trait]
impl LayerHandler for RaftComputeHandler {
    async fn handle(&self, request: Vec<u8>, caller_uid: Option<u32>) -> Vec<u8> {
        // Try to parse as a compute request to detect CreateVm.
        let req: ComputeRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(_) => {
                // Can't parse — pass through to inner handler.
                return self.inner.handle(request, caller_uid).await;
            }
        };

        // Only intercept CreateVm for scheduler routing.
        // All other operations (list, get, start, stop, delete) are local.
        match req {
            ComputeRequest::CreateVm {
                name,
                vcpus,
                memory_mb,
                image,
                gpu_bdf: _,
                tap: _,
                ssh_key,
                disk_size_mb,
                subnet,
                security_groups,
                zone,
                node_selector,
                anti_affinity,
                spread_topology,
            } => {
                self.handle_create_vm(
                    &request,
                    caller_uid,
                    name,
                    vcpus,
                    memory_mb,
                    image,
                    ssh_key,
                    disk_size_mb,
                    subnet,
                    security_groups,
                    zone,
                    node_selector,
                    anti_affinity,
                    spread_topology,
                )
                .await
            }
            _ => {
                // Pass through to inner handler.
                self.inner.handle(request, caller_uid).await
            }
        }
    }
}
