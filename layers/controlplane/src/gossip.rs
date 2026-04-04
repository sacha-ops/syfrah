//! SWIM gossip integration using the `foca` crate.
//!
//! Each node in the fabric runs a gossip agent that periodically probes
//! other members and disseminates `HypervisorReport` data. Membership
//! states are: Alive, Suspect, Down — matching foca's built-in SWIM model.
//!
//! Gossip runs over UDP on the fabric IPv6 interface, port 7300.

use std::collections::HashMap;
use std::net::SocketAddrV6;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

/// Default gossip UDP port on the fabric IPv6 interface.
pub const GOSSIP_PORT: u16 = 7300;

/// Concrete foca type used by the gossip agent.
type FocaInstance = foca::Foca<
    GossipNodeId,
    foca::BincodeCodec<bincode::config::Configuration>,
    rand::rngs::StdRng,
    foca::NoCustomBroadcast,
>;

/// How often we refresh local capacity data in the gossip report.
const REPORT_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// GossipNodeId — foca Identity
// ---------------------------------------------------------------------------

/// A gossip node identity. Uses the fabric IPv6 socket address as
/// the unique cluster-wide address, plus a monotonic incarnation bump
/// that allows fast rejoin after being declared Down.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GossipNodeId {
    pub addr: SocketAddrV6,
    pub bump: u16,
}

impl GossipNodeId {
    /// Create a new gossip node identity.
    pub fn new(addr: SocketAddrV6) -> Self {
        Self { addr, bump: 0 }
    }
}

impl foca::Identity for GossipNodeId {
    type Addr = SocketAddrV6;

    fn renew(&self) -> Option<Self> {
        Some(Self {
            addr: self.addr,
            bump: self.bump.wrapping_add(1),
        })
    }

    fn addr(&self) -> SocketAddrV6 {
        self.addr
    }

    fn win_addr_conflict(&self, other: &Self) -> bool {
        self.bump > other.bump
    }
}

// We use foca's built-in BincodeCodec (enabled via `bincode-codec` feature).
// It handles serde-based serialization of Header<GossipNodeId> and
// Member<GossipNodeId> over the wire.

// ---------------------------------------------------------------------------
// Member state — Alive / Suspect / Down
// ---------------------------------------------------------------------------

/// Gossip-observed member state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberState {
    Alive,
    Suspect,
    Down,
}

/// Per-member gossip data stored locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipMember {
    pub id: GossipNodeId,
    pub state: MemberState,
    pub report: Option<HypervisorGossipReport>,
}

// ---------------------------------------------------------------------------
// HypervisorGossipReport — disseminated via gossip
// ---------------------------------------------------------------------------

/// Capacity and status report disseminated via gossip protocol.
///
/// Each hypervisor publishes this every `REPORT_REFRESH_INTERVAL` so
/// that the scheduler on the Raft leader can make informed placement
/// decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HypervisorGossipReport {
    pub hypervisor_id: String,
    pub node_name: String,
    pub region: String,
    pub zone: String,
    pub state: String, // "Available", "Draining", etc.
    pub allocatable_vcpus: u32,
    pub allocatable_memory_mb: u64,
    pub used_vcpus: u32,
    pub used_memory_mb: u64,
    pub instance_count: u32,
    pub drain_status: bool,
    pub timestamp: u64,
    /// S3 reachability from the latest health probe (None if probe not running).
    #[serde(default)]
    pub s3_reachable: Option<bool>,
    /// S3 PUT latency from the latest health probe (ms).
    #[serde(default)]
    pub s3_put_latency_ms: Option<u64>,
    /// S3 GET latency from the latest health probe (ms).
    #[serde(default)]
    pub s3_get_latency_ms: Option<u64>,
    /// S3 degradation level string (Healthy, FsyncBlocking, EIO, Degraded, Error).
    #[serde(default)]
    pub s3_degradation_level: Option<String>,
}

impl HypervisorGossipReport {
    /// CPU utilization ratio (0.0 – 1.0). Returns 0 if allocatable is 0.
    pub fn cpu_utilization(&self) -> f64 {
        if self.allocatable_vcpus == 0 {
            return 0.0;
        }
        self.used_vcpus as f64 / self.allocatable_vcpus as f64
    }

    /// Memory utilization ratio (0.0 – 1.0). Returns 0 if allocatable is 0.
    pub fn memory_utilization(&self) -> f64 {
        if self.allocatable_memory_mb == 0 {
            return 0.0;
        }
        self.used_memory_mb as f64 / self.allocatable_memory_mb as f64
    }

    /// Available vCPUs = allocatable - used.
    pub fn available_vcpus(&self) -> u32 {
        self.allocatable_vcpus.saturating_sub(self.used_vcpus)
    }

    /// Available memory in MB = allocatable - used.
    pub fn available_memory_mb(&self) -> u64 {
        self.allocatable_memory_mb
            .saturating_sub(self.used_memory_mb)
    }
}

// ---------------------------------------------------------------------------
// GossipCluster — the shared state
// ---------------------------------------------------------------------------

/// Snapshot of gossip metrics for Prometheus export.
#[derive(Debug, Clone, Default)]
pub struct GossipMetricsSnapshot {
    /// Members in Alive state.
    pub members_alive: u64,
    /// Members in Suspect state.
    pub members_suspect: u64,
    /// Members in Down state.
    pub members_down: u64,
    /// Total gossip messages sent.
    pub messages_sent: u64,
    /// Total gossip messages received.
    pub messages_received: u64,
}

/// Thread-safe container for gossip cluster state.
///
/// The scheduler reads from this to get hypervisor reports for placement
/// decisions. The gossip agent writes to it when reports are received
/// or member states change.
#[derive(Clone)]
pub struct GossipCluster {
    inner: Arc<Mutex<GossipClusterInner>>,
    /// Total messages sent counter.
    messages_sent: Arc<AtomicU64>,
    /// Total messages received counter.
    messages_received: Arc<AtomicU64>,
}

struct GossipClusterInner {
    /// Reports indexed by hypervisor node name.
    reports: HashMap<String, HypervisorGossipReport>,
    /// Member states indexed by socket address string.
    member_states: HashMap<String, MemberState>,
}

impl GossipCluster {
    /// Create a new empty gossip cluster.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(GossipClusterInner {
                reports: HashMap::new(),
                member_states: HashMap::new(),
            })),
            messages_sent: Arc::new(AtomicU64::new(0)),
            messages_received: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Increment the messages sent counter.
    pub fn inc_messages_sent(&self) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the messages received counter.
    pub fn inc_messages_received(&self) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of gossip metrics for Prometheus export.
    pub fn metrics_snapshot(&self) -> GossipMetricsSnapshot {
        let inner = self.inner.lock().unwrap();
        let mut alive = 0u64;
        let mut suspect = 0u64;
        let mut down = 0u64;
        for state in inner.member_states.values() {
            match state {
                MemberState::Alive => alive += 1,
                MemberState::Suspect => suspect += 1,
                MemberState::Down => down += 1,
            }
        }
        GossipMetricsSnapshot {
            members_alive: alive,
            members_suspect: suspect,
            members_down: down,
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            messages_received: self.messages_received.load(Ordering::Relaxed),
        }
    }

    /// Update a hypervisor report (called when gossip delivers new data).
    pub fn update_report(&self, report: HypervisorGossipReport) {
        let mut inner = self.inner.lock().unwrap();
        inner.reports.insert(report.node_name.clone(), report);
    }

    /// Set a member's state (Alive/Suspect/Down).
    pub fn set_member_state(&self, addr: &str, state: MemberState) {
        let mut inner = self.inner.lock().unwrap();
        inner.member_states.insert(addr.to_string(), state);
    }

    /// Get all current hypervisor reports (snapshot).
    pub fn all_reports(&self) -> Vec<HypervisorGossipReport> {
        let inner = self.inner.lock().unwrap();
        inner.reports.values().cloned().collect()
    }

    /// Get a report for a specific node name.
    pub fn get_report(&self, node_name: &str) -> Option<HypervisorGossipReport> {
        let inner = self.inner.lock().unwrap();
        inner.reports.get(node_name).cloned()
    }

    /// Get the gossip state for a node (by its address string).
    pub fn get_member_state(&self, addr: &str) -> Option<MemberState> {
        let inner = self.inner.lock().unwrap();
        inner.member_states.get(addr).copied()
    }

    /// Get all members that are Down.
    pub fn down_members(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .member_states
            .iter()
            .filter(|(_, s)| **s == MemberState::Down)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Mark a member as Down by node name (for testing / manual trigger).
    pub fn mark_down(&self, node_name: &str) {
        let mut inner = self.inner.lock().unwrap();
        // Find the report and mark the member state.
        if let Some(report) = inner.reports.get(node_name) {
            let addr = format!("[{}]:{}", report.hypervisor_id, GOSSIP_PORT);
            inner.member_states.insert(addr, MemberState::Down);
        }
        // Also store by node_name directly for convenience.
        inner
            .member_states
            .insert(node_name.to_string(), MemberState::Down);
    }

    /// Remove a report (when a member is permanently down).
    pub fn remove_report(&self, node_name: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.reports.remove(node_name);
    }
}

impl Default for GossipCluster {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// GossipAgent — the async driver
// ---------------------------------------------------------------------------

/// Configuration for the gossip agent.
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Local bind address (fabric IPv6 + port 7300).
    pub bind_addr: SocketAddrV6,
    /// Addresses of seed nodes to announce to on startup.
    pub seeds: Vec<SocketAddrV6>,
    /// Local hypervisor info for building reports.
    pub local_node_name: String,
    pub local_hypervisor_id: String,
    pub local_region: String,
    pub local_zone: String,
}

/// Start the gossip agent. Returns a handle to the shared cluster state.
///
/// The agent runs in background tokio tasks:
/// 1. UDP receive loop — feeds incoming packets to foca.
/// 2. Timer loop — drives foca's periodic probes and announcements.
/// 3. Report refresh loop — updates local capacity data every 10s.
pub async fn start_gossip_agent(
    config: GossipConfig,
    cluster: GossipCluster,
    capacity_fn: Arc<dyn Fn() -> (u32, u64, u32, u64, u32) + Send + Sync>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(std::net::SocketAddr::V6(config.bind_addr)).await?;
    let socket = Arc::new(socket);
    info!("gossip agent started on {}", config.bind_addr);

    let my_id = GossipNodeId::new(config.bind_addr);
    let foca_config = foca::Config::simple();

    let codec = foca::BincodeCodec(bincode::config::standard());
    let rng = rand::rngs::StdRng::from_os_rng();
    let foca: FocaInstance = foca::Foca::new(my_id.clone(), foca_config, rng, codec);
    let foca = Arc::new(Mutex::new(foca));

    // Announce to seeds.
    for seed in &config.seeds {
        if *seed == config.bind_addr {
            continue;
        }
        let seed_id = GossipNodeId::new(*seed);
        let mut runtime = foca::AccumulatingRuntime::new();
        {
            let mut f = foca.lock().unwrap();
            if let Err(e) = f.announce(seed_id, &mut runtime) {
                warn!("gossip: failed to announce to seed {seed}: {e}");
            }
        }
        drain_runtime(&mut runtime, &socket, &cluster).await;
    }

    // Publish initial local report.
    {
        let (alloc_v, alloc_m, used_v, used_m, vm_count) = capacity_fn();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let report = HypervisorGossipReport {
            hypervisor_id: config.local_hypervisor_id.clone(),
            node_name: config.local_node_name.clone(),
            region: config.local_region.clone(),
            zone: config.local_zone.clone(),
            state: "Available".to_string(),
            allocatable_vcpus: alloc_v,
            allocatable_memory_mb: alloc_m,
            used_vcpus: used_v,
            used_memory_mb: used_m,
            instance_count: vm_count,
            drain_status: false,
            timestamp: now,
            s3_reachable: None,
            s3_put_latency_ms: None,
            s3_get_latency_ms: None,
            s3_degradation_level: None,
        };
        cluster.update_report(report);
        cluster.set_member_state(&config.bind_addr.to_string(), MemberState::Alive);
    }

    // -- UDP receive loop --
    let recv_socket = Arc::clone(&socket);
    let recv_foca = Arc::clone(&foca);
    let recv_cluster = cluster.clone();
    let mut recv_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            let recv_result = tokio::select! {
                _ = recv_shutdown.wait_for(|v| *v) => break,
                result = recv_socket.recv_from(&mut buf) => result,
            };
            match recv_result {
                Ok((len, src)) => {
                    debug!("gossip: received {len} bytes from {src}");
                    recv_cluster.inc_messages_received();
                    let mut runtime = foca::AccumulatingRuntime::new();
                    {
                        let mut f = recv_foca.lock().unwrap();
                        if let Err(e) = f.handle_data(&buf[..len], &mut runtime) {
                            warn!("gossip: handle_data error: {e}");
                        }
                    }
                    drain_runtime(&mut runtime, &recv_socket, &recv_cluster).await;
                }
                Err(e) => {
                    warn!("gossip: recv error: {e}");
                }
            }
        }
        debug!("gossip: receive loop stopped");
    });

    // -- Periodic probe timer --
    let timer_foca = Arc::clone(&foca);
    let timer_socket = Arc::clone(&socket);
    let timer_cluster = cluster.clone();
    let mut timer_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let probe_interval = Duration::from_secs(1);
        loop {
            tokio::select! {
                _ = timer_shutdown.wait_for(|v| *v) => break,
                _ = tokio::time::sleep(probe_interval) => {},
            }
            let mut runtime = foca::AccumulatingRuntime::new();
            {
                let mut f = timer_foca.lock().unwrap();
                let token = 0;
                let timer = foca::Timer::ProbeRandomMember(token);
                if let Err(e) = f.handle_timer(timer, &mut runtime) {
                    debug!("gossip: timer error (expected on empty cluster): {e}");
                }
            }
            drain_runtime(&mut runtime, &timer_socket, &timer_cluster).await;
        }
        debug!("gossip: timer loop stopped");
    });

    // -- Report refresh loop --
    let report_cluster = cluster.clone();
    let report_config = config.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.wait_for(|v| *v) => break,
                _ = tokio::time::sleep(REPORT_REFRESH_INTERVAL) => {
                    let (alloc_v, alloc_m, used_v, used_m, vm_count) = capacity_fn();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let report = HypervisorGossipReport {
                        hypervisor_id: report_config.local_hypervisor_id.clone(),
                        node_name: report_config.local_node_name.clone(),
                        region: report_config.local_region.clone(),
                        zone: report_config.local_zone.clone(),
                        state: "Available".to_string(),
                        allocatable_vcpus: alloc_v,
                        allocatable_memory_mb: alloc_m,
                        used_vcpus: used_v,
                        used_memory_mb: used_m,
                        instance_count: vm_count,
                        drain_status: false,
                        timestamp: now,
                        s3_reachable: None,
                        s3_put_latency_ms: None,
                        s3_get_latency_ms: None,
                        s3_degradation_level: None,
                    };
                    report_cluster.update_report(report);
                }
            }
        }
        debug!("gossip: report refresh loop stopped");
    });

    Ok(())
}

/// Drain the foca runtime: send packets, handle notifications.
async fn drain_runtime(
    runtime: &mut foca::AccumulatingRuntime<GossipNodeId>,
    socket: &UdpSocket,
    cluster: &GossipCluster,
) {
    // Send queued packets.
    while let Some((to, data)) = runtime.to_send() {
        let addr = std::net::SocketAddr::V6(to.addr);
        if let Err(e) = socket.send_to(&data, addr).await {
            warn!("gossip: send to {addr} failed: {e}");
        }
        cluster.inc_messages_sent();
    }

    // Process notifications.
    while let Some(notification) = runtime.to_notify() {
        match notification {
            foca::OwnedNotification::MemberUp(id) => {
                info!("gossip: member UP — {}", id.addr);
                cluster.set_member_state(&id.addr.to_string(), MemberState::Alive);
            }
            foca::OwnedNotification::MemberDown(id) => {
                warn!("gossip: member DOWN — {}", id.addr);
                cluster.set_member_state(&id.addr.to_string(), MemberState::Down);
            }
            foca::OwnedNotification::Active => {
                info!("gossip: this node is active in the cluster");
            }
            foca::OwnedNotification::Idle => {
                info!("gossip: cluster is idle (no active members)");
            }
            foca::OwnedNotification::Defunct => {
                warn!("gossip: this node was declared defunct");
            }
            foca::OwnedNotification::Rejoin(new_id) => {
                info!("gossip: rejoined cluster as {}", new_id.addr);
            }
            foca::OwnedNotification::Rename(old, new) => {
                info!("gossip: member renamed {} -> {}", old.addr, new.addr);
            }
        }
    }

    // We don't need to handle to_schedule() because we drive foca with
    // our own periodic timer loop above. Foca's AccumulatingRuntime
    // scheduling hints are informational.
    while runtime.to_schedule().is_some() {
        // drain and ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gossip_node_id_identity() {
        let addr: SocketAddrV6 = "[::1]:7300".parse().unwrap();
        let id = GossipNodeId::new(addr);
        assert_eq!(id.bump, 0);

        // renew bumps the counter
        let renewed = foca::Identity::renew(&id).unwrap();
        assert_eq!(renewed.bump, 1);
        assert_eq!(renewed.addr, addr);

        // newer wins conflict
        assert!(foca::Identity::win_addr_conflict(&renewed, &id));
        assert!(!foca::Identity::win_addr_conflict(&id, &renewed));

        // addr() returns the socket address
        assert_eq!(foca::Identity::addr(&id), addr);
    }

    #[test]
    fn bincode_codec_smoke() {
        // Basic smoke test that BincodeCodec can be constructed
        let _codec = foca::BincodeCodec(bincode::config::standard());
    }

    #[test]
    fn gossip_cluster_operations() {
        let cluster = GossipCluster::new();

        // No reports initially
        assert!(cluster.all_reports().is_empty());

        // Add a report
        let report = HypervisorGossipReport {
            hypervisor_id: "hv-1".to_string(),
            node_name: "hv-eu-1".to_string(),
            region: "eu-west".to_string(),
            zone: "az-1".to_string(),
            state: "Available".to_string(),
            allocatable_vcpus: 16,
            allocatable_memory_mb: 65536,
            used_vcpus: 4,
            used_memory_mb: 16384,
            instance_count: 2,
            drain_status: false,
            timestamp: 1000,
            s3_reachable: None,
            s3_put_latency_ms: None,
            s3_get_latency_ms: None,
            s3_degradation_level: None,
        };
        cluster.update_report(report.clone());

        assert_eq!(cluster.all_reports().len(), 1);
        let got = cluster.get_report("hv-eu-1").unwrap();
        assert_eq!(got.zone, "az-1");
        assert_eq!(got.available_vcpus(), 12);
        assert_eq!(got.available_memory_mb(), 49152);

        // Member state
        cluster.set_member_state("[::1]:7300", MemberState::Alive);
        assert_eq!(
            cluster.get_member_state("[::1]:7300"),
            Some(MemberState::Alive)
        );

        cluster.set_member_state("[::1]:7300", MemberState::Down);
        assert_eq!(
            cluster.get_member_state("[::1]:7300"),
            Some(MemberState::Down)
        );
        assert_eq!(cluster.down_members().len(), 1);
    }

    #[test]
    fn report_utilization() {
        let report = HypervisorGossipReport {
            hypervisor_id: "hv-1".to_string(),
            node_name: "hv-eu-1".to_string(),
            region: "eu-west".to_string(),
            zone: "az-1".to_string(),
            state: "Available".to_string(),
            allocatable_vcpus: 10,
            allocatable_memory_mb: 20480,
            used_vcpus: 5,
            used_memory_mb: 10240,
            instance_count: 3,
            drain_status: false,
            timestamp: 1000,
            s3_reachable: None,
            s3_put_latency_ms: None,
            s3_get_latency_ms: None,
            s3_degradation_level: None,
        };
        assert!((report.cpu_utilization() - 0.5).abs() < f64::EPSILON);
        assert!((report.memory_utilization() - 0.5).abs() < f64::EPSILON);

        // Zero allocatable
        let empty = HypervisorGossipReport {
            allocatable_vcpus: 0,
            allocatable_memory_mb: 0,
            ..report.clone()
        };
        assert_eq!(empty.cpu_utilization(), 0.0);
        assert_eq!(empty.memory_utilization(), 0.0);
    }

    #[test]
    fn member_state_serde() {
        let state = MemberState::Alive;
        let json = serde_json::to_string(&state).unwrap();
        let deser: MemberState = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, state);
    }
}
