//! Raft-aware org handler — routes mutations through Raft when the control plane is active.
//!
//! Architecture:
//! - Mutation requests → convert to `StateMachineCommand` → submit to Raft →
//!   state machine applies to redb on every node → read back entity from local redb.
//! - Read requests → served directly from local redb.
//! - Fallback: if Raft is not initialized, all requests go to the inner handler (direct writes).

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
fn to_raft_command(req: &OrgRequest) -> Option<StateMachineCommand> {
    match req {
        OrgRequest::OrgCreate { name } => {
            Some(StateMachineCommand::CreateOrg { name: name.clone() })
        }
        OrgRequest::OrgDelete { name } => {
            Some(StateMachineCommand::DeleteOrg { name: name.clone() })
        }
        OrgRequest::ProjectCreate { name, org } => Some(StateMachineCommand::CreateProject {
            name: name.clone(),
            org: org.clone(),
        }),
        OrgRequest::ProjectDelete { name, org } => Some(StateMachineCommand::DeleteProject {
            name: name.clone(),
            org: org.clone(),
        }),
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
        OrgRequest::VpcPeer { from, to } => Some(StateMachineCommand::PeerVpc {
            vpc_a: from.clone(),
            vpc_b: to.clone(),
        }),
        OrgRequest::VpcUnpeer { from, to } => Some(StateMachineCommand::UnpeerVpc {
            vpc_a: from.clone(),
            vpc_b: to.clone(),
        }),
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
        OrgRequest::NatGwDelete { name } => {
            Some(StateMachineCommand::DeleteNatGw { name: name.clone() })
        }
        OrgRequest::RouteAdd {
            vpc,
            destination,
            target,
            ..
        } => Some(StateMachineCommand::AddRoute {
            vpc: vpc.clone(),
            destination: destination.clone(),
            target: target.clone(),
        }),
        OrgRequest::RouteDelete {
            vpc, destination, ..
        } => Some(StateMachineCommand::DeleteRoute {
            vpc: vpc.clone(),
            destination: destination.clone(),
        }),
        // Complex operations that involve multi-step logic (VPC create with auto-VNI,
        // subnet create with default VPC resolution, SG create/delete with VPC lookup,
        // NatGw create, env extend/update, VPC attach/detach) fall through to the
        // inner handler for direct writes. They will be added to Raft in later phases.
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

        // Try to convert to a Raft command.
        match to_raft_command(&req) {
            Some(cmd) => {
                match client.write(cmd).await {
                    Ok(sm_resp) => {
                        match sm_resp {
                            StateMachineResponse::Error(msg) => {
                                let resp = OrgResponse::Error(msg);
                                serde_json::to_vec(&resp).unwrap_or_default()
                            }
                            _ => {
                                // Raft applied successfully. The state machine wrote to redb.
                                // Now dispatch the original request to the inner handler.
                                // The inner handler will try to write again, but since the
                                // data already exists it will return AlreadyExists for creates
                                // and succeed for deletes (idempotent).
                                //
                                // IMPORTANT: We can't just re-run the mutation because
                                // creates would fail with "already exists". Instead, we
                                // return an OrgResponse::Ok for mutations that don't need
                                // to return an entity, or we do a read for those that do.
                                raft_response_to_bytes(&req, &sm_resp)
                            }
                        }
                    }
                    Err(e) => {
                        let resp = OrgResponse::Error(format!("raft error: {e}"));
                        serde_json::to_vec(&resp).unwrap_or_default()
                    }
                }
            }
            None => {
                // No Raft mapping — fall through to direct handling.
                debug!("no raft mapping for request, using direct handler");
                self.inner.handle(request, caller_uid).await
            }
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
            // The CLI expects OrgResponse::Org(org). We construct a minimal Org
            // from the name since the full entity is in redb. The CLI only uses org.name.
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
