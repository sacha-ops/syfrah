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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use syfrah_api::handler::LayerHandler;
use syfrah_compute::control::{ComputeRequest, ComputeResponse};
use syfrah_controlplane::{
    commands::VolumeType, GossipCluster, HypervisorGossipReport, PlacementConstraints, RaftClient,
    RemoteCreateVmRequest, Scheduler, StateMachineCommand, StateMachineResponse,
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
    /// Storage store — used to read configured storage zones for preflight checks.
    storage_store: RwLock<Option<Arc<syfrah_org::StorageStore>>>,
    /// Raft state machine — used to read VM records for `vm get`/`vm list` (#1311).
    state_machine: RwLock<Option<Arc<syfrah_controlplane::RedbStateMachine>>>,
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
            storage_store: RwLock::new(None),
            state_machine: RwLock::new(None),
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

    /// Set the storage store (called during daemon init for storage preflight checks).
    pub async fn set_storage_store(&self, store: Arc<syfrah_org::StorageStore>) {
        let mut guard = self.storage_store.write().await;
        *guard = Some(store);
    }

    /// Set the Raft state machine (called during daemon init for VM record lookups #1311).
    pub async fn set_state_machine(&self, sm: Arc<syfrah_controlplane::RedbStateMachine>) {
        let mut guard = self.state_machine.write().await;
        *guard = Some(sm);
    }

    /// Build a set of zone names that have storage configured.
    /// Returns None if the storage store is not available.
    async fn configured_storage_zones(&self) -> Option<HashSet<String>> {
        let guard = self.storage_store.read().await;
        let store = guard.as_ref()?;
        match store.list_storage_configs() {
            Ok(configs) => Some(configs.into_iter().map(|(zone, _)| zone).collect()),
            Err(e) => {
                warn!("scheduler: failed to list storage configs: {e}");
                None
            }
        }
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
                s3_reachable: None,
                s3_put_latency_ms: None,
                s3_get_latency_ms: None,
                s3_degradation_level: None,
                storage_health: None,
                storage_dirty_bytes: 0,
                cache_metrics: None,
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
                s3_reachable: None,
                s3_put_latency_ms: None,
                s3_get_latency_ms: None,
                s3_degradation_level: None,
                storage_health: None,
                storage_dirty_bytes: 0,
                cache_metrics: None,
            };
            debug!(
                "populated peer scheduler candidate: {} (zone={})",
                peer.name, zone
            );
            self.gossip_cluster.update_report(report);
        }
    }

    /// Handle a CreateVm request with scheduler integration.
    ///
    /// Phase 1 (#1311): When Raft is available, writes a `CreateVmIntent` to Raft
    /// and returns immediately with `VmIntentAccepted`. The daemon continues
    /// provisioning in the background and updates the VM phase via `UpdateVmPhase`.
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
        root_disk_size_gb: u32,
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

        // Phase 1 (#1311): Write CreateVmIntent to Raft so the CLI can return
        // immediately. The daemon continues provisioning in the background.
        let intent_cmd = StateMachineCommand::CreateVmIntent {
            name: name.clone(),
            image: image.clone(),
            vcpus,
            memory_mb,
            zone: zone.clone(),
            subnet: subnet.as_ref().map(|s| s.name.clone()),
            env: None,
            project: None,
            org: None,
            ssh_key_path: ssh_key.clone(),
            disk_size_gb: root_disk_size_gb,
        };

        match client.write(intent_cmd).await {
            Ok(StateMachineResponse::Created(vm_id)) => {
                info!("raft compute: VM intent accepted for '{vm_id}' (Pending)");

                // Spawn background provisioning — the daemon does the actual
                // scheduling + create synchronously, updating the phase as it
                // goes. This keeps Phase 1 scope tight: CLI returns fast,
                // daemon still does the work.
                let inner = Arc::clone(&self.inner);
                let request_bytes = request.to_vec();
                let raft_client_bg = client.clone();
                let name_bg = name.clone();
                tokio::spawn(async move {
                    // Update phase to Scheduling.
                    let _ = raft_client_bg
                        .write(StateMachineCommand::UpdateVmPhase {
                            vm_id: name_bg.clone(),
                            phase: "Scheduling".to_string(),
                            hypervisor_id: None,
                            ip: None,
                            error: None,
                        })
                        .await;

                    // Update phase to Creating.
                    let _ = raft_client_bg
                        .write(StateMachineCommand::UpdateVmPhase {
                            vm_id: name_bg.clone(),
                            phase: "Creating".to_string(),
                            hypervisor_id: None,
                            ip: None,
                            error: None,
                        })
                        .await;

                    // Perform the actual provisioning via the inner handler.
                    let result = inner.handle(request_bytes, caller_uid).await;

                    // Check if provisioning succeeded and update phase accordingly.
                    match serde_json::from_slice::<ComputeResponse>(&result) {
                        Ok(ComputeResponse::Vm(v)) => {
                            let ip = v.get("ip").and_then(|i| i.as_str()).map(|s| s.to_string());
                            let hv = v
                                .get("hypervisor_id")
                                .and_then(|h| h.as_str())
                                .map(|s| s.to_string());
                            let _ = raft_client_bg
                                .write(StateMachineCommand::UpdateVmPhase {
                                    vm_id: name_bg.clone(),
                                    phase: "Running".to_string(),
                                    hypervisor_id: hv,
                                    ip,
                                    error: None,
                                })
                                .await;
                        }
                        Ok(ComputeResponse::Error(msg)) => {
                            let _ = raft_client_bg
                                .write(StateMachineCommand::UpdateVmPhase {
                                    vm_id: name_bg.clone(),
                                    phase: "Failed".to_string(),
                                    hypervisor_id: None,
                                    ip: None,
                                    error: Some(msg),
                                })
                                .await;
                        }
                        _ => {
                            let _ = raft_client_bg
                                .write(StateMachineCommand::UpdateVmPhase {
                                    vm_id: name_bg.clone(),
                                    phase: "Failed".to_string(),
                                    hypervisor_id: None,
                                    ip: None,
                                    error: Some(
                                        "unexpected response from compute handler".to_string(),
                                    ),
                                })
                                .await;
                        }
                    }
                });

                // Return immediately with the intent accepted response.
                let resp = ComputeResponse::VmIntentAccepted {
                    vm_id,
                    phase: "Pending".to_string(),
                };
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
            Ok(StateMachineResponse::Error(e)) => {
                let resp = ComputeResponse::Error(format!("failed to write VM intent: {e}"));
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
            Err(e) => {
                warn!("raft compute: failed to write CreateVmIntent to Raft: {e}");
                // Fall through to direct create path as fallback.
            }
            _ => {
                // Unexpected response; fall through to original path.
            }
        }
        // Drop raft client before fall-through since we re-acquire it below.
        drop(raft_client);
        let raft_client = self.raft_client.read().await;
        let client = match raft_client.as_ref() {
            Some(c) => c,
            None => {
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
        let storage_zones = self.configured_storage_zones().await;
        let schedule_result = if let Some(ref hv_store) = *hv_store_guard {
            scheduler.schedule_from_store_with_storage(
                vcpus,
                memory_mb as u64,
                &constraints,
                hv_store,
                &[],
                &existing_placements,
                storage_zones.as_ref(),
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
                                        if let Some((vpc_name, _)) = matches.into_iter().next() {
                                            let sid = format!("{vpc_name}/{}", subnet_info.name);
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

                            // Issue CreateVolume Raft command for the root volume.
                            self.create_root_volume_via_raft(
                                client,
                                &name,
                                root_disk_size_gb,
                                subnet.as_ref(),
                                &self.local_node_name,
                            )
                            .await;
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
                                .and_then(|matches| {
                                    matches.into_iter().next().map(|(vpc_name, _)| {
                                        format!("{vpc_name}/{}", subnet_info.name)
                                    })
                                })
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
                                            if let Some((vpc_name, _)) = matches.into_iter().next()
                                            {
                                                let sid =
                                                    format!("{vpc_name}/{}", subnet_info.name);
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

                            // Issue CreateVolume Raft command for the root volume.
                            self.create_root_volume_via_raft(
                                client,
                                &name,
                                root_disk_size_gb,
                                subnet.as_ref(),
                                &decision.hypervisor_id,
                            )
                            .await;

                            // Build a compute response mimicking local create.
                            let root_vol_id = format!("vol-root-{name}");
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
                                "root_volume_id": root_vol_id,
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

    /// Issue a CreateVolume Raft command for the auto-created root volume.
    ///
    /// Called after a successful VM creation (local or remote). The root volume
    /// is registered in the Raft state machine so it can be tracked, quota-checked,
    /// and cleaned up on VM delete.
    async fn create_root_volume_via_raft(
        &self,
        client: &RaftClient,
        vm_name: &str,
        size_gb: u32,
        subnet: Option<&syfrah_compute::types::SubnetInfo>,
        target_hypervisor: &str,
    ) {
        let root_volume_id = format!("vol-root-{vm_name}");
        let root_volume_name = format!("root-{vm_name}");

        // Derive org/project/env from the subnet's env_id if available.
        // env_id format is "org/project/env" — split it to extract each part.
        // Fall back to "default" when no subnet context is provided.
        let (org_id, project_id, env_id) = if let Some(si) = subnet {
            let parts: Vec<&str> = si.env_id.split('/').collect();
            if parts.len() == 3 {
                (
                    parts[0].to_string(),
                    parts[1].to_string(),
                    parts[2].to_string(),
                )
            } else {
                // env_id doesn't follow expected format; use it as env, derive rest.
                (
                    "default".to_string(),
                    "default".to_string(),
                    si.env_id.clone(),
                )
            }
        } else {
            (
                "default".to_string(),
                "default".to_string(),
                "default".to_string(),
            )
        };

        let cmd = StateMachineCommand::CreateVolume {
            id: root_volume_id.clone(),
            name: root_volume_name,
            size_gb,
            org_id,
            project_id,
            env_id,
            volume_type: VolumeType::Root,
            hypervisor_id: Some(target_hypervisor.to_string()),
            zone: None, // TODO(#1282): inherit zone from target hypervisor
        };

        match client.write(cmd).await {
            Ok(StateMachineResponse::Created(id)) => {
                info!("raft compute: created root volume '{id}' for VM '{vm_name}'");
            }
            Ok(StateMachineResponse::Error(e)) => {
                warn!("raft compute: failed to create root volume '{root_volume_id}': {e}");
            }
            Ok(_) => {
                debug!("raft compute: unexpected response for CreateVolume '{root_volume_id}'");
            }
            Err(e) => {
                warn!("raft compute: Raft write failed for root volume '{root_volume_id}': {e}");
            }
        }
    }

    /// Handle a DeleteVm request: look up the root volume, delete the VM,
    /// then cascade-delete the root volume via Raft.
    async fn handle_delete_vm(
        &self,
        request: &[u8],
        caller_uid: Option<u32>,
        vm_id: &str,
        _retain_disk: bool,
    ) -> Vec<u8> {
        // Before deleting, look up the VM to find its root_volume_id.
        // We do this by sending a GetVm request to the inner handler.
        let get_req = ComputeRequest::GetVm {
            id: vm_id.to_string(),
        };
        let get_payload = serde_json::to_vec(&get_req).unwrap_or_default();
        let get_result = self.inner.handle(get_payload, caller_uid).await;
        let root_volume_id = serde_json::from_slice::<ComputeResponse>(&get_result)
            .ok()
            .and_then(|resp| match resp {
                ComputeResponse::Vm(v) => v
                    .get("root_volume_id")
                    .and_then(|id| id.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            });

        // Now delete the VM via the inner handler.
        let result = self.inner.handle(request.to_vec(), caller_uid).await;

        // If deletion succeeded and we have a root volume, delete it via Raft.
        let delete_succeeded = serde_json::from_slice::<ComputeResponse>(&result)
            .ok()
            .map(|resp| !matches!(resp, ComputeResponse::Error(_)))
            .unwrap_or(false);

        if delete_succeeded {
            if let Some(vol_id) = root_volume_id {
                let raft_client = self.raft_client.read().await;
                if let Some(client) = raft_client.as_ref() {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    let cmd = StateMachineCommand::DeleteVolume {
                        volume_id: vol_id.clone(),
                        cascade: true,
                        deleted_at: now,
                    };

                    match client.write(cmd).await {
                        Ok(StateMachineResponse::Ok) => {
                            info!("raft compute: deleted root volume '{vol_id}' for VM '{vm_id}'");
                        }
                        Ok(StateMachineResponse::Error(e)) => {
                            warn!("raft compute: failed to delete root volume '{vol_id}': {e}");
                        }
                        Ok(_) => {
                            debug!("raft compute: unexpected response for DeleteVolume '{vol_id}'");
                        }
                        Err(e) => {
                            warn!(
                                "raft compute: Raft write failed for DeleteVolume '{vol_id}': {e}"
                            );
                        }
                    }
                } else {
                    debug!(
                        "raft compute: no raft client, skipping root volume cleanup for '{vm_id}'"
                    );
                }
            }
        }

        result
    }

    /// Convert a Raft VmRecord to a JSON value matching the CLI's expected format.
    fn vm_record_to_json(record: &syfrah_controlplane::VmRecord) -> serde_json::Value {
        serde_json::json!({
            "id": record.id,
            "phase": record.phase.to_string(),
            "vcpus": record.vcpus,
            "memory_mb": record.memory_mb,
            "image": record.image,
            "runtime": null,
            "created_at": record.created_at,
            "uptime_secs": null,
            "ip": record.ip,
            "subnet": record.subnet_id,
            "vpc": null,
            "security_groups": [],
            "hypervisor_id": record.hypervisor_id,
            "region": null,
            "zone": record.zone,
            "root_volume_id": format!("vol-root-{}", record.id),
            "error": record.error,
        })
    }

    /// Handle GetVm by checking Raft VM records first, then falling back to the
    /// inner handler. When both exist, prefer the inner handler's data but
    /// overlay the Raft phase if the VM is still provisioning.
    async fn handle_get_vm(&self, request: &[u8], caller_uid: Option<u32>, vm_id: &str) -> Vec<u8> {
        // Check Raft state machine for the VM record.
        let raft_record = {
            let sm_guard = self.state_machine.read().await;
            sm_guard.as_ref().and_then(|sm| {
                let storage = sm.storage.read().unwrap();
                storage.vm_records.get(vm_id).cloned()
            })
        };

        // Also try the inner handler (local compute).
        let inner_result = self.inner.handle(request.to_vec(), caller_uid).await;
        let inner_resp: Option<ComputeResponse> = serde_json::from_slice(&inner_result).ok();

        match (raft_record, inner_resp) {
            (Some(record), Some(ComputeResponse::Vm(mut v))) => {
                // Merge: use inner handler's runtime data, overlay Raft phase
                // if the VM hasn't fully started yet.
                let raft_phase = record.phase.to_string();
                v["phase"] = serde_json::Value::String(raft_phase);
                if let Some(ref err) = record.error {
                    v["error"] = serde_json::Value::String(err.clone());
                }
                let resp = ComputeResponse::Vm(v);
                serde_json::to_vec(&resp).unwrap_or_default()
            }
            (Some(record), _) => {
                // VM only exists in Raft (still provisioning or on remote node).
                let resp = ComputeResponse::Vm(Self::vm_record_to_json(&record));
                serde_json::to_vec(&resp).unwrap_or_default()
            }
            (None, _) => {
                // No Raft record — return inner handler's response as-is.
                inner_result
            }
        }
    }

    /// Handle ListVms by merging Raft VM records with local VM list.
    async fn handle_list_vms(&self, request: &[u8], caller_uid: Option<u32>) -> Vec<u8> {
        // Get local VMs from inner handler.
        let inner_result = self.inner.handle(request.to_vec(), caller_uid).await;
        let inner_resp: Option<ComputeResponse> = serde_json::from_slice(&inner_result).ok();

        // Get Raft VM records.
        let raft_records = {
            let sm_guard = self.state_machine.read().await;
            sm_guard.as_ref().map(|sm| {
                let storage = sm.storage.read().unwrap();
                storage.vm_records.clone()
            })
        };

        match (raft_records, inner_resp) {
            (Some(records), Some(ComputeResponse::VmList(mut local_vms))) => {
                // Build a set of local VM IDs for dedup.
                let local_ids: std::collections::HashSet<String> = local_vms
                    .iter()
                    .filter_map(|v| {
                        v.get("id")
                            .and_then(|id| id.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();

                // Overlay Raft phase on local VMs.
                for vm in &mut local_vms {
                    if let Some(id) = vm.get("id").and_then(|id| id.as_str()) {
                        if let Some(record) = records.get(id) {
                            vm["phase"] = serde_json::Value::String(record.phase.to_string());
                        }
                    }
                }

                // Add VMs that exist only in Raft (provisioning on remote nodes).
                for (id, record) in &records {
                    if !local_ids.contains(id)
                        && record.phase != syfrah_controlplane::VmPhase::Deleting
                    {
                        local_vms.push(Self::vm_record_to_json(record));
                    }
                }

                let resp = ComputeResponse::VmList(local_vms);
                serde_json::to_vec(&resp).unwrap_or_default()
            }
            _ => inner_result,
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

        // Intercept VM lifecycle operations for Raft integration (#1311).
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
                root_disk_size_gb,
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
                    root_disk_size_gb,
                )
                .await
            }
            ComputeRequest::DeleteVm { id, retain_disk } => {
                self.handle_delete_vm(&request, caller_uid, &id, retain_disk)
                    .await
            }
            ComputeRequest::GetVm { id } => self.handle_get_vm(&request, caller_uid, &id).await,
            ComputeRequest::ListVms => self.handle_list_vms(&request, caller_uid).await,
            _ => {
                // Pass through to inner handler.
                self.inner.handle(request, caller_uid).await
            }
        }
    }
}
