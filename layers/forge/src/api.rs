use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::capacity::CapacityTracker;
use crate::drain::DrainController;
use crate::generation::GenerationTracker;
use crate::task::TaskStore;

/// Shared application state for all Forge HTTP handlers.
#[derive(Clone)]
pub struct ForgeState {
    /// When the Forge server started (for uptime calculation).
    pub started_at: Instant,
    /// Task store for operation tracking.
    pub task_store: Option<Arc<TaskStore>>,
    /// VmManager for compute operations.
    pub vm_manager: Option<Arc<syfrah_compute::VmManager>>,
    /// Capacity tracker for admission control.
    pub capacity: Option<Arc<CapacityTracker>>,
    /// Org store for subnet/VPC resolution.
    pub org_store: Option<Arc<syfrah_org::OrgStore>>,
    /// Network backend for bridge/VXLAN/TAP/nftables operations.
    pub network_backend: Option<Arc<dyn syfrah_overlay::NetworkBackend>>,
    /// Generation tracker for resource drift detection.
    pub generation_tracker: Option<Arc<GenerationTracker>>,
    /// In-memory NIC registry (resource_id -> NicRecord).
    pub nic_registry: Arc<Mutex<HashMap<String, NicRecord>>>,
    /// In-memory NAT gateway registry (id -> NatGwRecord).
    pub nat_gw_registry: Arc<Mutex<HashMap<String, NatGwRecord>>>,
    /// In-memory FDB entry registry (key -> FdbEntry).
    pub fdb_registry: Arc<Mutex<HashMap<String, FdbEntry>>>,
    /// Drain controller for node drain/undrain protocol.
    pub drain_controller: Option<Arc<DrainController>>,
    /// Prometheus metrics collector.
    pub metrics_collector: Option<Arc<crate::metrics::MetricsCollector>>,
    /// Raft client for leader-forwarding on mutation requests.
    /// When set and this node is NOT the leader, mutation requests are
    /// transparently forwarded to the leader's Forge HTTP API.
    /// Wrapped in RwLock because it's injected after Forge starts (Raft
    /// initialization happens later in the daemon startup sequence).
    pub raft_client: Arc<tokio::sync::RwLock<Option<syfrah_controlplane::RaftClient>>>,
    /// Gossip cluster state for metrics export.
    /// Injected after Raft init (alongside raft_client).
    pub gossip_cluster: Arc<tokio::sync::RwLock<Option<syfrah_controlplane::GossipCluster>>>,
    /// Hypervisor store for scheduler-based placement when zone is specified.
    /// The scheduler reads from this store (Raft-replicated) to pick a
    /// hypervisor in the requested zone.
    pub hypervisor_store: Option<Arc<syfrah_org::HypervisorStore>>,
    /// Storage store for preflight checks (is S3 configured for the target zone?).
    pub storage_store: Option<Arc<syfrah_org::StorageStore>>,
    /// Local node name for scheduler (to identify "this" hypervisor).
    pub local_node_name: String,
    /// Local fabric IPv6 for scheduler.
    pub local_fabric_ipv6: String,
}

/// Stored NIC record for the in-memory registry.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NicRecord {
    pub id: String,
    pub tap_name: String,
    pub vm_id: String,
    pub vpc_id: String,
    pub ip: String,
    pub mac: String,
}

/// Standard error response with FORGE_ prefix codes.
#[derive(Serialize, Deserialize, Debug)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}

/// Health check response.
#[derive(Serialize, Deserialize, Debug)]
pub struct HealthResponse {
    pub status: String,
    pub uptime: u64,
}

/// Query parameters for listing tasks.
#[derive(Deserialize, Debug)]
pub struct TaskListQuery {
    pub resource_id: Option<String>,
}

/// Query parameters for consistency control on GET endpoints.
#[derive(Deserialize, Debug, Default)]
pub struct ConsistencyQuery {
    /// When set to "strong", the read is forwarded to the Raft leader
    /// which performs a ReadIndex check to guarantee linearizability.
    pub consistency: Option<String>,
}

impl ConsistencyQuery {
    /// Returns true if the caller requested strong (linearizable) consistency.
    fn is_strong(&self) -> bool {
        self.consistency
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("strong"))
            .unwrap_or(false)
    }
}

/// Query parameters for direct placement (skips leader forwarding).
#[derive(Deserialize, Debug, Default)]
pub struct DirectPlacementQuery {
    /// When set to "true", the request is processed locally without
    /// leader forwarding. Used by the scheduler when placing VMs on
    /// remote hypervisors — the target node must create locally.
    pub direct: Option<String>,
}

impl DirectPlacementQuery {
    /// Returns true if this is a direct placement (skip leader forwarding).
    fn is_direct(&self) -> bool {
        self.direct
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

/// Request body for creating an instance.
#[derive(Serialize, Deserialize, Debug)]
pub struct CreateInstanceRequest {
    pub name: String,
    pub image: String,
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default)]
    pub subnet: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub ssh_key: Option<String>,
    #[serde(default)]
    pub disk_size_mb: Option<u32>,
    #[serde(default)]
    pub security_groups: Vec<String>,
    /// Placement zone constraint. When specified on the leader, the Forge API
    /// runs the scheduler to place the VM on a hypervisor in the requested zone.
    #[serde(default)]
    pub zone: Option<String>,
    /// Pre-allocated IP from Raft IPAM (set by leader before dispatching to remote).
    /// When set, the target Forge uses this IP instead of allocating locally,
    /// preventing duplicate IP assignments across nodes.
    #[serde(default)]
    pub pre_allocated_ip: Option<String>,
    /// Pre-allocated MAC from Raft IPAM (set by leader before dispatching to remote).
    #[serde(default)]
    pub pre_allocated_mac: Option<String>,
}

fn default_vcpus() -> u32 {
    1
}
fn default_memory() -> u32 {
    512
}

/// Instance action for start/stop/reboot.
#[derive(Deserialize, Debug)]
pub struct InstanceActionRequest {
    pub action: Option<String>,
}

/// Request body for creating/ensuring a bridge.
#[derive(Deserialize, Debug)]
pub struct CreateBridgeRequest {
    /// VPC identifier — used to derive the bridge name via naming conventions.
    pub vpc_id: String,
}

/// Response for bridge operations.
#[derive(Serialize, Deserialize, Debug)]
pub struct BridgeResponse {
    pub id: String,
    pub bridge_name: String,
    pub vpc_id: String,
    pub generation: Option<crate::generation::ResourceGeneration>,
}

/// Request body for creating/ensuring a VXLAN.
#[derive(Deserialize, Debug)]
pub struct CreateVxlanRequest {
    /// VPC identifier — used to derive the VXLAN name via naming conventions.
    pub vpc_id: String,
    /// VXLAN Network Identifier.
    pub vni: u32,
    /// Local VTEP IP address.
    pub local_ip: String,
    /// VXLAN UDP port (default 4789).
    #[serde(default = "default_vxlan_port")]
    pub port: u16,
}

fn default_vxlan_port() -> u16 {
    4789
}

/// Response for VXLAN operations.
#[derive(Serialize, Deserialize, Debug)]
pub struct VxlanResponse {
    pub id: String,
    pub vxlan_name: String,
    pub vpc_id: String,
    pub vni: u32,
    pub generation: Option<crate::generation::ResourceGeneration>,
}

/// Request body for creating a NIC (TAP/veth).
#[derive(Deserialize, Debug)]
pub struct CreateNicRequest {
    /// VM identifier this NIC is attached to.
    pub vm_id: String,
    /// VPC identifier — used to derive bridge name for attachment.
    pub vpc_id: String,
    /// IP address assigned to this NIC.
    pub ip: String,
    /// MAC address for this NIC.
    pub mac: String,
    /// Security groups attached to this NIC.
    #[serde(default)]
    pub security_groups: Vec<String>,
}

/// Request body for applying security group rules to a VM.
#[derive(Deserialize, Debug)]
pub struct ApplySgRequest {
    /// VM identifier.
    pub vm_id: String,
    /// NIC's private IP address.
    pub ip: String,
    /// NIC's MAC address.
    pub mac: String,
    /// Security group names attached to this NIC.
    pub security_groups: Vec<String>,
    /// Security group rules to apply.
    #[serde(default)]
    pub rules: Vec<SgRuleInput>,
    /// Map of SG name -> list of IPs for SG reference resolution.
    #[serde(default)]
    pub sg_ip_map: HashMap<String, Vec<String>>,
    /// Host-side interface name (TAP or veth). Defaults to tap_name(vm_id).
    #[serde(default)]
    pub iface_name: Option<String>,
}

/// Input format for a security group rule.
#[derive(Deserialize, Debug)]
pub struct SgRuleInput {
    pub id: String,
    pub sg_id: String,
    pub direction: String,
    pub protocol: String,
    #[serde(default)]
    pub port_range_start: Option<u16>,
    #[serde(default)]
    pub port_range_end: Option<u16>,
    pub source: String,
    #[serde(default = "default_priority")]
    pub priority: u32,
}

fn default_priority() -> u32 {
    100
}

/// Request body for removing SG rules from a VM.
#[derive(Deserialize, Debug)]
pub struct RemoveSgRequest {
    /// VM identifier whose chains should be flushed.
    pub vm_id: String,
    /// Host-side interface name (TAP or veth). Defaults to tap_name(vm_id).
    #[serde(default)]
    pub iface_name: Option<String>,
}

/// NAT gateway state.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NatGwState {
    Pending,
    Active,
    Deleting,
}

/// Request body for creating a NAT gateway.
#[derive(Deserialize, Debug)]
pub struct CreateNatGwRequest {
    /// Bridge name to apply masquerade on.
    pub bridge: String,
    /// Subnet CIDR for NAT (e.g., "10.1.0.0/24").
    pub subnet_cidr: String,
}

/// Stored NAT gateway record.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NatGwRecord {
    pub id: String,
    pub bridge: String,
    pub subnet_cidr: String,
    pub state: NatGwState,
}

/// Request body for enforcing route table entries.
#[derive(Deserialize, Debug)]
pub struct EnforceRoutesRequest {
    /// Route entries to enforce.
    pub routes: Vec<RouteEntry>,
}

/// A single route entry.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RouteEntry {
    /// Destination CIDR (e.g., "10.2.0.0/24").
    pub destination: String,
    /// Target type: "nat-gw", "peering", "blackhole".
    pub target_type: String,
    /// Target ID (NAT GW id or peering id). Ignored for blackhole.
    #[serde(default)]
    pub target_id: Option<String>,
}

/// Request body for FDB add/remove operations.
#[derive(Deserialize, Debug)]
pub struct FdbRequest {
    /// "add" or "remove".
    pub action: String,
    /// VPC identifier — used to derive bridge name.
    pub vpc_id: String,
    /// VM MAC address.
    pub mac: String,
    /// Remote VTEP IP address (for FDB entries).
    pub vtep: String,
    /// VM IP address (for ARP proxy).
    #[serde(default)]
    pub vm_ip: Option<String>,
    /// VXLAN interface name (for ARP proxy). Derived from vpc_id if not provided.
    #[serde(default)]
    pub vxlan_name: Option<String>,
}

/// Stored FDB entry for the in-memory registry.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FdbEntry {
    pub vpc_id: String,
    pub bridge_name: String,
    pub mac: String,
    pub vtep: String,
    pub vm_ip: Option<String>,
}

/// Response for NIC operations.
#[derive(Serialize, Deserialize, Debug)]
pub struct NicResponse {
    pub id: String,
    pub tap_name: String,
    pub vm_id: String,
    pub vpc_id: String,
    pub ip: String,
    pub mac: String,
    pub generation: Option<crate::generation::ResourceGeneration>,
}

/// Query parameters for listing NICs.
#[derive(Deserialize, Debug)]
pub struct NicListQuery {
    pub vm_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Leader forwarding — transparent Raft leader forwarding for mutations
// ---------------------------------------------------------------------------

/// Check if we should forward this mutation to the leader.
/// Returns `Some(leader_forge_addr)` if we should forward, `None` if we should handle locally.
async fn should_forward_to_leader(state: &ForgeState) -> Option<String> {
    let guard = state.raft_client.read().await;
    let client = guard.as_ref()?;
    if client.is_leader() {
        return None; // We are the leader, process locally
    }
    // Derive Forge address from Raft address (port 7100 instead of 7200).
    let raft_addr = client.leader_addr()?;
    Some(raft_addr.replace(":7200", ":7100"))
}

/// Forward a JSON POST request to the leader's Forge API.
async fn forward_post_to_leader<T: Serialize>(
    leader_addr: &str,
    path: &str,
    body: &T,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    debug!("forwarding POST {} to leader at {}", path, leader_addr);
    let url = format!("http://{leader_addr}{path}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_FORWARD_FAILED",
                    "message": format!("failed to build HTTP client: {e}")
                })),
            )
        })?;

    let resp = client.post(&url).json(body).send().await.map_err(|e| {
        warn!("leader forward failed: {e}");
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "code": "FORGE_LEADER_UNREACHABLE",
                "message": format!("failed to reach leader at {leader_addr}: {e}")
            })),
        )
    })?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| {
        serde_json::json!({"code": "FORGE_FORWARD_PARSE_ERROR", "message": "failed to parse leader response"})
    });

    Ok((
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    ))
}

/// Forward a simple (no-body) request to the leader's Forge API.
async fn forward_simple_to_leader(
    leader_addr: &str,
    method: &str,
    path: &str,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    debug!(
        "forwarding {} {} to leader at {}",
        method, path, leader_addr
    );
    let url = format!("http://{leader_addr}{path}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_FORWARD_FAILED",
                    "message": format!("failed to build HTTP client: {e}")
                })),
            )
        })?;

    let req_builder = match method {
        "DELETE" => client.delete(&url),
        "POST" => client.post(&url),
        _ => client.get(&url),
    };

    let resp = req_builder.send().await.map_err(|e| {
        warn!("leader forward failed: {e}");
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "code": "FORGE_LEADER_UNREACHABLE",
                "message": format!("failed to reach leader at {leader_addr}: {e}")
            })),
        )
    })?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| {
        serde_json::json!({"code": "FORGE_FORWARD_PARSE_ERROR", "message": "failed to parse leader response"})
    });

    Ok((
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    ))
}

/// Get the Raft leader's Forge address if Raft is initialized.
/// Used for strong reads — we always forward to leader regardless of our role.
async fn leader_forge_addr_for_strong_read(state: &ForgeState) -> Option<String> {
    let guard = state.raft_client.read().await;
    let client = guard.as_ref()?;
    let raft_addr = client.leader_addr()?;
    Some(raft_addr.replace(":7200", ":7100"))
}

/// Forward a GET request to the leader for strong reads.
async fn forward_get_to_leader(
    leader_addr: &str,
    path: &str,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    debug!(
        "strong read: forwarding GET {} to leader at {}",
        path, leader_addr
    );
    let url = format!("http://{leader_addr}{path}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_FORWARD_FAILED",
                    "message": format!("failed to build HTTP client: {e}")
                })),
            )
        })?;

    let resp = client.get(&url).send().await.map_err(|e| {
        warn!("strong read forward failed: {e}");
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "code": "FORGE_LEADER_UNREACHABLE",
                "message": format!("failed to reach leader at {leader_addr}: {e}")
            })),
        )
    })?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| {
        serde_json::json!({"code": "FORGE_FORWARD_PARSE_ERROR", "message": "failed to parse leader response"})
    });

    Ok((
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    ))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /v1/hypervisor/health (alias: /v1/node/health)
/// Returns the 4-category health model.
async fn health_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let vm_count = match state.vm_manager.as_ref() {
        Some(m) => m.list().await.len() as u32,
        None => 0,
    };

    let health = crate::health::FourCategoryHealth::evaluate(uptime, vm_count);
    (StatusCode::OK, Json(serde_json::to_value(health).unwrap()))
}

/// GET /v1/hypervisor/status (alias: /v1/node/status)
async fn status_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let vm_count = match state.vm_manager.as_ref() {
        Some(m) => m.list().await.len(),
        None => 0,
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "healthy",
            "uptime": uptime,
            "vm_count": vm_count,
        })),
    )
}

/// GET /v1/hypervisor/capacity (alias: /v1/node/capacity)
/// Returns full breakdown: physical, reserved, overcommit, allocatable, used, available, disk.
async fn capacity_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    if let Some(ref cap) = state.capacity {
        let snap = cap.snapshot();
        (StatusCode::OK, Json(serde_json::to_value(snap).unwrap()))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_CAPACITY_UNAVAILABLE", "message": "capacity tracker not initialized"}),
            ),
        )
    }
}

/// GET /v1/hypervisor/reservations — list active resource reservations.
async fn reservations_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    if let Some(ref cap) = state.capacity {
        let active = cap.list_reservations();
        let entries: Vec<serde_json::Value> = active
            .iter()
            .map(|(id, r)| {
                serde_json::json!({
                    "id": id,
                    "vcpus": r.vcpus,
                    "memory_mb": r.memory_mb,
                    "age_secs": r.created_at.elapsed().as_secs(),
                })
            })
            .collect();
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "reservations": entries,
                "count": entries.len(),
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_CAPACITY_UNAVAILABLE", "message": "capacity tracker not initialized"}),
            ),
        )
    }
}

/// GET /v1/hypervisor/metrics (alias: /v1/node/metrics)
async fn metrics_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let vm_count = match state.vm_manager.as_ref() {
        Some(m) => m.list().await.len(),
        None => 0,
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "uptime_seconds": uptime,
            "vm_count": vm_count,
        })),
    )
}

/// GET /metrics — Prometheus text exposition format.
async fn prometheus_metrics_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let (vm_count, running, stopped) = match state.vm_manager.as_ref() {
        Some(m) => {
            let vms = m.list().await;
            let total = vms.len();
            // Count running vs stopped based on process status.
            // For now, all listed VMs are considered running.
            (total, total, 0usize)
        }
        None => (0, 0, 0),
    };

    let (cpu_ratio, mem_ratio) = match state.capacity.as_ref() {
        Some(cap) => {
            let alloc_v = cap.allocatable_vcpus() as f64;
            let alloc_m = cap.allocatable_memory_mb() as f64;
            let used_v = cap.used_vcpus() as f64;
            let used_m = cap.used_memory_mb() as f64;
            (
                if alloc_v > 0.0 { used_v / alloc_v } else { 0.0 },
                if alloc_m > 0.0 { used_m / alloc_m } else { 0.0 },
            )
        }
        None => (0.0, 0.0),
    };

    let mut body = match state.metrics_collector.as_ref() {
        Some(collector) => collector.render(vm_count, running, stopped, cpu_ratio, mem_ratio),
        None => {
            // Render basic metrics without collector
            let collector = crate::metrics::MetricsCollector::new();
            collector.render(vm_count, running, stopped, cpu_ratio, mem_ratio)
        }
    };

    // Append Raft metrics if the control plane is initialized.
    {
        let raft_guard = state.raft_client.read().await;
        if let Some(ref client) = *raft_guard {
            body.push_str(&render_raft_metrics(client));
        }
    }

    // Append gossip metrics if the gossip cluster is initialized.
    {
        let gossip_guard = state.gossip_cluster.read().await;
        if let Some(ref cluster) = *gossip_guard {
            body.push_str(&render_gossip_metrics(cluster));
        }
    }

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
}

/// Render gossip-specific metrics in Prometheus text exposition format.
fn render_gossip_metrics(cluster: &syfrah_controlplane::GossipCluster) -> String {
    use std::fmt::Write;

    let snapshot = cluster.metrics_snapshot();
    let mut out = String::with_capacity(512);

    let _ = writeln!(
        out,
        "# HELP gossip_members_total Number of gossip members by state"
    );
    let _ = writeln!(out, "# TYPE gossip_members_total gauge");
    let _ = writeln!(
        out,
        "gossip_members_total{{state=\"alive\"}} {}",
        snapshot.members_alive
    );
    let _ = writeln!(
        out,
        "gossip_members_total{{state=\"suspect\"}} {}",
        snapshot.members_suspect
    );
    let _ = writeln!(
        out,
        "gossip_members_total{{state=\"down\"}} {}",
        snapshot.members_down
    );

    let _ = writeln!(
        out,
        "# HELP gossip_messages_sent_total Total gossip messages sent"
    );
    let _ = writeln!(out, "# TYPE gossip_messages_sent_total counter");
    let _ = writeln!(out, "gossip_messages_sent_total {}", snapshot.messages_sent);

    let _ = writeln!(
        out,
        "# HELP gossip_messages_received_total Total gossip messages received"
    );
    let _ = writeln!(out, "# TYPE gossip_messages_received_total counter");
    let _ = writeln!(
        out,
        "gossip_messages_received_total {}",
        snapshot.messages_received
    );

    out
}

/// Render Raft-specific metrics in Prometheus text exposition format.
///
/// Collected from the RaftClient's metrics snapshot:
/// - raft_state gauge (0=follower, 1=candidate, 2=leader)
/// - raft_term gauge
/// - raft_commit_index gauge
/// - raft_last_applied gauge
/// - raft_log_entries gauge (current log size)
/// - raft_snapshot_count counter
fn render_raft_metrics(client: &syfrah_controlplane::RaftClient) -> String {
    use std::fmt::Write;

    let snapshot = client.metrics_snapshot();
    let mut out = String::with_capacity(1024);

    // raft_state: 0=follower, 1=candidate, 2=leader
    let _ = writeln!(
        out,
        "# HELP raft_state Current Raft state (0=follower, 1=candidate, 2=leader)"
    );
    let _ = writeln!(out, "# TYPE raft_state gauge");
    let _ = writeln!(out, "raft_state {}", snapshot.state);

    // raft_term
    let _ = writeln!(out, "# HELP raft_term Current Raft term");
    let _ = writeln!(out, "# TYPE raft_term gauge");
    let _ = writeln!(out, "raft_term {}", snapshot.term);

    // raft_commit_index
    let _ = writeln!(out, "# HELP raft_commit_index Raft commit index");
    let _ = writeln!(out, "# TYPE raft_commit_index gauge");
    let _ = writeln!(out, "raft_commit_index {}", snapshot.commit_index);

    // raft_last_applied
    let _ = writeln!(
        out,
        "# HELP raft_last_applied Index of last applied log entry"
    );
    let _ = writeln!(out, "# TYPE raft_last_applied gauge");
    let _ = writeln!(out, "raft_last_applied {}", snapshot.last_applied);

    // raft_log_entries (current log size)
    let _ = writeln!(out, "# HELP raft_log_entries Current number of log entries");
    let _ = writeln!(out, "# TYPE raft_log_entries gauge");
    let _ = writeln!(out, "raft_log_entries {}", snapshot.log_entries);

    // raft_snapshot_count
    let _ = writeln!(out, "# HELP raft_snapshot_count Number of snapshots taken");
    let _ = writeln!(out, "# TYPE raft_snapshot_count counter");
    let _ = writeln!(out, "raft_snapshot_count {}", snapshot.snapshot_count);

    out
}

/// GET /v1/hypervisor/resources (alias: /v1/node/resources)
async fn resources_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let vms = match state.vm_manager.as_ref() {
        Some(m) => m.list().await,
        None => vec![],
    };

    let vm_names: Vec<String> = vms.iter().map(|v| v.vm_id.0.clone()).collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "vm_count": vms.len(),
            "vms": vm_names,
        })),
    )
}

/// POST /v1/hypervisor/drain — start draining this node.
async fn drain_handler(
    State(state): State<Arc<ForgeState>>,
    body: Option<Json<crate::drain::DrainRequest>>,
) -> impl IntoResponse {
    let Some(ref ctrl) = state.drain_controller else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_DRAIN_UNAVAILABLE", "message": "drain controller not initialized"}),
            ),
        );
    };

    let (force, _timeout) = match body {
        Some(Json(req)) => (
            req.force,
            req.timeout_secs.map(std::time::Duration::from_secs),
        ),
        None => (false, None),
    };

    ctrl.start_drain(force, _timeout);
    info!(force = force, "node drain started");

    let status = ctrl.status();
    (StatusCode::OK, Json(serde_json::to_value(status).unwrap()))
}

/// POST /v1/hypervisor/activate — stop draining and return to Available.
async fn activate_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let Some(ref ctrl) = state.drain_controller else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_DRAIN_UNAVAILABLE", "message": "drain controller not initialized"}),
            ),
        );
    };

    ctrl.activate();
    info!("node activated (drain stopped)");

    (
        StatusCode::OK,
        Json(serde_json::json!({"status": "available", "draining": false})),
    )
}

/// GET /v1/hypervisor/drain — get drain status.
async fn drain_status_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let Some(ref ctrl) = state.drain_controller else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_DRAIN_UNAVAILABLE", "message": "drain controller not initialized"}),
            ),
        );
    };

    let status = ctrl.status();
    (StatusCode::OK, Json(serde_json::to_value(status).unwrap()))
}

/// GET /v1/tasks/:id
async fn get_task_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref task_store) = state.task_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_TASK_STORE_UNAVAILABLE", "message": "task store not initialized"}),
            ),
        );
    };

    match task_store.get_task(&id) {
        Ok(Some(task)) => (StatusCode::OK, Json(serde_json::to_value(task).unwrap())),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::json!({"code": "FORGE_TASK_NOT_FOUND", "message": format!("task {} not found", id)}),
            ),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"code": "FORGE_INTERNAL_ERROR", "message": e.to_string()})),
        ),
    }
}

/// GET /v1/tasks?resource_id=X
async fn list_tasks_handler(
    State(state): State<Arc<ForgeState>>,
    Query(query): Query<TaskListQuery>,
) -> impl IntoResponse {
    let Some(ref task_store) = state.task_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_TASK_STORE_UNAVAILABLE", "message": "task store not initialized"}),
            ),
        );
    };

    match task_store.list_tasks(query.resource_id.as_deref()) {
        Ok(tasks) => (StatusCode::OK, Json(serde_json::to_value(tasks).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"code": "FORGE_INTERNAL_ERROR", "message": e.to_string()})),
        ),
    }
}

/// POST /v1/instances — create a new VM instance.
async fn create_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Query(params): Query<DirectPlacementQuery>,
    Json(req): Json<CreateInstanceRequest>,
) -> impl IntoResponse {
    // Leader forwarding: if Raft is active and we are NOT the leader,
    // forward the entire request to the leader's Forge API.
    // Skip forwarding if this is a direct placement from the scheduler.
    if !params.is_direct() {
        if let Some(leader_addr) = should_forward_to_leader(&state).await {
            debug!("create_instance: not leader, forwarding to leader at {leader_addr}");
            match forward_post_to_leader(&leader_addr, "/v1/instances", &req).await {
                Ok(resp) => return resp,
                Err(resp) => return resp,
            }
        }
    }

    // Scheduler routing: if zone is specified and we have a hypervisor store,
    // run the scheduler to pick a hypervisor in the requested zone. If the
    // selected hypervisor is not this node, forward to the target.
    if let Some(ref zone) = req.zone {
        if let Some(ref hv_store) = state.hypervisor_store {
            let scheduler = syfrah_controlplane::Scheduler::new(
                state.local_node_name.clone(),
                state.local_fabric_ipv6.clone(),
            );
            let constraints = syfrah_controlplane::PlacementConstraints::from_cli(
                Some(zone.clone()),
                &[],
                None,
                None,
            );
            let existing: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
            // Build the set of zones with storage configured for preflight checks.
            let storage_zones: Option<std::collections::HashSet<String>> =
                state.storage_store.as_ref().and_then(|ss| {
                    ss.list_storage_configs()
                        .ok()
                        .map(|cfgs| cfgs.into_iter().map(|(z, _)| z).collect())
                });
            match scheduler.schedule_from_store_with_storage(
                req.vcpus,
                req.memory_mb as u64,
                &constraints,
                hv_store,
                &[],
                &existing,
                storage_zones.as_ref(),
            ) {
                Ok(decision) => {
                    if !decision.is_local_fallback
                        && decision.hypervisor_id != state.local_node_name
                    {
                        // Forward to the target hypervisor with ?direct=true.
                        let forge_addr = syfrah_controlplane::forge_addr_from_fabric_ipv6(
                            &decision.hypervisor_addr,
                        );
                        let remote_req = syfrah_controlplane::RemoteCreateVmRequest {
                            name: req.name.clone(),
                            image: req.image.clone(),
                            vcpus: req.vcpus,
                            memory_mb: req.memory_mb,
                            subnet: req.subnet.clone(),
                            project: req.project.clone(),
                            org: req.org.clone(),
                            ssh_key: req.ssh_key.clone(),
                            disk_size_mb: req.disk_size_mb,
                            security_groups: req.security_groups.clone(),
                            zone: None, // Don't forward zone to target — it creates locally.
                            pre_allocated_ip: None,
                            pre_allocated_mac: None,
                        };
                        match syfrah_controlplane::create_vm_on_remote(&forge_addr, &remote_req)
                            .await
                        {
                            Ok(resp) if resp.success => {
                                info!(
                                    "scheduler placed VM '{}' on '{}' (zone={})",
                                    req.name, decision.hypervisor_id, zone
                                );
                                return (
                                    StatusCode::CREATED,
                                    Json(serde_json::json!({
                                        "id": resp.vm_id.unwrap_or_else(|| req.name.clone()),
                                        "name": req.name,
                                        "image": req.image,
                                        "vcpus": req.vcpus,
                                        "memory_mb": req.memory_mb,
                                        "ip": resp.ip,
                                        "hypervisor": decision.hypervisor_id,
                                        "zone": zone,
                                        "status": "Created",
                                    })),
                                );
                            }
                            Ok(resp) => {
                                let msg = resp.error.unwrap_or_else(|| "unknown error".to_string());
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    Json(serde_json::json!({
                                        "code": "FORGE_CREATE_FAILED",
                                        "message": format!(
                                            "scheduler placed VM on '{}' but creation failed: {}",
                                            decision.hypervisor_id, msg
                                        ),
                                    })),
                                );
                            }
                            Err(e) => {
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    Json(serde_json::json!({
                                        "code": "FORGE_CREATE_FAILED",
                                        "message": format!(
                                            "scheduler placed VM on '{}' but unreachable: {}",
                                            decision.hypervisor_id, e
                                        ),
                                    })),
                                );
                            }
                        }
                    }
                    // If local fallback or this node was selected, continue to create locally.
                    info!(
                        "scheduler selected local node for VM '{}' (zone={})",
                        req.name, zone
                    );
                }
                Err(e) => {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "code": "FORGE_SCHEDULER_ERROR",
                            "message": format!("scheduler error: {e}"),
                        })),
                    );
                }
            }
        }
    }

    // Drain check: reject new creates when draining.
    if let Some(ref drain) = state.drain_controller {
        if drain.is_draining() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "code": "FORGE_NODE_DRAINING",
                    "message": "node is draining — new VM creation denied"
                })),
            );
        }
    }

    // Admission control: atomic check-and-reserve to prevent double-booking
    // under concurrent creates. The reservation expires after 60s if creation
    // doesn't complete. On success, reservation is converted to allocation.
    if let Some(ref capacity) = state.capacity {
        if !capacity.try_reserve(&req.name, req.vcpus, req.memory_mb as u64) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "code": "FORGE_INSUFFICIENT_CAPACITY",
                    "message": format!(
                        "insufficient capacity: requested {} vCPUs, {} MB memory",
                        req.vcpus, req.memory_mb
                    )
                })),
            );
        }
    }

    let Some(ref vm_manager) = state.vm_manager else {
        // Release any reservation made above.
        if let Some(ref capacity) = state.capacity {
            capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
        }
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Create task record.
    let task_id = format!("task-{}", uuid::Uuid::new_v4());
    if let Some(ref task_store) = state.task_store {
        let _ = task_store.create_task(&task_id, &req.name, "create_instance");
        let _ = task_store.start_task(&task_id);
    }

    // Resolve subnet if specified, with retry for Raft replication lag.
    let subnet_info = if let Some(ref subnet_name) = req.subnet {
        if let Some(ref org_store) = state.org_store {
            let mut resolved = None;
            let mut last_err = None;
            for attempt in 0..5u32 {
                match org_store.find_subnets_by_name(subnet_name) {
                    Ok(matches) if !matches.is_empty() => {
                        let (_vpc_name, subnet) = &matches[0];
                        resolved = Some(syfrah_compute::types::SubnetInfo {
                            name: subnet.name.clone(),
                            cidr: subnet.cidr.clone(),
                            gateway: subnet.gateway.clone(),
                            vpc_id: subnet.vpc_id.0.clone(),
                            env_id: subnet.env_id.0.clone(),
                        });
                        break;
                    }
                    Ok(_) => {
                        if attempt < 4 {
                            warn!(
                                subnet = %subnet_name,
                                attempt = attempt + 1,
                                "subnet not found — Raft may not have replicated yet. Retrying..."
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                    }
                    Err(e) => {
                        last_err = Some(e);
                        break;
                    }
                }
            }
            if let Some(err) = last_err {
                if let Some(ref capacity) = state.capacity {
                    capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
                }
                if let Some(ref task_store) = state.task_store {
                    let _ = task_store.fail_task(&task_id, &err.to_string());
                }
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "code": "FORGE_SUBNET_RESOLVE_FAILED",
                        "message": err.to_string(),
                        "task_id": task_id,
                    })),
                );
            }
            if resolved.is_none() {
                // Release capacity reservation on error.
                if let Some(ref capacity) = state.capacity {
                    capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
                }
                if let Some(ref task_store) = state.task_store {
                    let _ = task_store.fail_task(&task_id, "subnet not found");
                }
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "code": "FORGE_SUBNET_NOT_FOUND",
                        "message": format!("subnet '{}' not found after 5 retries", subnet_name),
                        "task_id": task_id,
                    })),
                );
            }
            resolved
        } else {
            warn!("forge: subnet requested but org store not available");
            None
        }
    } else {
        None
    };

    let vm_id = syfrah_compute::VmId::generate();
    let root_volume_id = Some(format!("vol-root-{}", &vm_id.0));
    let spec = syfrah_compute::VmSpec {
        id: vm_id,
        name: req.name.clone(),
        vcpus: req.vcpus,
        memory_mb: req.memory_mb,
        image: req.image,
        kernel: None,
        network: None,
        volumes: vec![],
        gpu: syfrah_compute::GpuMode::None,
        ssh_key: req.ssh_key,
        disk_size_mb: req.disk_size_mb,
        subnet: subnet_info,
        security_groups: if req.security_groups.is_empty() {
            vec!["default".to_string()]
        } else {
            req.security_groups
        },
        pre_allocated_ip: req.pre_allocated_ip,
        pre_allocated_mac: req.pre_allocated_mac,
        root_volume_id,
    };

    match vm_manager.create_vm(spec).await {
        Ok(status) => {
            if let Some(ref capacity) = state.capacity {
                capacity.commit(&req.name, req.vcpus, req.memory_mb as u64);
                // Immediate Raft capacity update so the scheduler sees increased
                // utilization right away (not just at the next 10s tick).
                if let Some(raft_client) = state.raft_client.read().await.as_ref() {
                    let cmd = syfrah_controlplane::StateMachineCommand::UpdateHypervisorCapacity {
                        name: state.local_node_name.clone(),
                        allocatable_vcpus: capacity.allocatable_vcpus(),
                        allocatable_memory_mb: capacity.allocatable_memory_mb(),
                        used_vcpus: capacity.used_vcpus(),
                        used_memory_mb: capacity.used_memory_mb(),
                    };
                    if let Err(e) = raft_client.write(cmd).await {
                        debug!("immediate capacity update after create failed: {e}");
                    }
                }
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.complete_task(&task_id);
            }
            (
                StatusCode::CREATED,
                Json(serde_json::to_value(status).unwrap()),
            )
        }
        Err(e) => {
            if let Some(ref capacity) = state.capacity {
                capacity.release(&req.name, req.vcpus, req.memory_mb as u64);
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.fail_task(&task_id, &e.to_string());
            }
            warn!("forge: create instance failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_CREATE_FAILED",
                    "message": e.to_string(),
                    "task_id": task_id,
                })),
            )
        }
    }
}

/// GET /v1/instances — list all instances.
/// Supports `?consistency=strong` for linearizable reads via the leader.
async fn list_instances_handler(
    State(state): State<Arc<ForgeState>>,
    Query(params): Query<ConsistencyQuery>,
) -> impl IntoResponse {
    // Strong consistency: forward to leader for linearizable read.
    if params.is_strong() {
        if let Some(leader_addr) = leader_forge_addr_for_strong_read(&state).await {
            // If we ARE the leader, serve locally (no forwarding needed).
            let is_leader = {
                let guard = state.raft_client.read().await;
                guard.as_ref().map(|c| c.is_leader()).unwrap_or(false)
            };
            if !is_leader {
                match forward_get_to_leader(&leader_addr, "/v1/instances").await {
                    Ok(resp) => return resp,
                    Err(resp) => return resp,
                }
            }
        }
    }

    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    let vms = vm_manager.list().await;
    (StatusCode::OK, Json(serde_json::to_value(vms).unwrap()))
}

/// GET /v1/instances/:id — get a single instance.
/// Supports `?consistency=strong` for linearizable reads via the leader.
async fn get_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
    Query(params): Query<ConsistencyQuery>,
) -> impl IntoResponse {
    // Strong consistency: forward to leader for linearizable read.
    if params.is_strong() {
        if let Some(leader_addr) = leader_forge_addr_for_strong_read(&state).await {
            let is_leader = {
                let guard = state.raft_client.read().await;
                guard.as_ref().map(|c| c.is_leader()).unwrap_or(false)
            };
            if !is_leader {
                match forward_get_to_leader(&leader_addr, &format!("/v1/instances/{id}")).await {
                    Ok(resp) => return resp,
                    Err(resp) => return resp,
                }
            }
        }
    }

    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    match vm_manager.info(&id).await {
        Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "FORGE_INSTANCE_NOT_FOUND",
                "message": format!("instance '{}' not found: {}", id, e),
            })),
        ),
    }
}

/// DELETE /v1/instances/:id — delete an instance with reverse dependency cleanup.
///
/// Orchestration order (reverse of create):
///   stop VM -> announce FDB remove -> release IP (IPAM) -> delete NIC record
///   -> delete TAP/veth -> remove nftables rules -> remove FDB entries
///   -> if bridge empty: remove gateway IP, NAT, VXLAN, bridge
///
/// Best-effort: errors in cleanup steps are logged but do not fail the delete.
/// The VmManager handles the full reverse-dependency flow internally.
async fn delete_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Leader forwarding for mutations.
    if let Some(leader_addr) = should_forward_to_leader(&state).await {
        debug!("delete_instance: not leader, forwarding to leader at {leader_addr}");
        match forward_simple_to_leader(&leader_addr, "DELETE", &format!("/v1/instances/{id}")).await
        {
            Ok(resp) => return resp,
            Err(resp) => return resp,
        }
    }

    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Create task record.
    let task_id = format!("task-{}", uuid::Uuid::new_v4());
    if let Some(ref task_store) = state.task_store {
        let _ = task_store.create_task(&task_id, &id, "delete_instance");
        let _ = task_store.start_task(&task_id);
    }

    info!(instance = %id, task = %task_id, "starting delete orchestration");

    // Get VM info first for capacity release.
    let vm_info = vm_manager.info(&id).await.ok();

    // VmManager::delete_vm handles the full reverse-dependency cleanup:
    // stop -> FDB remove -> IPAM release -> NIC delete -> TAP delete
    // -> nftables remove -> bridge cleanup (if empty)
    // All cleanup steps are best-effort — errors are logged, not propagated.
    match vm_manager.delete_vm(&id).await {
        Ok(()) => {
            // Release capacity tracking and immediately propagate to Raft
            // so the scheduler sees freed capacity without waiting for the
            // next periodic update tick.
            if let (Some(ref capacity), Some(ref info)) = (&state.capacity, &vm_info) {
                capacity.release(&id, info.vcpus, info.memory_mb as u64);
                info!(
                    instance = %id,
                    vcpus = info.vcpus,
                    memory_mb = info.memory_mb,
                    "capacity released"
                );
                // Immediate Raft capacity update.
                if let Some(raft_client) = state.raft_client.read().await.as_ref() {
                    let cmd = syfrah_controlplane::StateMachineCommand::UpdateHypervisorCapacity {
                        name: state.local_node_name.clone(),
                        allocatable_vcpus: capacity.allocatable_vcpus(),
                        allocatable_memory_mb: capacity.allocatable_memory_mb(),
                        used_vcpus: capacity.used_vcpus(),
                        used_memory_mb: capacity.used_memory_mb(),
                    };
                    if let Err(e) = raft_client.write(cmd).await {
                        debug!("immediate capacity update after delete failed: {e}");
                    }
                }
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.complete_task(&task_id);
            }
            info!(instance = %id, task = %task_id, "delete orchestration complete");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "code": "FORGE_INSTANCE_DELETED",
                    "message": format!("instance '{}' deleted", id),
                    "task_id": task_id,
                })),
            )
        }
        Err(e) => {
            // Even on error, attempt capacity release (best-effort).
            if let (Some(ref capacity), Some(ref info)) = (&state.capacity, &vm_info) {
                capacity.release(&id, info.vcpus, info.memory_mb as u64);
                warn!(
                    instance = %id,
                    "released capacity despite delete error (best-effort cleanup)"
                );
            }
            if let Some(ref task_store) = state.task_store {
                let _ = task_store.fail_task(&task_id, &e.to_string());
            }
            warn!(instance = %id, error = %e, "delete orchestration failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "code": "FORGE_DELETE_FAILED",
                    "message": e.to_string(),
                    "task_id": task_id,
                })),
            )
        }
    }
}

/// POST /v1/instances/:id/start
async fn start_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    match vm_manager.start_vm(&id).await {
        Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_START_FAILED",
                "message": e.to_string(),
            })),
        ),
    }
}

/// POST /v1/instances/:id/stop
async fn stop_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    match vm_manager.shutdown_vm(&id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "code": "FORGE_INSTANCE_STOPPED",
                "message": format!("instance '{}' stop initiated", id),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_STOP_FAILED",
                "message": e.to_string(),
            })),
        ),
    }
}

/// POST /v1/instances/:id/reboot
async fn reboot_instance_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref vm_manager) = state.vm_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"code": "FORGE_COMPUTE_UNAVAILABLE", "message": "compute backend not initialized"}),
            ),
        );
    };

    // Reboot = shutdown then start.
    if let Err(e) = vm_manager.shutdown_vm(&id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_REBOOT_FAILED",
                "message": format!("shutdown phase failed: {}", e),
            })),
        );
    }

    // Wait briefly for shutdown.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    match vm_manager.start_vm(&id).await {
        Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_REBOOT_FAILED",
                "message": format!("start phase failed: {}", e),
            })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Bridge / VXLAN handlers
// ---------------------------------------------------------------------------

/// POST /v1/networks/bridges — ensure a bridge exists for a VPC.
async fn create_bridge_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateBridgeRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let bridge_name = syfrah_overlay::naming::bridge_name(&req.vpc_id);
    let resource_id = format!("br-{}", req.vpc_id);

    if let Err(e) = backend.create_bridge(&bridge_name).await {
        warn!(vpc_id = %req.vpc_id, error = %e, "failed to create bridge");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_BRIDGE_CREATE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    let gen = state
        .generation_tracker
        .as_ref()
        .map(|gt| gt.register(&resource_id));

    // Apply per-bridge accept rules (intra-VPC + internet egress) so
    // that traffic is not dropped by the forward chain's policy drop.
    if let Err(e) = backend.apply_bridge_accept_rules(&bridge_name).await {
        warn!(vpc_id = %req.vpc_id, error = %e, "failed to apply bridge accept rules");
    }

    info!(vpc_id = %req.vpc_id, bridge = %bridge_name, "bridge ensured");
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(BridgeResponse {
                id: resource_id,
                bridge_name,
                vpc_id: req.vpc_id,
                generation: gen,
            })
            .unwrap(),
        ),
    )
}

/// DELETE /v1/networks/bridges/:id — remove a bridge.
async fn delete_bridge_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    // The id is the bridge kernel name (e.g. syfb-XXXXXXXX).
    if let Err(e) = backend.delete_bridge(&id).await {
        warn!(bridge = %id, error = %e, "failed to delete bridge");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_BRIDGE_DELETE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    if let Some(ref gt) = state.generation_tracker {
        gt.remove(&id);
    }

    info!(bridge = %id, "bridge deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_BRIDGE_DELETED",
            "message": format!("bridge '{}' deleted", id)
        })),
    )
}

/// POST /v1/networks/vxlans — ensure a VXLAN exists for a VPC.
async fn create_vxlan_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateVxlanRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let vxlan_name = syfrah_overlay::naming::vxlan_name(&req.vpc_id);
    let resource_id = format!("vx-{}", req.vpc_id);

    if let Err(e) = backend
        .create_vxlan(&vxlan_name, req.vni, &req.local_ip, req.port)
        .await
    {
        warn!(vpc_id = %req.vpc_id, error = %e, "failed to create VXLAN");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_VXLAN_CREATE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    // Attach VXLAN to its bridge.
    let bridge_name = syfrah_overlay::naming::bridge_name(&req.vpc_id);
    if let Err(e) = backend.attach_to_bridge(&vxlan_name, &bridge_name).await {
        warn!(
            vpc_id = %req.vpc_id,
            vxlan = %vxlan_name,
            bridge = %bridge_name,
            error = %e,
            "failed to attach VXLAN to bridge (bridge may not exist yet)"
        );
        // Non-fatal: the bridge may be created separately.
    }

    let gen = state
        .generation_tracker
        .as_ref()
        .map(|gt| gt.register(&resource_id));

    info!(vpc_id = %req.vpc_id, vxlan = %vxlan_name, vni = req.vni, "VXLAN ensured");
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(VxlanResponse {
                id: resource_id,
                vxlan_name,
                vpc_id: req.vpc_id,
                vni: req.vni,
                generation: gen,
            })
            .unwrap(),
        ),
    )
}

/// DELETE /v1/networks/vxlans/:id — remove a VXLAN.
async fn delete_vxlan_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    if let Err(e) = backend.delete_vxlan(&id).await {
        warn!(vxlan = %id, error = %e, "failed to delete VXLAN");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_VXLAN_DELETE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    if let Some(ref gt) = state.generation_tracker {
        gt.remove(&id);
    }

    info!(vxlan = %id, "VXLAN deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_VXLAN_DELETED",
            "message": format!("VXLAN '{}' deleted", id)
        })),
    )
}

// ---------------------------------------------------------------------------
// NIC handlers
// ---------------------------------------------------------------------------

/// POST /v1/networks/interfaces — create a TAP device, attach to bridge, register NIC.
async fn create_nic_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateNicRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let tap_name = syfrah_overlay::naming::tap_name(&req.vm_id);
    let bridge_name = syfrah_overlay::naming::bridge_name(&req.vpc_id);
    let resource_id = format!("nic-{}", req.vm_id);

    // Create TAP device.
    if let Err(e) = backend.create_tap(&tap_name).await {
        warn!(vm_id = %req.vm_id, error = %e, "failed to create TAP");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_NIC_CREATE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    // Attach TAP to bridge.
    if let Err(e) = backend.attach_to_bridge(&tap_name, &bridge_name).await {
        warn!(
            vm_id = %req.vm_id,
            tap = %tap_name,
            bridge = %bridge_name,
            error = %e,
            "failed to attach TAP to bridge"
        );
        // Best-effort: clean up TAP on bridge attach failure.
        let _ = backend.delete_tap(&tap_name).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_NIC_ATTACH_FAILED",
                "message": e.to_string()
            })),
        );
    }

    // Apply anti-spoofing + default firewall rules.
    if let Err(e) = backend.apply_vm_rules(&tap_name, &req.mac, &req.ip).await {
        warn!(vm_id = %req.vm_id, error = %e, "failed to apply VM firewall rules");
        // Non-fatal: NIC is created, rules can be re-applied by reconciler.
    }

    // Register in NIC registry.
    let record = NicRecord {
        id: resource_id.clone(),
        tap_name: tap_name.clone(),
        vm_id: req.vm_id.clone(),
        vpc_id: req.vpc_id.clone(),
        ip: req.ip.clone(),
        mac: req.mac.clone(),
    };
    state
        .nic_registry
        .lock()
        .unwrap()
        .insert(resource_id.clone(), record);

    let gen = state
        .generation_tracker
        .as_ref()
        .map(|gt| gt.register(&resource_id));

    info!(vm_id = %req.vm_id, tap = %tap_name, "NIC created and attached");
    (
        StatusCode::CREATED,
        Json(
            serde_json::to_value(NicResponse {
                id: resource_id,
                tap_name,
                vm_id: req.vm_id,
                vpc_id: req.vpc_id,
                ip: req.ip,
                mac: req.mac,
                generation: gen,
            })
            .unwrap(),
        ),
    )
}

/// DELETE /v1/networks/interfaces/:id — remove a NIC.
async fn delete_nic_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    // Look up NIC in registry.
    let record = state.nic_registry.lock().unwrap().remove(&id);
    let tap_name = match record {
        Some(ref r) => r.tap_name.clone(),
        None => {
            // If not in registry, treat id as the TAP name directly.
            id.clone()
        }
    };

    // Remove firewall rules first.
    if let Err(e) = backend.remove_vm_rules(&tap_name).await {
        warn!(tap = %tap_name, error = %e, "failed to remove VM firewall rules");
    }

    // Delete TAP device.
    if let Err(e) = backend.delete_tap(&tap_name).await {
        warn!(tap = %tap_name, error = %e, "failed to delete TAP");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_NIC_DELETE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    if let Some(ref gt) = state.generation_tracker {
        gt.remove(&id);
    }

    info!(nic = %id, tap = %tap_name, "NIC deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_NIC_DELETED",
            "message": format!("NIC '{}' deleted", id)
        })),
    )
}

/// GET /v1/networks/interfaces?vm_id=X — list NICs, optionally filtered by VM.
async fn list_nics_handler(
    State(state): State<Arc<ForgeState>>,
    Query(query): Query<NicListQuery>,
) -> impl IntoResponse {
    let registry = state.nic_registry.lock().unwrap();
    let nics: Vec<&NicRecord> = if let Some(ref vm_id) = query.vm_id {
        registry.values().filter(|r| r.vm_id == *vm_id).collect()
    } else {
        registry.values().collect()
    };

    (StatusCode::OK, Json(serde_json::to_value(nics).unwrap()))
}

// ---------------------------------------------------------------------------
// Security Group handlers
// ---------------------------------------------------------------------------

/// Convert SgRuleInput to overlay SecurityGroupRule.
fn to_sg_rule(input: &SgRuleInput) -> syfrah_overlay::sg::SecurityGroupRule {
    use syfrah_overlay::sg::*;
    SecurityGroupRule {
        id: RuleId(input.id.clone()),
        sg_id: SecurityGroupId(input.sg_id.clone()),
        direction: input.direction.parse().unwrap_or(Direction::Ingress),
        protocol: match input.protocol.to_lowercase().as_str() {
            "tcp" => Protocol::Tcp,
            "udp" => Protocol::Udp,
            "icmp" => Protocol::Icmp,
            _ => Protocol::All,
        },
        port_range: match (input.port_range_start, input.port_range_end) {
            (Some(start), Some(end)) => Some(PortRange {
                from: start,
                to: end,
            }),
            (Some(start), None) => Some(PortRange {
                from: start,
                to: start,
            }),
            _ => None,
        },
        source: TrafficSource::Cidr(input.source.clone()),
        priority: input.priority,
        description: String::new(),
        created_at: 0,
    }
}

/// POST /v1/networks/sg/apply — generate nftables from SG rules, apply per-VM chains.
async fn apply_sg_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<ApplySgRequest>,
) -> impl IntoResponse {
    let _backend = match state.network_backend {
        Some(ref b) => b,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "code": "FORGE_NETWORK_UNAVAILABLE",
                    "message": "network backend not initialized"
                })),
            );
        }
    };

    let iface_name = req
        .iface_name
        .clone()
        .unwrap_or_else(|| syfrah_overlay::naming::tap_name(&req.vm_id));
    let nic = syfrah_overlay::sg_nft::NetworkInterface {
        id: syfrah_overlay::sg_nft::NicId(format!("nic-{}", req.vm_id)),
        vm_id: req.vm_id.clone(),
        private_ip: match req.ip.parse() {
            Ok(ip) => ip,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "code": "FORGE_INVALID_IP",
                        "message": format!("invalid IP address: {}", req.ip)
                    })),
                );
            }
        },
        mac: req.mac.clone(),
        security_groups: req
            .security_groups
            .iter()
            .map(|s| syfrah_overlay::sg::SecurityGroupId(s.clone()))
            .collect(),
        iface_name,
    };

    let rules: Vec<syfrah_overlay::sg::SecurityGroupRule> =
        req.rules.iter().map(to_sg_rule).collect();

    // Build the ruleset (this is pure computation, no I/O).
    let ruleset = syfrah_overlay::sg_nft::build_sg_ruleset(&nic, &rules, &req.sg_ip_map);

    // Apply via nft -f - (this shells out).
    if let Err(e) = syfrah_overlay::nft::apply_ruleset(&ruleset) {
        warn!(vm_id = %req.vm_id, error = %e, "failed to apply SG rules");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_SG_APPLY_FAILED",
                "message": e.to_string()
            })),
        );
    }

    info!(vm_id = %req.vm_id, sg_count = req.security_groups.len(), "SG rules applied");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_SG_APPLIED",
            "vm_id": req.vm_id,
            "chains": [
                syfrah_overlay::sg_nft::ingress_chain_name(&req.vm_id),
                syfrah_overlay::sg_nft::egress_chain_name(&req.vm_id),
            ]
        })),
    )
}

/// POST /v1/networks/sg/remove — flush VM chains.
async fn remove_sg_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<RemoveSgRequest>,
) -> impl IntoResponse {
    let _backend = match state.network_backend {
        Some(ref b) => b,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "code": "FORGE_NETWORK_UNAVAILABLE",
                    "message": "network backend not initialized"
                })),
            );
        }
    };

    let iface_name = req
        .iface_name
        .as_deref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| syfrah_overlay::naming::tap_name(&req.vm_id));
    if let Err(e) = syfrah_overlay::sg_nft::remove_sg_for_vm(&req.vm_id, &iface_name) {
        warn!(vm_id = %req.vm_id, error = %e, "failed to remove SG chains");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_SG_REMOVE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    info!(vm_id = %req.vm_id, "SG chains removed");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_SG_REMOVED",
            "vm_id": req.vm_id
        })),
    )
}

// ---------------------------------------------------------------------------
// NAT Gateway handlers
// ---------------------------------------------------------------------------

/// POST /v1/networks/nat-gw — create NAT GW, apply masquerade.
async fn create_nat_gw_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<CreateNatGwRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let id = format!("nat-{}-{}", req.bridge, req.subnet_cidr.replace('/', "_"));

    // Register as Pending.
    {
        let mut registry = state.nat_gw_registry.lock().unwrap();
        registry.insert(
            id.clone(),
            NatGwRecord {
                id: id.clone(),
                bridge: req.bridge.clone(),
                subnet_cidr: req.subnet_cidr.clone(),
                state: NatGwState::Pending,
            },
        );
    }

    // Apply masquerade.
    if let Err(e) = backend.apply_nat(&req.bridge, &req.subnet_cidr).await {
        warn!(bridge = %req.bridge, subnet = %req.subnet_cidr, error = %e, "failed to apply NAT");
        state.nat_gw_registry.lock().unwrap().remove(&id);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_NAT_CREATE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    // Transition to Active.
    {
        let mut registry = state.nat_gw_registry.lock().unwrap();
        if let Some(record) = registry.get_mut(&id) {
            record.state = NatGwState::Active;
        }
    }

    let gen = state.generation_tracker.as_ref().map(|gt| gt.register(&id));

    info!(id = %id, bridge = %req.bridge, subnet = %req.subnet_cidr, "NAT GW created");
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "bridge": req.bridge,
            "subnet_cidr": req.subnet_cidr,
            "state": "Active",
            "generation": gen,
        })),
    )
}

/// DELETE /v1/networks/nat-gw/:id — remove masquerade.
async fn delete_nat_gw_handler(
    State(state): State<Arc<ForgeState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    // Look up record.
    let record = {
        let mut registry = state.nat_gw_registry.lock().unwrap();
        if let Some(r) = registry.get_mut(&id) {
            r.state = NatGwState::Deleting;
            Some(r.clone())
        } else {
            None
        }
    };

    let Some(record) = record else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "FORGE_NAT_NOT_FOUND",
                "message": format!("NAT GW '{}' not found", id)
            })),
        );
    };

    if let Err(e) = backend
        .remove_nat(&record.bridge, &record.subnet_cidr)
        .await
    {
        warn!(id = %id, error = %e, "failed to remove NAT");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "code": "FORGE_NAT_DELETE_FAILED",
                "message": e.to_string()
            })),
        );
    }

    state.nat_gw_registry.lock().unwrap().remove(&id);

    if let Some(ref gt) = state.generation_tracker {
        gt.remove(&id);
    }

    info!(id = %id, "NAT GW deleted");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": "FORGE_NAT_DELETED",
            "message": format!("NAT GW '{}' deleted", id)
        })),
    )
}

/// GET /v1/networks/nat-gw — list all NAT gateways.
async fn list_nat_gw_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let registry = state.nat_gw_registry.lock().unwrap();
    let gws: Vec<&NatGwRecord> = registry.values().collect();
    (StatusCode::OK, Json(serde_json::to_value(gws).unwrap()))
}

// ---------------------------------------------------------------------------
// FDB management handlers
// ---------------------------------------------------------------------------

/// GET /v1/networks/fdb — list all FDB entries.
async fn list_fdb_handler(State(state): State<Arc<ForgeState>>) -> impl IntoResponse {
    let registry = state.fdb_registry.lock().unwrap();
    let entries: Vec<&FdbEntry> = registry.values().collect();
    (StatusCode::OK, Json(serde_json::to_value(entries).unwrap()))
}

/// POST /v1/networks/fdb — add or remove FDB + ARP proxy entries.
async fn manage_fdb_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<FdbRequest>,
) -> impl IntoResponse {
    let Some(ref backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let bridge_name = syfrah_overlay::naming::bridge_name(&req.vpc_id);
    let vxlan_name = req
        .vxlan_name
        .clone()
        .unwrap_or_else(|| syfrah_overlay::naming::vxlan_name(&req.vpc_id));
    let key = format!("fdb-{}-{}", req.vpc_id, req.mac);

    match req.action.as_str() {
        "add" => {
            // Add FDB entry.
            if let Err(e) = backend
                .add_fdb_entry(&bridge_name, &req.mac, &req.vtep)
                .await
            {
                warn!(vpc = %req.vpc_id, mac = %req.mac, error = %e, "failed to add FDB entry");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "code": "FORGE_FDB_ADD_FAILED",
                        "message": e.to_string()
                    })),
                );
            }

            // Add ARP proxy if vm_ip provided.
            if let Some(ref vm_ip) = req.vm_ip {
                if let Err(e) = backend.add_arp_proxy(&vxlan_name, vm_ip, &req.mac).await {
                    warn!(vpc = %req.vpc_id, ip = %vm_ip, error = %e, "failed to add ARP proxy");
                    // Non-fatal: FDB entry was added, ARP proxy can be retried.
                }
            }

            // Register in fdb_registry.
            state.fdb_registry.lock().unwrap().insert(
                key.clone(),
                FdbEntry {
                    vpc_id: req.vpc_id.clone(),
                    bridge_name: bridge_name.clone(),
                    mac: req.mac.clone(),
                    vtep: req.vtep.clone(),
                    vm_ip: req.vm_ip.clone(),
                },
            );

            info!(vpc = %req.vpc_id, mac = %req.mac, vtep = %req.vtep, "FDB entry added");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "code": "FORGE_FDB_ADDED",
                    "key": key,
                    "bridge": bridge_name,
                    "mac": req.mac,
                    "vtep": req.vtep,
                })),
            )
        }
        "remove" => {
            // Remove FDB entry.
            if let Err(e) = backend.remove_fdb_entry(&bridge_name, &req.mac).await {
                warn!(vpc = %req.vpc_id, mac = %req.mac, error = %e, "failed to remove FDB entry");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "code": "FORGE_FDB_REMOVE_FAILED",
                        "message": e.to_string()
                    })),
                );
            }

            // Remove ARP proxy if vm_ip provided.
            if let Some(ref vm_ip) = req.vm_ip {
                if let Err(e) = backend.remove_arp_proxy(&vxlan_name, vm_ip).await {
                    warn!(vpc = %req.vpc_id, ip = %vm_ip, error = %e, "failed to remove ARP proxy");
                }
            }

            state.fdb_registry.lock().unwrap().remove(&key);

            info!(vpc = %req.vpc_id, mac = %req.mac, "FDB entry removed");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "code": "FORGE_FDB_REMOVED",
                    "key": key,
                })),
            )
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "code": "FORGE_FDB_INVALID_ACTION",
                "message": format!("invalid action '{}': expected 'add' or 'remove'", other)
            })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Route enforcement handler
// ---------------------------------------------------------------------------

/// POST /v1/networks/routes/enforce — apply blackhole routes as nftables DROP.
async fn enforce_routes_handler(
    State(state): State<Arc<ForgeState>>,
    Json(req): Json<EnforceRoutesRequest>,
) -> impl IntoResponse {
    let Some(ref _backend) = state.network_backend else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "FORGE_NETWORK_UNAVAILABLE",
                "message": "network backend not initialized"
            })),
        );
    };

    let mut applied = Vec::new();
    let mut errors = Vec::new();

    for route in &req.routes {
        // Validate route targets.
        match route.target_type.as_str() {
            "nat-gw" => {
                if let Some(ref target_id) = route.target_id {
                    let registry = state.nat_gw_registry.lock().unwrap();
                    match registry.get(target_id) {
                        Some(gw) if gw.state == NatGwState::Active => {
                            applied.push(route.clone());
                        }
                        Some(_) => {
                            errors.push(serde_json::json!({
                                "destination": route.destination,
                                "error": format!("NAT GW '{}' is not Active", target_id)
                            }));
                        }
                        None => {
                            errors.push(serde_json::json!({
                                "destination": route.destination,
                                "error": format!("NAT GW '{}' not found", target_id)
                            }));
                        }
                    }
                } else {
                    errors.push(serde_json::json!({
                        "destination": route.destination,
                        "error": "nat-gw route requires target_id"
                    }));
                }
            }
            "peering" => {
                // Peering validation: just check target_id exists.
                if route.target_id.is_some() {
                    applied.push(route.clone());
                } else {
                    errors.push(serde_json::json!({
                        "destination": route.destination,
                        "error": "peering route requires target_id"
                    }));
                }
            }
            "blackhole" => {
                // Blackhole routes are applied as nftables DROP rules.
                let ruleset = format!(
                    "add table inet syfrah_routes\nadd chain inet syfrah_routes blackhole\nadd rule inet syfrah_routes blackhole ip daddr {} drop\n",
                    route.destination
                );
                if let Err(e) = syfrah_overlay::nft::apply_ruleset(&ruleset) {
                    errors.push(serde_json::json!({
                        "destination": route.destination,
                        "error": e.to_string()
                    }));
                } else {
                    applied.push(route.clone());
                }
            }
            other => {
                errors.push(serde_json::json!({
                    "destination": route.destination,
                    "error": format!("unknown target_type: {}", other)
                }));
            }
        }
    }

    let status = if errors.is_empty() {
        StatusCode::OK
    } else if applied.is_empty() {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK // partial success
    };

    info!(
        applied = applied.len(),
        errors = errors.len(),
        "route enforcement complete"
    );

    (
        status,
        Json(serde_json::json!({
            "code": "FORGE_ROUTES_ENFORCED",
            "applied": applied.len(),
            "errors": errors,
        })),
    )
}

/// Build the Forge HTTP router with all routes.
pub fn forge_router(state: Arc<ForgeState>) -> Router {
    Router::new()
        // -- Hypervisor endpoints (canonical) --
        .route("/v1/hypervisor/health", get(health_handler))
        .route("/v1/hypervisor/status", get(status_handler))
        .route("/v1/hypervisor/capacity", get(capacity_handler))
        .route("/v1/hypervisor/reservations", get(reservations_handler))
        .route("/v1/hypervisor/metrics", get(metrics_handler))
        .route("/v1/hypervisor/resources", get(resources_handler))
        .route(
            "/v1/hypervisor/drain",
            get(drain_status_handler).post(drain_handler),
        )
        .route("/v1/hypervisor/activate", post(activate_handler))
        .route("/metrics", get(prometheus_metrics_handler))
        // -- Deprecated /v1/node/* aliases --
        .route("/v1/node/health", get(health_handler))
        .route("/v1/node/status", get(status_handler))
        .route("/v1/node/capacity", get(capacity_handler))
        .route("/v1/node/metrics", get(metrics_handler))
        .route("/v1/node/resources", get(resources_handler))
        // -- Networks: bridges --
        .route("/v1/networks/bridges", post(create_bridge_handler))
        .route("/v1/networks/bridges/{id}", delete(delete_bridge_handler))
        // -- Networks: VXLANs --
        .route("/v1/networks/vxlans", post(create_vxlan_handler))
        .route("/v1/networks/vxlans/{id}", delete(delete_vxlan_handler))
        // -- Networks: NICs --
        .route(
            "/v1/networks/interfaces",
            get(list_nics_handler).post(create_nic_handler),
        )
        .route("/v1/networks/interfaces/{id}", delete(delete_nic_handler))
        // -- Networks: Security Groups --
        .route("/v1/networks/sg/apply", post(apply_sg_handler))
        .route("/v1/networks/sg/remove", post(remove_sg_handler))
        // -- Networks: NAT Gateways --
        .route(
            "/v1/networks/nat-gw",
            get(list_nat_gw_handler).post(create_nat_gw_handler),
        )
        .route("/v1/networks/nat-gw/{id}", delete(delete_nat_gw_handler))
        // -- Networks: FDB --
        .route(
            "/v1/networks/fdb",
            get(list_fdb_handler).post(manage_fdb_handler),
        )
        // -- Networks: Routes --
        .route("/v1/networks/routes/enforce", post(enforce_routes_handler))
        // -- Tasks --
        .route("/v1/tasks", get(list_tasks_handler))
        .route("/v1/tasks/{id}", get(get_task_handler))
        // -- Instances --
        .route(
            "/v1/instances",
            get(list_instances_handler).post(create_instance_handler),
        )
        .route(
            "/v1/instances/{id}",
            get(get_instance_handler).delete(delete_instance_handler),
        )
        .route("/v1/instances/{id}/start", post(start_instance_handler))
        .route("/v1/instances/{id}/stop", post(stop_instance_handler))
        .route("/v1/instances/{id}/reboot", post(reboot_instance_handler))
        .layer(axum::middleware::from_fn(crate::auth::auth_middleware))
        .with_state(state)
}

/// The Forge HTTP server. Binds to the fabric IPv6 address on port 7100.
pub struct ForgeServer;

impl ForgeServer {
    /// Start the Forge HTTP server.
    pub async fn serve(
        bind_addr: SocketAddr,
        state: Arc<ForgeState>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let app = forge_router(state);
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        info!("Forge HTTP API listening on {}", bind_addr);

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                loop {
                    if shutdown_rx.changed().await.is_err() {
                        break;
                    }
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
            })
            .await?;

        info!("Forge HTTP API shut down");
        Ok(())
    }
}

/// Empty handler for the control socket — keeps backward compatibility.
pub struct ForgeHandler;

#[async_trait::async_trait]
impl LayerHandler for ForgeHandler {
    async fn handle(&self, _request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "error": "not implemented",
            "layer": "forge"
        }))
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state() -> Arc<ForgeState> {
        Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
            capacity: None,
            org_store: None,
            network_backend: None,
            generation_tracker: None,
            nic_registry: Arc::new(Mutex::new(HashMap::new())),
            nat_gw_registry: Arc::new(Mutex::new(HashMap::new())),
            fdb_registry: Arc::new(Mutex::new(HashMap::new())),
            drain_controller: None,
            metrics_collector: None,
            raft_client: Arc::new(tokio::sync::RwLock::new(None)),
            gossip_cluster: Arc::new(tokio::sync::RwLock::new(None)),
            hypervisor_store: None,
            storage_store: None,
            local_node_name: String::new(),
            local_fabric_ipv6: String::new(),
        })
    }

    fn test_state_with_tasks() -> (tempfile::TempDir, Arc<ForgeState>) {
        let dir = tempfile::tempdir().unwrap();
        let db = syfrah_state::LayerDb::open_at(&dir.path().join("tasks.redb")).unwrap();
        let store = Arc::new(TaskStore::new(db));
        store.create_task("t-1", "vm-1", "create").unwrap();
        store.create_task("t-2", "vm-2", "create").unwrap();
        store.create_task("t-3", "vm-1", "delete").unwrap();

        let state = Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: Some(store),
            vm_manager: None,
            capacity: None,
            org_store: None,
            network_backend: None,
            generation_tracker: None,
            nic_registry: Arc::new(Mutex::new(HashMap::new())),
            nat_gw_registry: Arc::new(Mutex::new(HashMap::new())),
            fdb_registry: Arc::new(Mutex::new(HashMap::new())),
            drain_controller: None,
            metrics_collector: None,
            raft_client: Arc::new(tokio::sync::RwLock::new(None)),
            gossip_cluster: Arc::new(tokio::sync::RwLock::new(None)),
            hypervisor_store: None,
            storage_store: None,
            local_node_name: String::new(),
            local_fabric_ipv6: String::new(),
        });
        (dir, state)
    }

    #[tokio::test]
    async fn handler_returns_not_implemented() {
        let handler = ForgeHandler;
        let resp = handler.handle(vec![], None).await;
        let body: serde_json::Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(body["error"], "not implemented");
        assert_eq!(body["layer"], "forge");
    }

    #[tokio::test]
    async fn health_endpoint_returns_four_categories() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/node/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["status"], "healthy");
        assert!(health["agent_health"].is_object());
        assert!(health["node_health"].is_object());
        assert!(health["workload_health"].is_object());
        assert!(health["control_health"].is_object());
    }

    #[tokio::test]
    async fn get_task_returns_task() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks/t-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let task: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(task["id"], "t-1");
    }

    #[tokio::test]
    async fn get_task_not_found() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn list_all_tasks() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tasks: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[tokio::test]
    async fn list_tasks_filtered() {
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks?resource_id=vm-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tasks: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn instances_unavailable_without_compute() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/instances")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err["code"], "FORGE_COMPUTE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn create_instance_requires_compute() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"test","image":"alpine","vcpus":1,"memory_mb":512}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn delete_instance_requires_compute() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/instances/test-vm")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn tasks_unavailable_without_store() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .uri("/v1/tasks")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_with_subnet_requires_org_store() {
        // When subnet is specified but org_store is None, the handler should
        // still attempt creation (subnet_info will be None, network setup
        // will be skipped by VmManager).
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"test","image":"alpine","subnet":"frontend"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // 503 because vm_manager is None, not because of subnet
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_orchestration_task_tracking() {
        // Verify that create sets up task tracking even when compute is unavailable
        let (_dir, state) = test_state_with_tasks();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"orch-test","image":"alpine","vcpus":1,"memory_mb":512}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // 503 because vm_manager is None
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_with_capacity_admission() {
        // Verify admission control rejects when capacity is insufficient
        let cap = Arc::new(CapacityTracker::with_capacity(1, 256));
        let state = Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
            capacity: Some(cap),
            org_store: None,
            network_backend: None,
            generation_tracker: None,
            nic_registry: Arc::new(Mutex::new(HashMap::new())),
            nat_gw_registry: Arc::new(Mutex::new(HashMap::new())),
            fdb_registry: Arc::new(Mutex::new(HashMap::new())),
            drain_controller: None,
            metrics_collector: None,
            raft_client: Arc::new(tokio::sync::RwLock::new(None)),
            gossip_cluster: Arc::new(tokio::sync::RwLock::new(None)),
            hypervisor_store: None,
            storage_store: None,
            local_node_name: String::new(),
            local_fabric_ipv6: String::new(),
        });
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/instances")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"big-vm","image":"alpine","vcpus":4,"memory_mb":8192}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 409);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err["code"], "FORGE_INSUFFICIENT_CAPACITY");
    }

    fn test_state_with_network() -> Arc<ForgeState> {
        let backend: Arc<dyn syfrah_overlay::NetworkBackend> =
            Arc::new(syfrah_overlay::MockBackend::new());
        let gen_tracker = Arc::new(crate::generation::GenerationTracker::new());
        Arc::new(ForgeState {
            started_at: Instant::now(),
            task_store: None,
            vm_manager: None,
            capacity: None,
            org_store: None,
            network_backend: Some(backend),
            generation_tracker: Some(gen_tracker),
            nic_registry: Arc::new(Mutex::new(HashMap::new())),
            nat_gw_registry: Arc::new(Mutex::new(HashMap::new())),
            fdb_registry: Arc::new(Mutex::new(HashMap::new())),
            drain_controller: None,
            metrics_collector: None,
            raft_client: Arc::new(tokio::sync::RwLock::new(None)),
            gossip_cluster: Arc::new(tokio::sync::RwLock::new(None)),
            hypervisor_store: None,
            storage_store: None,
            local_node_name: String::new(),
            local_fabric_ipv6: String::new(),
        })
    }

    #[tokio::test]
    async fn create_bridge_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-prod"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["vpc_id"], "vpc-prod");
        assert!(result["bridge_name"].as_str().unwrap().starts_with("syfb-"));
        assert!(result["generation"].is_object());
    }

    #[tokio::test]
    async fn delete_bridge_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/networks/bridges/syfb-12345678")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["code"], "FORGE_BRIDGE_DELETED");
    }

    #[tokio::test]
    async fn create_vxlan_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/vxlans")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vpc_id":"vpc-prod","vni":100,"local_ip":"10.0.0.1"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["vpc_id"], "vpc-prod");
        assert_eq!(result["vni"], 100);
        assert!(result["vxlan_name"].as_str().unwrap().starts_with("syfx-"));
    }

    #[tokio::test]
    async fn idempotent_create() {
        // Creating the same bridge twice should succeed (idempotent).
        let state = test_state_with_network();

        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-test"}"#))
            .unwrap();
        let resp1 = app1.oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), 200);

        let app2 = forge_router(state);
        let req2 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-test"}"#))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), 200);
    }

    #[tokio::test]
    async fn bridge_unavailable_without_network() {
        let state = test_state(); // no network backend
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/bridges")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vpc_id":"vpc-test"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn create_nic_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/interfaces")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vm_id":"vm-1","vpc_id":"vpc-prod","ip":"10.1.0.3","mac":"02:00:0a:01:00:03"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 201);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["vm_id"], "vm-1");
        assert!(result["tap_name"].as_str().unwrap().starts_with("syft-"));
        assert!(result["generation"].is_object());
    }

    #[tokio::test]
    async fn delete_nic_endpoint() {
        let state = test_state_with_network();

        // First create a NIC.
        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/interfaces")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vm_id":"vm-del","vpc_id":"vpc-1","ip":"10.1.0.5","mac":"02:00:0a:01:00:05"}"#,
            ))
            .unwrap();
        let resp1 = app1.oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), 201);

        // Now delete it.
        let app2 = forge_router(Arc::clone(&state));
        let req2 = axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/networks/interfaces/nic-vm-del")
            .body(Body::empty())
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), 200);

        let body = resp2.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["code"], "FORGE_NIC_DELETED");
    }

    #[tokio::test]
    async fn list_nics_by_vm() {
        let state = test_state_with_network();

        // Create two NICs for different VMs.
        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/interfaces")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vm_id":"vm-a","vpc_id":"vpc-1","ip":"10.1.0.1","mac":"02:00:0a:01:00:01"}"#,
            ))
            .unwrap();
        app1.oneshot(req1).await.unwrap();

        let app2 = forge_router(Arc::clone(&state));
        let req2 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/interfaces")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vm_id":"vm-b","vpc_id":"vpc-1","ip":"10.1.0.2","mac":"02:00:0a:01:00:02"}"#,
            ))
            .unwrap();
        app2.oneshot(req2).await.unwrap();

        // List all NICs.
        let app3 = forge_router(Arc::clone(&state));
        let req3 = axum::http::Request::builder()
            .uri("/v1/networks/interfaces")
            .body(Body::empty())
            .unwrap();
        let resp3 = app3.oneshot(req3).await.unwrap();
        assert_eq!(resp3.status(), 200);
        let body = resp3.into_body().collect().await.unwrap().to_bytes();
        let nics: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(nics.len(), 2);

        // List NICs for vm-a only.
        let app4 = forge_router(state);
        let req4 = axum::http::Request::builder()
            .uri("/v1/networks/interfaces?vm_id=vm-a")
            .body(Body::empty())
            .unwrap();
        let resp4 = app4.oneshot(req4).await.unwrap();
        assert_eq!(resp4.status(), 200);
        let body = resp4.into_body().collect().await.unwrap().to_bytes();
        let nics: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(nics.len(), 1);
        assert_eq!(nics[0]["vm_id"], "vm-a");
    }

    #[tokio::test]
    async fn sg_apply_unavailable_without_network() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/sg/apply")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vm_id":"vm-1","ip":"10.1.0.3","mac":"02:00:0a:01:00:03","security_groups":["default"]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn sg_remove_unavailable_without_network() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/sg/remove")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"vm_id":"vm-1"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn sg_apply_invalid_ip() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/sg/apply")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"vm_id":"vm-1","ip":"not-an-ip","mac":"02:00:0a:01:00:03","security_groups":["default"]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err["code"], "FORGE_INVALID_IP");
    }

    #[tokio::test]
    async fn create_nat_gw_endpoint() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/nat-gw")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"bridge":"syfb-12345678","subnet_cidr":"10.1.0.0/24"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 201);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["state"], "Active");
        assert!(result["id"].as_str().unwrap().starts_with("nat-"));
    }

    #[tokio::test]
    async fn delete_nat_gw_not_found() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/networks/nat-gw/nat-nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn nat_gw_lifecycle() {
        let state = test_state_with_network();

        // Create.
        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/nat-gw")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"bridge":"syfb-aabbccdd","subnet_cidr":"10.2.0.0/24"}"#,
            ))
            .unwrap();
        let resp1 = app1.oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), 201);
        let body = resp1.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let nat_id = result["id"].as_str().unwrap().to_string();

        // List — should have 1 entry.
        let app2 = forge_router(Arc::clone(&state));
        let req2 = axum::http::Request::builder()
            .uri("/v1/networks/nat-gw")
            .body(Body::empty())
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        let body = resp2.into_body().collect().await.unwrap().to_bytes();
        let gws: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(gws.len(), 1);

        // Delete.
        let app3 = forge_router(Arc::clone(&state));
        let req3 = axum::http::Request::builder()
            .method("DELETE")
            .uri(format!("/v1/networks/nat-gw/{}", nat_id))
            .body(Body::empty())
            .unwrap();
        let resp3 = app3.oneshot(req3).await.unwrap();
        assert_eq!(resp3.status(), 200);

        // List — should be empty.
        let app4 = forge_router(state);
        let req4 = axum::http::Request::builder()
            .uri("/v1/networks/nat-gw")
            .body(Body::empty())
            .unwrap();
        let resp4 = app4.oneshot(req4).await.unwrap();
        let body = resp4.into_body().collect().await.unwrap().to_bytes();
        let gws: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(gws.is_empty());
    }

    #[tokio::test]
    async fn enforce_routes_unavailable_without_network() {
        let state = test_state();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"routes":[]}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 503);
    }

    #[tokio::test]
    async fn enforce_routes_validates_nat_gw_target() {
        let state = test_state_with_network();
        let app = forge_router(state);

        // Route references a non-existent NAT GW.
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"routes":[{"destination":"10.2.0.0/24","target_type":"nat-gw","target_id":"nat-nonexistent"}]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Should return 500 since all routes failed.
        assert_eq!(resp.status(), 500);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["applied"], 0);
        assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn enforce_routes_unknown_target_type() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"routes":[{"destination":"10.3.0.0/24","target_type":"unknown"}]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["applied"], 0);
    }

    #[tokio::test]
    async fn blackhole_applied() {
        // Blackhole routes are applied as nftables DROP rules.
        // In test env nft binary may not exist — the handler still records errors.
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"routes":[{"destination":"10.99.0.0/16","target_type":"blackhole"}]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // nft may or may not be available — either applied==1 or errors==1.
        let applied = result["applied"].as_u64().unwrap_or(0);
        let errors = result["errors"].as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(applied as usize + errors, 1, "exactly one route processed");
    }

    #[tokio::test]
    async fn target_validation_peering() {
        let state = test_state_with_network();
        let app = forge_router(state);

        // Peering route with target_id succeeds validation.
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"routes":[{"destination":"10.4.0.0/24","target_type":"peering","target_id":"peer-abc"}]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["applied"], 1);
        assert_eq!(result["errors"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn target_validation_peering_missing_id() {
        let state = test_state_with_network();
        let app = forge_router(state);

        // Peering route without target_id fails validation.
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"routes":[{"destination":"10.4.0.0/24","target_type":"peering"}]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 500);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["applied"], 0);
    }

    #[tokio::test]
    async fn inactive_target_logged() {
        // NAT GW exists but is not Active — route should fail with descriptive error.
        let state = test_state_with_network();

        // Insert a NAT GW in Pending state (not Active).
        {
            let mut registry = state.nat_gw_registry.lock().unwrap();
            registry.insert(
                "nat-pending-1".to_string(),
                NatGwRecord {
                    id: "nat-pending-1".to_string(),
                    bridge: "syfb-12345678".to_string(),
                    subnet_cidr: "10.5.0.0/24".to_string(),
                    state: NatGwState::Pending,
                },
            );
        }

        let app = forge_router(state);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/routes/enforce")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"routes":[{"destination":"10.5.0.0/24","target_type":"nat-gw","target_id":"nat-pending-1"}]}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 500);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["applied"], 0);
        let err_msg = result["errors"][0]["error"].as_str().unwrap();
        assert!(
            err_msg.contains("not Active"),
            "error should mention inactive state: {err_msg}"
        );
    }

    #[tokio::test]
    async fn fdb_add_and_list() {
        let state = test_state_with_network();

        // Add FDB entry.
        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/fdb")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"action":"add","vpc_id":"vpc-fdb","mac":"02:00:0a:01:00:03","vtep":"10.0.0.2","vm_ip":"10.1.0.3"}"#,
            ))
            .unwrap();
        let resp1 = app1.oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), 200);

        // List FDB entries.
        let app2 = forge_router(Arc::clone(&state));
        let req2 = axum::http::Request::builder()
            .uri("/v1/networks/fdb")
            .body(Body::empty())
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        let body = resp2.into_body().collect().await.unwrap().to_bytes();
        let entries: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["mac"], "02:00:0a:01:00:03");
    }

    #[tokio::test]
    async fn fdb_remove() {
        let state = test_state_with_network();

        // Add then remove.
        let app1 = forge_router(Arc::clone(&state));
        let req1 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/fdb")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"action":"add","vpc_id":"vpc-fdb","mac":"02:00:0a:01:00:04","vtep":"10.0.0.3"}"#,
            ))
            .unwrap();
        app1.oneshot(req1).await.unwrap();

        let app2 = forge_router(Arc::clone(&state));
        let req2 = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/fdb")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"action":"remove","vpc_id":"vpc-fdb","mac":"02:00:0a:01:00:04","vtep":"10.0.0.3"}"#,
            ))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), 200);

        // List should be empty (only the entry we removed).
        let app3 = forge_router(state);
        let req3 = axum::http::Request::builder()
            .uri("/v1/networks/fdb")
            .body(Body::empty())
            .unwrap();
        let resp3 = app3.oneshot(req3).await.unwrap();
        let body = resp3.into_body().collect().await.unwrap().to_bytes();
        let entries: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn fdb_invalid_action() {
        let state = test_state_with_network();
        let app = forge_router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/networks/fdb")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"action":"invalid","vpc_id":"vpc-1","mac":"02:00:00:00:00:01","vtep":"10.0.0.1"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
    }
}
