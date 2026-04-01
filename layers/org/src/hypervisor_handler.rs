//! Control socket handler for hypervisor operations.
//!
//! Follows the LayerHandler pattern: typed Request/Response enums,
//! serialized as JSON over the daemon's Unix domain socket.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use syfrah_api::{LayerRequest, LayerResponse};
use tokio::net::UnixStream;

use crate::hypervisor::HypervisorStore;
use crate::types::{Hypervisor, HypervisorState};

// ---------------------------------------------------------------------------
// Request / Response enums
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub enum HypervisorRequest {
    List {
        region: Option<String>,
        zone: Option<String>,
    },
    Get {
        name: String,
    },
    Register {
        region: String,
        zone: String,
    },
    Enable {
        name: String,
    },
    /// Status of the local (this node's) hypervisor.
    Status,
    /// Capacity of the local hypervisor.
    Capacity,
    /// Set labels on a hypervisor.
    LabelSet {
        name: String,
        labels: Vec<(String, String)>,
    },
    /// Remove labels from a hypervisor.
    LabelRemove {
        name: String,
        keys: Vec<String>,
    },
    /// Add taints to a hypervisor.
    TaintAdd {
        name: String,
        taints: Vec<String>,
    },
    /// Remove taints from a hypervisor.
    TaintRemove {
        name: String,
        keys: Vec<String>,
    },
    /// Drain a hypervisor.
    Drain {
        name: String,
        force: bool,
    },
    /// Activate a hypervisor (return to Available).
    Activate {
        name: String,
    },
    /// Put a hypervisor into maintenance.
    Maintenance {
        name: String,
        drain: bool,
    },
    /// Decommission a hypervisor.
    Decommission {
        name: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum HypervisorResponse {
    Hypervisor(Box<Hypervisor>),
    HypervisorList(Vec<Hypervisor>),
    Ok,
    Error(String),
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub struct HypervisorLayerHandler {
    store: Arc<HypervisorStore>,
    /// The local node name (to identify "this" hypervisor for status/capacity).
    local_node_name: String,
    /// Optional placement store for VM count checks (deletion guards).
    placement_store: Option<Arc<crate::placement::PlacementStore>>,
}

impl HypervisorLayerHandler {
    pub fn new(store: Arc<HypervisorStore>, local_node_name: String) -> Self {
        Self {
            store,
            local_node_name,
            placement_store: None,
        }
    }

    /// Set the placement store for VM count guard checks.
    pub fn with_placement_store(mut self, store: Arc<crate::placement::PlacementStore>) -> Self {
        self.placement_store = Some(store);
        self
    }

    /// Count VMs on a given hypervisor (by fabric IPv6).
    fn vm_count_on(&self, fabric_ipv6: &str) -> usize {
        self.placement_store
            .as_ref()
            .and_then(|ps| ps.list_by_node(fabric_ipv6).ok())
            .map(|vms| vms.len())
            .unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl LayerHandler for HypervisorLayerHandler {
    async fn handle(&self, request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
        let req: HypervisorRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(e) => {
                let resp = HypervisorResponse::Error(format!("invalid hypervisor request: {e}"));
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
        };

        let resp = self.handle_request(req);
        serde_json::to_vec(&resp).unwrap_or_default()
    }
}

impl HypervisorLayerHandler {
    fn handle_request(&self, req: HypervisorRequest) -> HypervisorResponse {
        match req {
            HypervisorRequest::List { region, zone } => {
                let result = if let Some(r) = region {
                    self.store.list_by_region(&r)
                } else if let Some(z) = zone {
                    self.store.list_by_zone(&z)
                } else {
                    self.store.list()
                };
                match result {
                    Ok(list) => HypervisorResponse::HypervisorList(list),
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::Get { name } => match self.store.get(&name) {
                Ok(Some(hv)) => HypervisorResponse::Hypervisor(Box::new(hv)),
                Ok(None) => HypervisorResponse::Error(format!("hypervisor '{name}' not found")),
                Err(e) => HypervisorResponse::Error(e.to_string()),
            },

            HypervisorRequest::Register { region, zone } => {
                // Manual register: use discovery to create/update the local hypervisor
                // with the specified region/zone.
                match self.store.get(&self.local_node_name) {
                    Ok(Some(mut hv)) => {
                        hv.region = region;
                        hv.zone = zone;
                        match self.store.update(&hv) {
                            Ok(()) => HypervisorResponse::Hypervisor(Box::new(hv)),
                            Err(e) => HypervisorResponse::Error(e.to_string()),
                        }
                    }
                    Ok(None) => {
                        let hw = crate::discovery::probe_hardware();
                        let cap = crate::discovery::compute_capacity(&hw);
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let hv = crate::types::Hypervisor {
                            id: crate::types::HypervisorId(format!("hv-{}", &self.local_node_name)),
                            name: self.local_node_name.clone(),
                            region,
                            zone,
                            state: crate::types::HypervisorState::NotReady,
                            fabric_node_id: self.local_node_name.clone(),
                            public_ip: String::new(),
                            fabric_ipv6: String::new(),
                            hardware: hw,
                            capacity: cap,
                            labels: std::collections::HashMap::new(),
                            taints: Vec::new(),
                            created_at: now,
                        };
                        match self.store.create(&hv) {
                            Ok(()) => HypervisorResponse::Hypervisor(Box::new(hv)),
                            Err(e) => HypervisorResponse::Error(e.to_string()),
                        }
                    }
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::Enable { name } => {
                match self.store.update_state(&name, HypervisorState::Available) {
                    Ok(()) => HypervisorResponse::Ok,
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::Status | HypervisorRequest::Capacity => {
                match self.store.get(&self.local_node_name) {
                    Ok(Some(hv)) => HypervisorResponse::Hypervisor(Box::new(hv)),
                    Ok(None) => HypervisorResponse::Error("no hypervisor on this node".to_string()),
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::LabelSet { name, labels } => match self.store.get(&name) {
                Ok(Some(mut hv)) => {
                    for (k, v) in labels {
                        hv.labels.insert(k, v);
                    }
                    match self.store.update(&hv) {
                        Ok(()) => HypervisorResponse::Ok,
                        Err(e) => HypervisorResponse::Error(e.to_string()),
                    }
                }
                Ok(None) => HypervisorResponse::Error(format!("hypervisor '{name}' not found")),
                Err(e) => HypervisorResponse::Error(e.to_string()),
            },

            HypervisorRequest::LabelRemove { name, keys } => match self.store.get(&name) {
                Ok(Some(mut hv)) => {
                    for k in &keys {
                        hv.labels.remove(k);
                    }
                    match self.store.update(&hv) {
                        Ok(()) => HypervisorResponse::Ok,
                        Err(e) => HypervisorResponse::Error(e.to_string()),
                    }
                }
                Ok(None) => HypervisorResponse::Error(format!("hypervisor '{name}' not found")),
                Err(e) => HypervisorResponse::Error(e.to_string()),
            },

            HypervisorRequest::TaintAdd { name, taints } => match self.store.get(&name) {
                Ok(Some(mut hv)) => {
                    for taint_str in &taints {
                        match parse_taint(taint_str) {
                            Ok(taint) => hv.taints.push(taint),
                            Err(e) => return HypervisorResponse::Error(e),
                        }
                    }
                    match self.store.update(&hv) {
                        Ok(()) => HypervisorResponse::Ok,
                        Err(e) => HypervisorResponse::Error(e.to_string()),
                    }
                }
                Ok(None) => HypervisorResponse::Error(format!("hypervisor '{name}' not found")),
                Err(e) => HypervisorResponse::Error(e.to_string()),
            },

            HypervisorRequest::TaintRemove { name, keys } => match self.store.get(&name) {
                Ok(Some(mut hv)) => {
                    hv.taints.retain(|t| !keys.contains(&t.key));
                    match self.store.update(&hv) {
                        Ok(()) => HypervisorResponse::Ok,
                        Err(e) => HypervisorResponse::Error(e.to_string()),
                    }
                }
                Ok(None) => HypervisorResponse::Error(format!("hypervisor '{name}' not found")),
                Err(e) => HypervisorResponse::Error(e.to_string()),
            },

            HypervisorRequest::Drain { name, force: _ } => {
                match self.store.update_state(&name, HypervisorState::Draining) {
                    Ok(()) => HypervisorResponse::Ok,
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::Activate { name } => {
                match self.store.update_state(&name, HypervisorState::Available) {
                    Ok(()) => HypervisorResponse::Ok,
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::Maintenance { name, drain: _ } => {
                // Guard: must drain first (no running VMs)
                if let Ok(Some(hv)) = self.store.get(&name) {
                    let count = self.vm_count_on(&hv.fabric_ipv6);
                    if count > 0 {
                        return HypervisorResponse::Error(format!(
                            "cannot enter maintenance: {count} VM(s) still running on '{name}'. Drain first."
                        ));
                    }
                }
                match self.store.update_state(&name, HypervisorState::Maintenance) {
                    Ok(()) => HypervisorResponse::Ok,
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }

            HypervisorRequest::Decommission { name } => {
                // Guard: no running VMs
                if let Ok(Some(hv)) = self.store.get(&name) {
                    let count = self.vm_count_on(&hv.fabric_ipv6);
                    if count > 0 {
                        return HypervisorResponse::Error(format!(
                            "cannot decommission: {count} VM(s) still running on '{name}'. Drain first."
                        ));
                    }
                    // Guard: Decommissioned is terminal
                    if hv.state == HypervisorState::Decommissioned {
                        return HypervisorResponse::Error(
                            "hypervisor is already decommissioned".to_string(),
                        );
                    }
                }
                match self
                    .store
                    .update_state(&name, HypervisorState::Decommissioned)
                {
                    Ok(()) => HypervisorResponse::Ok,
                    Err(e) => HypervisorResponse::Error(e.to_string()),
                }
            }
        }
    }
}

/// Parse a taint string like "key=value:NoSchedule" or "key:NoSchedule".
fn parse_taint(s: &str) -> Result<crate::types::Taint, String> {
    let (kv, effect_str) = s.rsplit_once(':').ok_or_else(|| {
        format!("invalid taint format '{s}': expected key=value:Effect or key:Effect")
    })?;

    let effect = match effect_str {
        "NoSchedule" => crate::types::TaintEffect::NoSchedule,
        "NoExecute" => crate::types::TaintEffect::NoExecute,
        _ => {
            return Err(format!(
                "invalid taint effect '{effect_str}': expected NoSchedule or NoExecute"
            ))
        }
    };

    let (key, value) = if let Some((k, v)) = kv.split_once('=') {
        (k.to_string(), Some(v.to_string()))
    } else {
        (kv.to_string(), None)
    };

    Ok(crate::types::Taint { key, value, effect })
}

// ---------------------------------------------------------------------------
// Client helper
// ---------------------------------------------------------------------------

/// Send a hypervisor request to the daemon's control socket.
pub async fn send_hypervisor_request(
    socket_path: &Path,
    req: &HypervisorRequest,
) -> Result<HypervisorResponse, Box<dyn std::error::Error>> {
    let payload = serde_json::to_vec(req)?;
    let envelope = LayerRequest::Hypervisor(payload);

    let mut stream = UnixStream::connect(socket_path).await?;
    syfrah_api::transport::write_message(&mut stream, &envelope).await?;
    let resp: LayerResponse = syfrah_api::transport::read_message(&mut stream).await?;

    match resp {
        LayerResponse::Hypervisor(data) => {
            let hv_resp: HypervisorResponse = serde_json::from_slice(&data)?;
            Ok(hv_resp)
        }
        LayerResponse::UnknownLayer(name) => Err(format!("unknown layer: {name}").into()),
        other => Err(format!("unexpected response variant: {other:?}").into()),
    }
}
