use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Lifecycle state for mutable network resources (SGs, NICs, NAT GWs, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceState {
    Pending,
    Active,
    Failed,
    Deleting,
    Deleted,
}

impl fmt::Display for ResourceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceState::Pending => f.write_str("Pending"),
            ResourceState::Active => f.write_str("Active"),
            ResourceState::Failed => f.write_str("Failed"),
            ResourceState::Deleting => f.write_str("Deleting"),
            ResourceState::Deleted => f.write_str("Deleted"),
        }
    }
}

/// Unique identifier for a security group.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecurityGroupId(pub String);

impl fmt::Display for SecurityGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A security group — a stateful firewall ruleset scoped to a VPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityGroup {
    pub id: SecurityGroupId,
    pub name: String,
    pub vpc_id: VpcId,
    pub description: Option<String>,
    pub is_default: bool,
    pub state: ResourceState,
    pub created_at: u64,
}

/// Unique identifier for a network interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NicId(pub String);

impl fmt::Display for NicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A network interface — the attachment point for security groups.
/// Every VM has at least one NIC. SGs attach to NICs, not VMs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub id: NicId,
    pub name: String,
    pub vm_id: Option<String>,
    pub subnet_id: String,
    pub vpc_id: String,
    pub private_ip: String,
    pub mac: String,
    pub security_groups: Vec<SecurityGroupId>,
    pub state: ResourceState,
    pub created_at: u64,
}

/// Unique identifier for a VPC.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct VpcId(pub String);

impl fmt::Display for VpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The owner of a VPC — either a project or an org (for shared VPCs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpcOwner {
    Project(ProjectId),
    Org(OrgId),
}

/// A Virtual Private Cloud — one VPC = one VXLAN VNI = one isolated L2 domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vpc {
    pub id: VpcId,
    pub name: String,
    pub cidr: String,
    pub vni: u32,
    pub owner: VpcOwner,
    pub shared: bool,
    pub created_at: u64,
}

/// Unique identifier for an organization.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct OrgId(pub String);

impl fmt::Display for OrgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub String);

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for an environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentId(pub String);

impl fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An organization — the root tenant in the Syfrah hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Org {
    pub id: OrgId,
    pub name: String,
    pub created_at: u64,
}

/// A project within an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub org_id: OrgId,
    pub created_at: u64,
}

/// A record of a shared VPC being attached to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpcAttachment {
    pub vpc_name: String,
    pub project_id: ProjectId,
    pub attached_at: u64,
}

/// Unique identifier for a subnet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubnetId(pub String);

impl fmt::Display for SubnetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A subnet within a VPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subnet {
    pub id: SubnetId,
    pub name: String,
    pub vpc_id: VpcId,
    pub env_id: EnvironmentId,
    pub cidr: String,
    pub gateway: String,
    pub created_at: u64,
}

/// Unique identifier for a VPC peering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeeringId(pub String);

impl fmt::Display for PeeringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Status of a VPC peering connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeeringStatus {
    Active,
    Pending,
    Deleted,
}

impl fmt::Display for PeeringStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeeringStatus::Active => f.write_str("Active"),
            PeeringStatus::Pending => f.write_str("Pending"),
            PeeringStatus::Deleted => f.write_str("Deleted"),
        }
    }
}

/// A VPC peering connection between two VPCs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpcPeering {
    pub id: PeeringId,
    pub vpc_a: String,
    pub vpc_b: String,
    pub status: PeeringStatus,
    pub created_at: u64,
}

/// An environment — a runtime context within a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id: EnvironmentId,
    pub name: String,
    pub project_id: ProjectId,
    pub ttl: Option<u64>,
    pub deletion_protection: bool,
    pub labels: HashMap<String, String>,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

/// Unique identifier for a security group rule.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Traffic direction for a security group rule.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Ingress,
    Egress,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Ingress => f.write_str("Ingress"),
            Direction::Egress => f.write_str("Egress"),
        }
    }
}

/// Network protocol for a security group rule.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
    Icmp,
    All,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Tcp => f.write_str("TCP"),
            Protocol::Udp => f.write_str("UDP"),
            Protocol::Icmp => f.write_str("ICMP"),
            Protocol::All => f.write_str("All"),
        }
    }
}

/// A port range (inclusive). Single port: from == to.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRange {
    pub from: u16,
    pub to: u16,
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.from == self.to {
            write!(f, "{}", self.from)
        } else {
            write!(f, "{}-{}", self.from, self.to)
        }
    }
}

/// The source (for ingress) or destination (for egress) of traffic.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    Cidr(String),
    SecurityGroup(SecurityGroupId),
}

impl fmt::Display for RuleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleSource::Cidr(cidr) => write!(f, "{cidr}"),
            RuleSource::SecurityGroup(sg_id) => write!(f, "sg:{sg_id}"),
        }
    }
}

/// A rule within a security group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityGroupRule {
    pub id: RuleId,
    pub sg_id: SecurityGroupId,
    pub direction: Direction,
    pub protocol: Protocol,
    pub port_range: Option<PortRange>,
    pub source: RuleSource,
    pub priority: u32,
    pub description: Option<String>,
}

/// Unique identifier for a route table.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteTableId(pub String);

impl fmt::Display for RouteTableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A route table — a collection of routes scoped to a VPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteTable {
    pub id: RouteTableId,
    pub name: String,
    pub vpc_id: VpcId,
    pub is_default: bool,
    pub state: ResourceState,
    pub created_at: u64,
}

/// Unique identifier for a route.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteId(pub String);

impl fmt::Display for RouteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The target of a route — where matching traffic is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteTarget {
    Local,
    NatGateway(String),
    VpcPeering(String),
    Blackhole,
}

impl fmt::Display for RouteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteTarget::Local => f.write_str("local"),
            RouteTarget::NatGateway(id) => write!(f, "nat-gw:{id}"),
            RouteTarget::VpcPeering(id) => write!(f, "peering:{id}"),
            RouteTarget::Blackhole => f.write_str("blackhole"),
        }
    }
}

/// How a route was created — determines deletion rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteOrigin {
    System,
    User,
    Propagated,
}

impl fmt::Display for RouteOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteOrigin::System => f.write_str("system"),
            RouteOrigin::User => f.write_str("user"),
            RouteOrigin::Propagated => f.write_str("propagated"),
        }
    }
}

/// Status of a route — whether it is actively forwarding traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteStatus {
    Active,
    Blackhole,
}

impl fmt::Display for RouteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteStatus::Active => f.write_str("active"),
            RouteStatus::Blackhole => f.write_str("blackhole"),
        }
    }
}

/// A route within a route table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub id: RouteId,
    pub route_table_id: RouteTableId,
    pub destination: String,
    pub target: RouteTarget,
    pub origin: RouteOrigin,
    pub status: RouteStatus,
    pub priority: u32,
    pub created_at: u64,
}

/// Unique identifier for a NAT Gateway.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct NatGatewayId(pub String);

impl fmt::Display for NatGatewayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A NAT Gateway — provides SNAT masquerade for a VPC's subnets.
///
/// Placed in a specific subnet, uses the node's public IP for outbound traffic.
/// State transitions: Pending → Active (nftables applied), Pending → Failed,
/// Active → Deleting → Deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatGateway {
    pub id: NatGatewayId,
    pub name: String,
    pub vpc_id: VpcId,
    pub subnet_id: SubnetId,
    pub public_ip: String,
    pub state: ResourceState,
    pub created_at: u64,
}

/// Whether a VM placement is being added or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementAction {
    Add,
    Remove,
}

impl fmt::Display for PlacementAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlacementAction::Add => f.write_str("Add"),
            PlacementAction::Remove => f.write_str("Remove"),
        }
    }
}

/// Tracks which node a VM is placed on, along with its network coordinates.
///
/// Persisted in the `vm_placements` redb table (key: "vpc_id/vm_id").
/// Used for FDB distribution: when a VM is created or deleted, all nodes
/// in the VPC must update their forwarding tables accordingly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmPlacement {
    pub vpc_id: String,
    pub vm_id: String,
    pub vm_mac: String,
    pub vm_ip: String,
    pub subnet_id: String,
    pub hosting_node: String,
    pub action: PlacementAction,
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Hypervisor model (ADR-004)
// ---------------------------------------------------------------------------

/// Unique identifier for a hypervisor. Format: `hv-{ulid}`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct HypervisorId(pub String);

impl fmt::Display for HypervisorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle state for a hypervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HypervisorState {
    /// Hardware detection in progress.
    Registering,
    /// Registered but not schedulable.
    NotReady,
    /// Healthy and schedulable.
    Available,
    /// Draining — no new VMs, existing VMs being evacuated.
    Draining,
    /// Offline for planned work.
    Maintenance,
    /// Permanently removed. Terminal state.
    Decommissioned,
}

impl fmt::Display for HypervisorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HypervisorState::Registering => f.write_str("Registering"),
            HypervisorState::NotReady => f.write_str("NotReady"),
            HypervisorState::Available => f.write_str("Available"),
            HypervisorState::Draining => f.write_str("Draining"),
            HypervisorState::Maintenance => f.write_str("Maintenance"),
            HypervisorState::Decommissioned => f.write_str("Decommissioned"),
        }
    }
}

/// Disk type detected on the hypervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskType {
    NVMe,
    SSD,
    HDD,
}

impl fmt::Display for DiskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiskType::NVMe => f.write_str("NVMe"),
            DiskType::SSD => f.write_str("SSD"),
            DiskType::HDD => f.write_str("HDD"),
        }
    }
}

/// CPU architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuArchitecture {
    X86_64,
    Aarch64,
}

impl fmt::Display for CpuArchitecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuArchitecture::X86_64 => f.write_str("x86_64"),
            CpuArchitecture::Aarch64 => f.write_str("aarch64"),
        }
    }
}

/// GPU specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuSpec {
    pub model: String,
    pub vram_mb: u32,
    pub count: u32,
}

/// Detected hardware specifications of a hypervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareSpec {
    pub cpu_model: String,
    pub cpu_cores_physical: u32,
    pub cpu_threads_logical: u32,
    pub memory_gb: u32,
    pub local_disk_type: DiskType,
    pub local_disk_gb: u32,
    pub gpu: Option<GpuSpec>,
    pub network_bandwidth_gbps: u32,
    pub architecture: CpuArchitecture,
}

/// Allocatable capacity on a hypervisor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllocatableCapacity {
    pub physical_vcpus: u32,
    pub physical_memory_mb: u64,
    pub allocatable_vcpus: u32,
    pub allocatable_memory_mb: u64,
    pub used_vcpus: u32,
    pub used_memory_mb: u64,
    pub available_vcpus: u32,
    pub available_memory_mb: u64,
    pub reserved_vcpus: u32,
    pub reserved_memory_mb: u64,
    pub overcommit_cpu: f32,
    pub overcommit_memory: f32,
    pub local_total_gb: u32,
    pub local_used_gb: u32,
    pub local_allocatable_gb: u32,
}

impl Default for AllocatableCapacity {
    fn default() -> Self {
        Self {
            physical_vcpus: 0,
            physical_memory_mb: 0,
            allocatable_vcpus: 0,
            allocatable_memory_mb: 0,
            used_vcpus: 0,
            used_memory_mb: 0,
            available_vcpus: 0,
            available_memory_mb: 0,
            reserved_vcpus: 1,
            reserved_memory_mb: 1024,
            overcommit_cpu: 2.0,
            overcommit_memory: 1.0,
            local_total_gb: 0,
            local_used_gb: 0,
            local_allocatable_gb: 0,
        }
    }
}

/// Taint effect — what happens to VMs that don't tolerate this taint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaintEffect {
    NoSchedule,
    NoExecute,
}

impl fmt::Display for TaintEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaintEffect::NoSchedule => f.write_str("NoSchedule"),
            TaintEffect::NoExecute => f.write_str("NoExecute"),
        }
    }
}

/// A scheduling taint on a hypervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Taint {
    pub key: String,
    pub value: Option<String>,
    pub effect: TaintEffect,
}

impl fmt::Display for Taint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Some(v) => write!(f, "{}={}:{}", self.key, v, self.effect),
            None => write!(f, "{}:{}", self.key, self.effect),
        }
    }
}

/// Runtime status of a hypervisor (observed, not persisted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HypervisorStatus {
    pub hypervisor_id: HypervisorId,
    pub last_heartbeat: u64,
    pub reachable: bool,
    pub forge_version: String,
    pub uptime_seconds: u64,
}

/// A hypervisor — a compute host that can run VMs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hypervisor {
    pub id: HypervisorId,
    pub name: String,
    pub region: String,
    pub zone: String,
    pub state: HypervisorState,
    pub fabric_node_id: String,
    pub public_ip: String,
    pub fabric_ipv6: String,
    pub hardware: HardwareSpec,
    pub capacity: AllocatableCapacity,
    pub labels: HashMap<String, String>,
    pub taints: Vec<Taint>,
    pub created_at: u64,
}

/// Gossip report published by hypervisor nodes.
///
/// Replaces the generic NodeReport for compute-capable nodes.
/// The scheduler and control plane consume this for placement decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HypervisorReport {
    pub hypervisor_id: HypervisorId,
    pub fabric_node_id: String,
    pub state: HypervisorState,
    pub capacity: AllocatableCapacity,
    pub vm_count: u32,
    pub host_cpu_percent: f32,
    pub host_memory_percent: f32,
    pub host_disk_percent: f32,
    pub labels: HashMap<String, String>,
    pub taints: Vec<Taint>,
    pub timestamp: u64,
}

impl HypervisorReport {
    /// Build a report from a hypervisor record and runtime stats.
    pub fn from_hypervisor(hv: &Hypervisor, vm_count: u32) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            hypervisor_id: hv.id.clone(),
            fabric_node_id: hv.fabric_node_id.clone(),
            state: hv.state.clone(),
            capacity: hv.capacity.clone(),
            vm_count,
            host_cpu_percent: 0.0,
            host_memory_percent: 0.0,
            host_disk_percent: 0.0,
            labels: hv.labels.clone(),
            taints: hv.taints.clone(),
            timestamp: now,
        }
    }
}
