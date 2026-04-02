//! Raft-aware hypervisor handler — routes ALL mutations through Raft.
//!
//! Architecture:
//! - Mutation requests (Register, Enable, Drain, etc.) → convert to `StateMachineCommand`
//!   → submit to Raft → state machine applies to HypervisorStore (redb) on every node.
//! - Read requests (List, Get, Status, Capacity) → served directly from local redb.
//! - Fallback: if Raft is not initialized, pass through to inner handler (direct writes).

use std::sync::Arc;

use syfrah_api::handler::LayerHandler;
use syfrah_controlplane::commands::{StateMachineCommand, StateMachineResponse};
use syfrah_controlplane::RaftClient;
use syfrah_org::hypervisor_handler::{HypervisorRequest, HypervisorResponse};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Hypervisor layer handler that routes mutations through Raft when available.
pub struct RaftHypervisorHandler {
    /// The inner handler for direct reads and fallback writes.
    inner: Arc<dyn LayerHandler>,
    /// Optional Raft client — set when controlplane is initialized.
    raft_client: RwLock<Option<RaftClient>>,
}

impl RaftHypervisorHandler {
    /// Create a new Raft-aware hypervisor handler.
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

/// Check if a hypervisor request is a read.
fn is_read_request(req: &HypervisorRequest) -> bool {
    matches!(
        req,
        HypervisorRequest::List { .. }
            | HypervisorRequest::Get { .. }
            | HypervisorRequest::Status
            | HypervisorRequest::Capacity
    )
}

/// Convert a hypervisor mutation request to a state machine command.
fn to_raft_command(req: &HypervisorRequest) -> Option<StateMachineCommand> {
    match req {
        HypervisorRequest::Register { region: _, zone: _ } => {
            // Handled specially in handle() — needs fabric state for node name + IPv6.
            None
        }
        HypervisorRequest::Enable { name } => {
            Some(StateMachineCommand::EnableHypervisor { name: name.clone() })
        }
        HypervisorRequest::Drain { name, force: _ } => {
            Some(StateMachineCommand::DrainHypervisor { name: name.clone() })
        }
        HypervisorRequest::Decommission { name } => {
            Some(StateMachineCommand::DecommissionHypervisor { name: name.clone() })
        }
        HypervisorRequest::LabelSet { name, labels } => {
            Some(StateMachineCommand::UpdateHypervisorLabels {
                name: name.clone(),
                labels: labels.iter().cloned().collect(),
            })
        }
        HypervisorRequest::TaintAdd { name, taints } => {
            Some(StateMachineCommand::UpdateHypervisorTaints {
                name: name.clone(),
                taints: taints.clone(),
            })
        }
        HypervisorRequest::Activate { name } => {
            Some(StateMachineCommand::EnableHypervisor { name: name.clone() })
        }
        // These need complex pre-resolution or are reads — handled in handle() or passed through.
        _ => None,
    }
}

#[async_trait::async_trait]
impl LayerHandler for RaftHypervisorHandler {
    async fn handle(&self, request: Vec<u8>, caller_uid: Option<u32>) -> Vec<u8> {
        let req: HypervisorRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(e) => {
                let resp = HypervisorResponse::Error(format!("invalid hypervisor request: {e}"));
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
                debug!("raft hypervisor: no raft, falling back to direct write");
                return self.inner.handle(request, caller_uid).await;
            }
        };

        // Handle Register specially — needs local node name from inner handler context.
        // The inner handler resolves the local node name, but for Raft we need to
        // create the command ourselves. We extract it from the request and use the
        // inner handler's response to get the node name. Actually, the Register request
        // always registers the LOCAL node, so we get the node name from the inner handler
        // first (which does the local redb write), then replicate via Raft.
        //
        // Better approach: the CLI sends Register with region/zone. We route this through
        // Raft with the node name determined from fabric state.
        if let HypervisorRequest::Register { region, zone } = &req {
            // Get local node name and fabric IPv6 from fabric state.
            let state = match crate::store::load() {
                Ok(s) => s,
                Err(e) => {
                    let resp =
                        HypervisorResponse::Error(format!("failed to load fabric state: {e}"));
                    return serde_json::to_vec(&resp).unwrap_or_default();
                }
            };
            let node_name = state.node_name;
            let fabric_ipv6 = state.mesh_ipv6.to_string();

            let cmd = StateMachineCommand::RegisterHypervisor {
                name: node_name.clone(),
                region: region.clone(),
                zone: zone.clone(),
                fabric_ipv6,
            };

            match client.write(cmd).await {
                Ok(StateMachineResponse::Error(msg)) => {
                    let resp = HypervisorResponse::Error(msg);
                    return serde_json::to_vec(&resp).unwrap_or_default();
                }
                Ok(_) => {
                    info!(
                        "raft hypervisor: registered '{}' via Raft (region={}, zone={})",
                        node_name, region, zone
                    );
                    let resp = HypervisorResponse::Ok;
                    return serde_json::to_vec(&resp).unwrap_or_default();
                }
                Err(e) => {
                    let resp = HypervisorResponse::Error(format!("raft error: {e}"));
                    return serde_json::to_vec(&resp).unwrap_or_default();
                }
            }
        }

        // Standard path: convert to Raft command and submit.
        match to_raft_command(&req) {
            Some(cmd) => match client.write(cmd).await {
                Ok(StateMachineResponse::Error(msg)) => {
                    let resp = HypervisorResponse::Error(msg);
                    serde_json::to_vec(&resp).unwrap_or_default()
                }
                Ok(_) => {
                    let resp = HypervisorResponse::Ok;
                    serde_json::to_vec(&resp).unwrap_or_default()
                }
                Err(e) => {
                    let resp = HypervisorResponse::Error(format!("raft error: {e}"));
                    serde_json::to_vec(&resp).unwrap_or_default()
                }
            },
            None => {
                // No Raft mapping — fall through to direct handling.
                debug!("raft hypervisor: no raft mapping, using direct handler");
                self.inner.handle(request, caller_uid).await
            }
        }
    }
}
