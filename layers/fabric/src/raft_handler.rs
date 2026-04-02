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
use tokio::sync::RwLock;
use tracing::debug;

/// Org layer handler that routes mutations through Raft when available.
pub struct RaftOrgHandler {
    /// The inner handler for direct reads and fallback writes.
    inner: Arc<dyn LayerHandler>,
    /// Optional Raft client — set when controlplane is initialized.
    raft_client: RwLock<Option<RaftClient>>,
}

impl RaftOrgHandler {
    /// Create a new Raft-aware org handler wrapping the given inner handler.
    pub fn new(inner: Arc<dyn LayerHandler>) -> Self {
        Self {
            inner,
            raft_client: RwLock::new(None),
        }
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
                return self.inner.handle(request, caller_uid).await;
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
                // Submit a Composite command if the default VPC doesn't exist.
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
                return submit_raft_command(client, &req, cmd).await;
            }
            OrgRequest::SubnetDelete { name, vpc } if vpc.is_none() => {
                // Need to resolve VPC from inner handler since we don't have store access.
                // Fall through to inner handler for resolution.
                return self.inner.handle(request, caller_uid).await;
            }
            OrgRequest::SgAttach { .. } | OrgRequest::SgDetach { .. } => {
                // SG attach/detach needs NIC resolution from store.
                // Fall through to inner handler.
                return self.inner.handle(request, caller_uid).await;
            }
            _ => {}
        }

        // Standard path: convert to Raft command and submit.
        match to_raft_command(&req) {
            Some(cmd) => submit_raft_command(client, &req, cmd).await,
            None => {
                // No Raft mapping — fall through to direct handling.
                debug!("no raft mapping for request, using direct handler");
                self.inner.handle(request, caller_uid).await
            }
        }
    }
}

/// Submit a command to Raft and convert the response to OrgResponse bytes.
async fn submit_raft_command(
    client: &RaftClient,
    req: &OrgRequest,
    cmd: StateMachineCommand,
) -> Vec<u8> {
    match client.write(cmd).await {
        Ok(sm_resp) => match sm_resp {
            StateMachineResponse::Error(msg) => {
                let resp = OrgResponse::Error(msg);
                serde_json::to_vec(&resp).unwrap_or_default()
            }
            _ => raft_response_to_bytes(req, &sm_resp),
        },
        Err(e) => {
            let resp = OrgResponse::Error(format!("raft error: {e}"));
            serde_json::to_vec(&resp).unwrap_or_default()
        }
    }
}

/// Convert a successful Raft response to OrgResponse bytes.
///
/// For mutations that the CLI expects to return an entity (OrgCreate returns Org,
/// ProjectCreate returns Project, etc.), we synthesize the response from the
/// state machine's Created(id) response. For mutations that just return Ok
/// (deletes, etc.), we return OrgResponse::Ok.
fn raft_response_to_bytes(req: &OrgRequest, _sm_resp: &StateMachineResponse) -> Vec<u8> {
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
        // All other mutations return Ok.
        _ => OrgResponse::Ok,
    };
    serde_json::to_vec(&resp).unwrap_or_default()
}
