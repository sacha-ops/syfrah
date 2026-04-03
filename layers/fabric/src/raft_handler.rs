//! Raft-aware org handler — routes ALL mutations through Raft when the control plane is active.
//!
//! Architecture:
//! - Mutation requests → convert to `StateMachineCommand` → submit to Raft →
//!   state machine applies to redb on every node → read back entity from local redb.
//! - Read requests → served directly from local redb.
//! - Fallback: if Raft is not initialized, all requests go to the inner handler (direct writes).
//!
//! Complex operations (NatGwCreate with nftables, NatGwDelete with nftables cleanup)
//! store the data via Raft for replication, then run node-local side effects on the
//! leader only after Raft returns success.

use std::sync::Arc;

use syfrah_api::handler::LayerHandler;
use syfrah_controlplane::commands::{StateMachineCommand, StateMachineResponse};
use syfrah_controlplane::RaftClient;
use syfrah_org::api::{OrgRequest, OrgResponse};
use syfrah_org::OrgStore;
use tokio::sync::RwLock;
use tracing::debug;

/// Org layer handler that routes mutations through Raft when available.
pub struct RaftOrgHandler {
    /// The inner handler for direct reads and fallback writes.
    inner: Arc<dyn LayerHandler>,
    /// Direct store access for read-after-write (entity responses after Raft apply).
    org_store: Arc<OrgStore>,
    /// Optional Raft client — set when controlplane is initialized.
    raft_client: RwLock<Option<RaftClient>>,
    /// Optional network backend for peering data plane side effects.
    network_backend: RwLock<Option<Arc<dyn syfrah_overlay::NetworkBackend>>>,
}

impl RaftOrgHandler {
    /// Create a new Raft-aware org handler wrapping the given inner handler.
    pub fn new(inner: Arc<dyn LayerHandler>, org_store: Arc<OrgStore>) -> Self {
        Self {
            inner,
            org_store,
            raft_client: RwLock::new(None),
            network_backend: RwLock::new(None),
        }
    }

    /// Set the network backend for peering data plane side effects.
    pub async fn set_network_backend(&self, backend: Arc<dyn syfrah_overlay::NetworkBackend>) {
        let mut guard = self.network_backend.write().await;
        *guard = Some(backend);
    }

    /// Set the Raft client (called when controlplane is initialized).
    pub async fn set_raft_client(&self, client: RaftClient) {
        let mut guard = self.raft_client.write().await;
        *guard = Some(client);
    }
}

/// Check if a request is a read (no mutation needed).
fn is_read_request(req: &OrgRequest) -> bool {
    matches!(
        req,
        OrgRequest::OrgList
            | OrgRequest::ProjectList { .. }
            | OrgRequest::EnvList { .. }
            | OrgRequest::VpcList { .. }
            | OrgRequest::VpcPeeringsList { .. }
            | OrgRequest::SubnetList { .. }
            | OrgRequest::SgList { .. }
            | OrgRequest::SgShow { .. }
            | OrgRequest::SgListForNic { .. }
            | OrgRequest::SgListRules { .. }
            | OrgRequest::SgCheck { .. }
            | OrgRequest::NatGwList { .. }
            | OrgRequest::NatGwShow { .. }
            | OrgRequest::RouteTableList { .. }
            | OrgRequest::RouteList { .. }
            | OrgRequest::SubnetResolve { .. }
    )
}

/// Convert an org mutation request to a state machine command.
///
/// Every mutation that writes to redb MUST be mapped here so that all nodes
/// get the same writes through Raft log replay.
fn to_raft_command(req: &OrgRequest) -> Option<StateMachineCommand> {
    match req {
        // -- Org --
        OrgRequest::OrgCreate { name } => {
            Some(StateMachineCommand::CreateOrg { name: name.clone() })
        }
        OrgRequest::OrgDelete { name } => {
            Some(StateMachineCommand::DeleteOrg { name: name.clone() })
        }

        // -- Project --
        OrgRequest::ProjectCreate { name, org } => Some(StateMachineCommand::CreateProject {
            name: name.clone(),
            org: org.clone(),
        }),
        OrgRequest::ProjectDelete { name, org } => Some(StateMachineCommand::DeleteProject {
            name: name.clone(),
            org: org.clone(),
        }),

        // -- Environment --
        OrgRequest::EnvCreate {
            name,
            project,
            org,
            ttl,
            deletion_protection,
            labels,
        } => Some(StateMachineCommand::CreateEnv {
            name: name.clone(),
            project: project.clone(),
            org: org.clone(),
            ttl: *ttl,
            deletion_protection: *deletion_protection,
            labels: labels.clone(),
        }),
        OrgRequest::EnvDestroy { name, project, org } => Some(StateMachineCommand::DeleteEnv {
            name: name.clone(),
            project: project.clone(),
            org: org.clone(),
        }),
        OrgRequest::EnvExtend {
            name,
            project,
            org,
            ttl_seconds,
        } => Some(StateMachineCommand::ExtendEnv {
            name: name.clone(),
            project: project.clone(),
            org: org.clone(),
            ttl_seconds: *ttl_seconds,
        }),
        OrgRequest::EnvUpdate {
            name,
            project,
            org,
            deletion_protection,
        } => Some(StateMachineCommand::UpdateEnv {
            name: name.clone(),
            project: project.clone(),
            org: org.clone(),
            deletion_protection: *deletion_protection,
        }),

        // -- VPC --
        OrgRequest::VpcCreate {
            name,
            org,
            project,
            shared,
            cidr,
        } => {
            // Pre-compute the owner string and CIDR for the state machine.
            let owner = if *shared {
                org.clone()
            } else {
                match project {
                    Some(p) => format!("{org}/{p}"),
                    None => return None, // Validation error — will be caught by inner handler.
                }
            };
            let cidr_str = cidr.clone().unwrap_or_else(|| {
                if *shared {
                    "10.100.0.0/16".to_string()
                } else {
                    "10.1.0.0/16".to_string()
                }
            });
            Some(StateMachineCommand::CreateVpc {
                name: name.clone(),
                cidr: cidr_str,
                owner,
                shared: *shared,
            })
        }
        OrgRequest::VpcDelete { name } => {
            Some(StateMachineCommand::DeleteVpc { name: name.clone() })
        }
        OrgRequest::VpcAttach { vpc, project } => Some(StateMachineCommand::AttachVpc {
            vpc: vpc.clone(),
            project: project.clone(),
        }),
        OrgRequest::VpcDetach { vpc, project } => Some(StateMachineCommand::DetachVpc {
            vpc: vpc.clone(),
            project: project.clone(),
        }),
        OrgRequest::VpcPeer { from, to } => Some(StateMachineCommand::PeerVpc {
            vpc_a: from.clone(),
            vpc_b: to.clone(),
        }),
        OrgRequest::VpcUnpeer { from, to } => Some(StateMachineCommand::UnpeerVpc {
            vpc_a: from.clone(),
            vpc_b: to.clone(),
        }),

        // -- Subnet --
        // SubnetCreate is handled specially in handle() because it may need to
        // auto-create a default VPC (Composite command).
        OrgRequest::SubnetCreate { .. } => None,
        OrgRequest::SubnetDelete { name, vpc } => {
            // SubnetDelete needs VPC resolution if vpc is None. Handled in handle().
            if vpc.is_some() {
                Some(StateMachineCommand::DeleteSubnet {
                    name: name.clone(),
                    vpc: vpc.clone().unwrap(),
                })
            } else {
                None // Needs VPC resolution — handled in handle().
            }
        }

        // -- Security Group --
        OrgRequest::SgCreate {
            name,
            vpc,
            description: _,
        } => Some(StateMachineCommand::CreateSg {
            name: name.clone(),
            vpc: vpc.clone(),
        }),
        OrgRequest::SgDelete { name, vpc: _ } => {
            Some(StateMachineCommand::DeleteSg { name: name.clone() })
        }
        OrgRequest::SgAttach { sg, vm: _, nic: _ } | OrgRequest::SgDetach { sg, vm: _, nic: _ } => {
            // SG attach/detach needs NIC resolution — handled in handle().
            let _ = sg;
            None
        }
        OrgRequest::SgAddRule {
            sg,
            direction,
            protocol,
            port,
            source,
            ..
        } => Some(StateMachineCommand::AddSgRule {
            sg: sg.clone(),
            direction: direction.clone(),
            protocol: protocol.clone(),
            port: port.clone(),
            source: source.clone().unwrap_or_else(|| "0.0.0.0/0".to_string()),
        }),
        OrgRequest::SgRemoveRule { sg, rule_id } => Some(StateMachineCommand::RemoveSgRule {
            sg: sg.clone(),
            rule_id: rule_id.clone(),
        }),

        // -- NAT Gateway --
        OrgRequest::NatGwCreate { name, vpc, subnet } => Some(StateMachineCommand::CreateNatGw {
            name: name.clone(),
            vpc: vpc.clone(),
            subnet: subnet.clone(),
        }),
        OrgRequest::NatGwDelete { name } => {
            Some(StateMachineCommand::DeleteNatGw { name: name.clone() })
        }

        // -- Route Table --
        OrgRequest::RouteTableCreate { name, vpc } => Some(StateMachineCommand::CreateRouteTable {
            name: name.clone(),
            vpc: vpc.clone(),
        }),
        OrgRequest::RouteTableDelete { name, vpc } => Some(StateMachineCommand::DeleteRouteTable {
            name: name.clone(),
            vpc: vpc.clone(),
        }),
        OrgRequest::RouteTableAssociate { table, subnet } => {
            Some(StateMachineCommand::AssociateRouteTable {
                table: table.clone(),
                subnet: subnet.clone(),
            })
        }
        OrgRequest::RouteTableDisassociate { subnet } => {
            Some(StateMachineCommand::DisassociateRouteTable {
                subnet: subnet.clone(),
            })
        }

        // -- Route --
        OrgRequest::RouteAdd {
            vpc,
            table,
            destination,
            target,
            priority,
        } => Some(StateMachineCommand::AddRoute {
            vpc: vpc.clone(),
            table: table.clone(),
            destination: destination.clone(),
            target: target.clone(),
            priority: *priority,
        }),
        OrgRequest::RouteDelete {
            vpc,
            table,
            destination,
        } => Some(StateMachineCommand::DeleteRoute {
            vpc: vpc.clone(),
            table: table.clone(),
            destination: destination.clone(),
        }),

        // Reads — should not reach here, but be safe.
        _ => None,
    }
}

#[async_trait::async_trait]
impl LayerHandler for RaftOrgHandler {
    async fn handle(&self, request: Vec<u8>, caller_uid: Option<u32>) -> Vec<u8> {
        let req: OrgRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(e) => {
                let resp = OrgResponse::Error(format!("invalid org request: {e}"));
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
        };

        // Read requests always go directly to local store.
        if is_read_request(&req) {
            return self.inner.handle(request, caller_uid).await;
        }

        // Check if Raft is available.
        let raft_client = self.raft_client.read().await;
        let client = match raft_client.as_ref() {
            Some(c) => c,
            None => {
                // No Raft — direct write (backward compatible).
                debug!("raft not available, falling back to direct write");
                let result = self.inner.handle(request, caller_uid).await;

                // Post-write side effect: wire or tear down VPC peering data plane.
                // This mirrors the post-Raft side effect below but for the fallback path.
                wire_peering_data_plane(&req, &result, &self.org_store, &self.network_backend)
                    .await;

                return result;
            }
        };

        // Handle complex operations that need pre-resolution or composite commands.
        match &req {
            OrgRequest::SubnetCreate {
                name,
                env,
                project,
                org,
                vpc,
                cidr,
            } => {
                // For subnet creation, we may need to auto-create a default VPC.
                let vpc_name = match vpc {
                    Some(v) => v.clone(),
                    None => format!("{org}-{project}-default"),
                };
                let env_id = format!("{org}/{project}/{env}");
                let cmd = StateMachineCommand::CreateSubnet {
                    name: name.clone(),
                    vpc: vpc_name,
                    env_id,
                    cidr: cidr.clone(),
                };
                return submit_raft_command(client, &req, cmd, &self.org_store).await;
            }
            OrgRequest::SubnetDelete { name, vpc } if vpc.is_none() => {
                // Need to resolve VPC from store.
                let matches = match self.org_store.find_subnets_by_name(name) {
                    Ok(m) => m,
                    Err(e) => {
                        let resp = OrgResponse::Error(e.to_string());
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                };
                let vpc_name = match matches.len() {
                    0 => {
                        let resp = OrgResponse::Error(format!("subnet '{name}' not found"));
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                    1 => matches.into_iter().next().unwrap().0,
                    _ => {
                        let resp = OrgResponse::Error(format!(
                            "subnet '{name}' exists in multiple VPCs — specify --vpc"
                        ));
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                };
                let cmd = StateMachineCommand::DeleteSubnet {
                    name: name.clone(),
                    vpc: vpc_name,
                };
                return submit_raft_command(client, &req, cmd, &self.org_store).await;
            }
            OrgRequest::SgAttach { sg, vm, nic } => {
                // Resolve SG and NIC for the Raft command.
                let sg_record = match self.org_store.find_sg_by_name(sg) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        let resp = OrgResponse::Error(format!("security group not found: {sg}"));
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                    Err(e) => {
                        let resp = OrgResponse::Error(e.to_string());
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                };
                let sg_key = format!("{}/{}", sg_record.vpc_id.0, sg_record.name);
                let nic_id = match resolve_nic(&self.org_store, vm.as_deref(), nic.as_deref()) {
                    Ok(id) => id,
                    Err(e) => {
                        let resp = OrgResponse::Error(e);
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                };
                let cmd = StateMachineCommand::AttachSg { sg: sg_key, nic_id };
                return submit_raft_command(client, &req, cmd, &self.org_store).await;
            }
            OrgRequest::SgDetach { sg, vm, nic } => {
                let sg_record = match self.org_store.find_sg_by_name(sg) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        let resp = OrgResponse::Error(format!("security group not found: {sg}"));
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                    Err(e) => {
                        let resp = OrgResponse::Error(e.to_string());
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                };
                let sg_key = format!("{}/{}", sg_record.vpc_id.0, sg_record.name);
                let nic_id = match resolve_nic(&self.org_store, vm.as_deref(), nic.as_deref()) {
                    Ok(id) => id,
                    Err(e) => {
                        let resp = OrgResponse::Error(e);
                        return serde_json::to_vec(&resp).unwrap_or_default();
                    }
                };
                let cmd = StateMachineCommand::DetachSg { sg: sg_key, nic_id };
                return submit_raft_command(client, &req, cmd, &self.org_store).await;
            }
            _ => {}
        }

        // Standard path: convert to Raft command and submit.
        match to_raft_command(&req) {
            Some(cmd) => {
                let result = submit_raft_command(client, &req, cmd, &self.org_store).await;

                // Post-Raft side effect: wire or tear down VPC peering data plane.
                wire_peering_data_plane(&req, &result, &self.org_store, &self.network_backend)
                    .await;

                result
            }
            None => {
                // No Raft mapping — fall through to direct handling.
                debug!("no raft mapping for request, using direct handler");
                self.inner.handle(request, caller_uid).await
            }
        }
    }
}

/// Wire or tear down VPC peering data plane after a successful peer/unpeer operation.
///
/// Called from BOTH the Raft path and the direct-write fallback path so that the
/// data plane is always wired regardless of which code path handles the mutation.
async fn wire_peering_data_plane(
    req: &OrgRequest,
    result: &[u8],
    org_store: &OrgStore,
    network_backend: &RwLock<Option<Arc<dyn syfrah_overlay::NetworkBackend>>>,
) {
    match req {
        OrgRequest::VpcPeer { from, to } => {
            // Only wire the data plane when the store write succeeded.
            let resp: OrgResponse = serde_json::from_slice(result)
                .unwrap_or(OrgResponse::Error("deserialization failed".to_string()));
            if !matches!(resp, OrgResponse::Error(_)) {
                // Resolve VPC names to bridge names.
                let vpc_a = org_store.get_vpc(from);
                let vpc_b = org_store.get_vpc(to);
                if let (Ok(Some(va)), Ok(Some(vb))) = (vpc_a, vpc_b) {
                    let bridge_a = syfrah_overlay::naming::bridge_name(&va.id.0);
                    let bridge_b = syfrah_overlay::naming::bridge_name(&vb.id.0);
                    let peering_id = format!("{}-{}", va.id.0, vb.id.0);
                    let backend_guard = network_backend.read().await;
                    if let Some(ref backend) = *backend_guard {
                        // Only wire data plane when BOTH bridges exist locally.
                        // If only one exists, the veth would be created but one end
                        // cannot attach — resulting in a dangling interface. The
                        // reconcile loop will wire the peering once the second bridge
                        // appears.
                        let has_a = backend.link_exists(&bridge_a).await;
                        let has_b = backend.link_exists(&bridge_b).await;
                        if has_a && has_b {
                            if let Err(e) = syfrah_overlay::veth_peer::create_veth_peer(
                                backend.as_ref(),
                                &peering_id,
                                &bridge_a,
                                &bridge_b,
                            )
                            .await
                            {
                                tracing::warn!(
                                    "peering data plane wiring failed for \
                                     {from}<->{to}: {e}"
                                );
                            } else {
                                tracing::info!(
                                    "peering data plane wired: \
                                     {from}<->{to} ({bridge_a}<->{bridge_b})"
                                );
                            }
                        } else {
                            tracing::debug!(
                                "skipping peering data plane for {from}<->{to}: \
                                 need both bridges (has_a={has_a}, has_b={has_b}); \
                                 reconcile loop will wire when ready"
                            );
                        }
                    }
                }
            }
        }
        OrgRequest::VpcUnpeer { from, to } => {
            // Tear down the data plane when the store write succeeded.
            let resp: OrgResponse = serde_json::from_slice(result)
                .unwrap_or(OrgResponse::Error("deserialization failed".to_string()));
            if !matches!(resp, OrgResponse::Error(_)) {
                let vpc_a = org_store.get_vpc(from);
                let vpc_b = org_store.get_vpc(to);
                if let (Ok(Some(va)), Ok(Some(vb))) = (vpc_a, vpc_b) {
                    let bridge_a = syfrah_overlay::naming::bridge_name(&va.id.0);
                    let bridge_b = syfrah_overlay::naming::bridge_name(&vb.id.0);
                    let peering_id = format!("{}-{}", va.id.0, vb.id.0);
                    let backend_guard = network_backend.read().await;
                    if let Some(ref backend) = *backend_guard {
                        if let Err(e) = syfrah_overlay::veth_peer::delete_veth_peer(
                            backend.as_ref(),
                            &peering_id,
                            &bridge_a,
                            &bridge_b,
                        )
                        .await
                        {
                            tracing::warn!(
                                "peering data plane teardown failed for \
                                 {from}<->{to}: {e}"
                            );
                        } else {
                            tracing::info!(
                                "peering data plane torn down: \
                                 {from}<->{to} ({bridge_a}<->{bridge_b})"
                            );
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Resolve a NIC ID from VM name or NIC name.
fn resolve_nic(store: &OrgStore, vm: Option<&str>, nic: Option<&str>) -> Result<String, String> {
    match (vm, nic) {
        (Some(vm_name), _) => match store.find_nic_by_vm(vm_name) {
            Ok(Some(n)) => Ok(n.id.0),
            Ok(None) => Err(format!("VM '{vm_name}' has no NIC")),
            Err(e) => Err(e.to_string()),
        },
        (None, Some(nic_id)) => Ok(nic_id.to_string()),
        (None, None) => Err("specify --vm or --nic".to_string()),
    }
}

/// Submit a command to Raft. On success, read the entity back from local redb
/// for operations where the CLI expects an entity response.
async fn submit_raft_command(
    client: &RaftClient,
    req: &OrgRequest,
    cmd: StateMachineCommand,
    store: &OrgStore,
) -> Vec<u8> {
    match client.write(cmd).await {
        Ok(sm_resp) => match sm_resp {
            StateMachineResponse::Error(msg) => {
                let resp = OrgResponse::Error(msg);
                serde_json::to_vec(&resp).unwrap_or_default()
            }
            _ => raft_response_to_bytes(req, &sm_resp, store),
        },
        Err(e) => {
            let resp = OrgResponse::Error(format!("raft error: {e}"));
            serde_json::to_vec(&resp).unwrap_or_default()
        }
    }
}

/// Convert a successful Raft response to OrgResponse bytes.
///
/// For create mutations where the CLI expects an entity, we read back from
/// local redb (which was updated by the state machine apply). For deletes and
/// other mutations, we return OrgResponse::Ok.
fn raft_response_to_bytes(
    req: &OrgRequest,
    sm_resp: &StateMachineResponse,
    store: &OrgStore,
) -> Vec<u8> {
    let resp = match req {
        OrgRequest::OrgCreate { name } => {
            let org = syfrah_org::types::Org {
                id: syfrah_org::types::OrgId(name.clone()),
                name: name.clone(),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            OrgResponse::Org(org)
        }
        OrgRequest::ProjectCreate { name, org } => {
            let project = syfrah_org::types::Project {
                id: syfrah_org::types::ProjectId(format!("{org}/{name}")),
                name: name.clone(),
                org_id: syfrah_org::types::OrgId(org.clone()),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            OrgResponse::Project(project)
        }
        OrgRequest::EnvCreate {
            name,
            project,
            org,
            ttl,
            deletion_protection,
            labels,
        } => {
            let env = syfrah_org::types::Environment {
                id: syfrah_org::types::EnvironmentId(format!("{org}/{project}/{name}")),
                name: name.clone(),
                project_id: syfrah_org::types::ProjectId(format!("{org}/{project}")),
                ttl: *ttl,
                deletion_protection: *deletion_protection,
                labels: labels.clone(),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                expires_at: ttl.map(|t| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        + t
                }),
            };
            OrgResponse::Env(env)
        }
        // Read back created entities from local redb.
        OrgRequest::VpcCreate { name, .. } => match store.get_vpc(name) {
            Ok(Some(vpc)) => OrgResponse::Vpc(vpc),
            _ => OrgResponse::Ok,
        },
        OrgRequest::SubnetCreate {
            name,
            vpc,
            org,
            project,
            ..
        } => {
            let vpc_name = match vpc {
                Some(v) => v.clone(),
                None => format!("{org}-{project}-default"),
            };
            match store.get_subnet(&vpc_name, name) {
                Ok(subnet) => OrgResponse::Subnet(subnet),
                _ => OrgResponse::Ok,
            }
        }
        OrgRequest::SgCreate { name, .. } => match store.find_sg_by_name(name) {
            Ok(Some(sg)) => OrgResponse::Sg(sg),
            _ => OrgResponse::Ok,
        },
        OrgRequest::NatGwCreate { name, .. } => match store.get_nat_gw_by_name(name) {
            Ok(Some(gw)) => OrgResponse::NatGwResp(gw),
            _ => OrgResponse::Ok,
        },
        OrgRequest::SgAddRule { .. } => {
            // SG rules return Ok — the CLI prints the rule from the command itself.
            OrgResponse::Ok
        }
        OrgRequest::RouteTableCreate { name, vpc } => match store.get_vpc(vpc) {
            Ok(Some(v)) => match store.get_route_table(&v.id, name) {
                Ok(Some(table)) => OrgResponse::RouteTableResp(table),
                _ => OrgResponse::Ok,
            },
            _ => OrgResponse::Ok,
        },
        OrgRequest::RouteAdd {
            vpc,
            table,
            destination,
            ..
        } => {
            // Read back the created route.
            match store.get_vpc(vpc) {
                Ok(Some(v)) => {
                    let table_name = table.as_deref().unwrap_or("default");
                    match store.get_route_table(&v.id, table_name) {
                        Ok(Some(rt)) => match store.get_route(&rt.id, destination) {
                            Ok(Some(route)) => OrgResponse::RouteResp(route),
                            _ => OrgResponse::Ok,
                        },
                        _ => OrgResponse::Ok,
                    }
                }
                _ => OrgResponse::Ok,
            }
        }
        OrgRequest::EnvExtend {
            name, project, org, ..
        }
        | OrgRequest::EnvUpdate {
            name, project, org, ..
        } => {
            // Read back the updated env.
            match store.get_env(org, project, name) {
                Ok(env) => OrgResponse::Env(env),
                _ => OrgResponse::Ok,
            }
        }
        // All other mutations return Ok.
        _ => OrgResponse::Ok,
    };
    let _ = sm_resp; // suppress unused warning
    serde_json::to_vec(&resp).unwrap_or_default()
}
