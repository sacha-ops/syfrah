# ADR-004: Hypervisor Model — Region/Zone/Hypervisor/VM Topology

**Status**: Proposed
**Date**: 2026-03-30
**Decided by**: Sacha + team
**Supersedes**: Informal "node" usage in compute/overlay/forge layers

## Context

The platform uses "node" loosely across layers. In the fabric, a node is any WireGuard mesh participant — a hypervisor, a router, a control-plane-only box, a monitoring appliance. In the compute and forge layers, "node" implicitly means "a server that runs VMs," but this is never formalized. The result is ambiguity:

- ADR-001 uses `hosting_node` in `VmPlacement` — is that a fabric node ID or a compute concept?
- ADR-003 Forge uses `/v1/node/*` API paths — but Forge on a non-VM-hosting node has a different role than Forge on a hypervisor.
- The gossip layer reports `NodeReport` — but the scheduler only cares about nodes that can host VMs.
- The zones-and-regions model defines topology down to "node" but never defines what makes a node schedulable for compute.

Every cloud provider distinguishes between "network participant" and "compute host." AWS has hosts (Nitro hypervisors) inside AZs. GCP has Borglet machines. Azure has Host Agents on hypervisors. The common pattern: not every machine in the infrastructure runs tenant workloads. Some are routers, some are control plane, some are monitoring. The compute scheduler needs a first-class resource representing "a machine that can run VMs" — with hardware specs, allocatable capacity, lifecycle state, placement labels, and scheduling constraints.

This ADR introduces the **Hypervisor** as that first-class resource. It formalizes the topology hierarchy (Region → Zone → Hypervisor → VM), clarifies the node-vs-hypervisor boundary, defines the resource model, and specifies every API, CLI, lifecycle, and integration point.

### Relationship to existing decisions

- **ARCHITECTURE.md** — The stack diagram shows Forge per node. This ADR clarifies: Forge runs on all nodes, but compute management (VM lifecycle, capacity reporting, scheduling) is only active on hypervisor nodes.
- **ADR-001** — `VmPlacement.hosting_node` becomes `VmPlacement.hypervisor_id`. Subnet/VPC/IPAM concepts are unchanged.
- **ADR-002** — Security groups, route tables, NAT gateways attach to NICs. The hypervisor is the execution environment — NICs live on VMs, VMs live on hypervisors. No changes to the network policy model.
- **ADR-003 (Forge)** — Forge is the per-node orchestrator. On hypervisor nodes, Forge manages VMs + network + storage. On non-hypervisor nodes, Forge manages network transit (bridges, VXLANs, FDB for overlay forwarding) but has no compute responsibilities. The `/v1/node/*` API paths become `/v1/hypervisor/*` for compute-related endpoints.
- **zones-and-regions.md** — Regions and zones are metadata on fabric nodes. This ADR adds a layer: within a zone, hypervisors are the schedulable compute hosts. The topology becomes Region → Zone → Hypervisor → VM.

## Decision

### 1. Topology hierarchy

The full resource hierarchy from geography down to workload:

```
Region (eu-west, us-east, ap-south)
  └── Zone (eu-west-1, eu-west-ovh, us-east-fsn)
        └── Hypervisor (hv-001, hv-002, ...)
              └── VM Instance (web-1, db-1, gpu-train-1)
```

Regions and zones are **metadata labels** on fabric nodes (unchanged from zones-and-regions.md). Hypervisors are **first-class resources** created when a node with KVM capability joins the mesh or is explicitly registered by the operator. VMs are scheduled onto hypervisors by the control plane.

```
    Region: eu-west                    Region: eu-central
    ┌────────────────────────┐         ┌──────────────────────┐
    │                        │         │                      │
    │  Zone: eu-west-1       │         │  Zone: eu-cent-1     │
    │  ┌──────┐ ┌──────┐    │         │  ┌──────┐            │
    │  │hv-001│ │hv-002│    │         │  │hv-005│            │
    │  │64 CPU│ │32 CPU│    │         │  │32 CPU│            │
    │  └──┬───┘ └──┬───┘    │         │  └──┬───┘            │
    │     │        │        │         │     │                │
    │  [web-1]  [web-2]     │         │  [web-3]             │
    │  [gpu-1]  [db-1 ]     │         │                      │
    │                        │         │                      │
    │  Zone: eu-west-2       │         │  Zone: eu-cent-2     │
    │  ┌──────┐ ┌──────┐    │         │  (no hypervisors)    │
    │  │hv-003│ │hv-004│    │         │                      │
    │  │32 CPU│ │router│    │         │                      │
    │  └──┬───┘ └──────┘    │         │                      │
    │     │     (not a       │         │                      │
    │  [db-2]  hypervisor)   │         │                      │
    │                        │         │                      │
    └────────────────────────┘         └──────────────────────┘
```

Note `hv-004` labeled "router" — it is a fabric node but NOT a hypervisor. It participates in the WireGuard mesh, can run Forge for network transit duties (VXLAN forwarding, FDB population), but is not in the scheduler's placement pool. This distinction is the core of this ADR.

### 2. Node vs. Hypervisor — the boundary

| Concept | Layer | Definition | Example |
|---------|-------|------------|---------|
| **Node** | Fabric | Any server running syfrah that participates in the WireGuard mesh. Has a fabric IPv6 address, a WireGuard keypair, and peer connections. | Router, monitoring box, control-plane-only server, hypervisor |
| **Hypervisor** | Compute | A node that hosts VM instances via KVM/Cloud Hypervisor. Has hardware specs, allocatable capacity, placement constraints, and a lifecycle state. | Bare-metal server with `/dev/kvm`, registered as a hypervisor |

The relationship is strict:

- **Every hypervisor IS a fabric node.** It has a `fabric_node_id` linking it to its mesh identity.
- **Not every node is a hypervisor.** A node without KVM, or a node deliberately not registered as a hypervisor, is just a mesh participant.
- **The fabric layer does not know about hypervisors.** It manages WireGuard peers, IPv6 addressing, and mesh topology. It neither creates nor queries hypervisor records.
- **The compute layer, Forge, and the scheduler DO know about hypervisors.** Hypervisors are the schedulable units for VM placement.

### 3. Hypervisor as a first-class resource

```rust
struct Hypervisor {
    /// Globally unique identifier. Format: hv-{ulid}.
    /// Assigned at registration time. Immutable.
    id: HypervisorId,

    /// Operator-assigned name, unique within the mesh.
    /// Typically matches the fabric node name (e.g., "par-hv-1").
    name: String,

    /// Geographic region this hypervisor belongs to.
    /// Inherited from the fabric node's region label at registration.
    region: String,

    /// Availability zone within the region.
    /// Inherited from the fabric node's zone label at registration.
    zone: String,

    /// Current lifecycle state.
    state: HypervisorState,

    /// Link to the underlying fabric node.
    /// This is the bridge between the compute and fabric layers.
    fabric_node_id: NodeId,

    /// Public IPv4/IPv6 address of the server (for external reachability).
    public_ip: String,

    /// Fabric IPv6 address (syfrah0 interface).
    /// Used for all intra-mesh communication: Forge API, VXLAN VTEP, gossip.
    fabric_ipv6: String,

    /// Detected hardware specifications.
    hardware: HardwareSpec,

    /// Current allocatable capacity (updated every reconciliation cycle).
    capacity: AllocatableCapacity,

    /// Arbitrary key-value labels for placement (e.g., gpu=a100, tier=premium).
    labels: HashMap<String, String>,

    /// Scheduling taints — prevent placement unless tolerated by the VM spec.
    taints: Vec<Taint>,

    /// Unix timestamp (seconds) when this hypervisor was registered.
    created_at: u64,

    /// Unix timestamp of the last heartbeat received via gossip.
    /// Used by the scheduler to determine liveness.
    last_heartbeat: u64,
}
```

### 4. HardwareSpec — what the machine has

Detected at registration time by probing the host system. Updated on restart if hardware changes (e.g., after a RAM upgrade or GPU installation).

```rust
struct HardwareSpec {
    /// CPU model string (e.g., "AMD EPYC 7763", "Intel Xeon E-2388G").
    /// Parsed from /proc/cpuinfo.
    cpu_model: String,

    /// Physical CPU cores (excludes SMT/HT threads).
    cpu_cores_physical: u32,

    /// Logical CPU threads (physical cores * threads-per-core).
    /// With SMT enabled on a 64-core EPYC: 128 logical threads.
    cpu_threads_logical: u32,

    /// Total installed RAM in gigabytes.
    /// Parsed from /proc/meminfo (MemTotal).
    memory_gb: u32,

    /// Primary storage type.
    disk_type: DiskType,

    /// Total usable disk capacity in gigabytes.
    /// Parsed from lsblk. Excludes the OS partition.
    disk_gb: u32,

    /// GPU specification, if present. None for CPU-only servers.
    gpu: Option<GpuSpec>,

    /// Network interface bandwidth in Gbps.
    /// Detected from ethtool or sysfs, or operator-specified.
    network_bandwidth_gbps: u32,

    /// CPU architecture.
    architecture: CpuArchitecture,
}

enum DiskType {
    NVMe,
    SSD,
    HDD,
}

enum CpuArchitecture {
    X86_64,
    Aarch64,
}

struct GpuSpec {
    /// GPU model (e.g., "NVIDIA A100 80GB", "NVIDIA L4").
    model: String,

    /// Video RAM per GPU in megabytes.
    vram_mb: u32,

    /// Number of GPUs available for passthrough.
    count: u32,
}
```

### 5. AllocatableCapacity — what's available for VMs

Capacity is the bridge between hardware reality and scheduling decisions. The hypervisor knows its total resources, what's consumed by the OS and Syfrah overhead, what's reserved, and what's free for new VMs.

```rust
struct AllocatableCapacity {
    // --- CPU ---
    /// Total vCPUs available for allocation.
    /// Computed as: cpu_threads_logical * overcommit_cpu.
    /// Example: 128 threads * 2.0 overcommit = 256 allocatable vCPUs.
    total_vcpus: u32,

    /// vCPUs currently allocated to running VMs.
    used_vcpus: u32,

    /// vCPUs available for new VMs.
    /// Computed as: total_vcpus - used_vcpus - reserved_vcpus.
    allocatable_vcpus: u32,

    // --- Memory ---
    /// Total memory available for allocation in MB.
    /// Computed as: (memory_gb * 1024) * overcommit_memory - reserved_memory_mb.
    total_memory_mb: u64,

    /// Memory currently allocated to running VMs in MB.
    used_memory_mb: u64,

    /// Memory available for new VMs in MB.
    allocatable_memory_mb: u64,

    // --- Disk ---
    /// Total disk available for VM volumes in GB.
    total_disk_gb: u32,

    /// Disk currently consumed by VM volumes in GB.
    used_disk_gb: u32,

    /// Disk available for new VM volumes in GB.
    allocatable_disk_gb: u32,

    // --- Reservations ---
    /// vCPUs reserved for the host OS and Syfrah daemon overhead.
    /// Default: 2 vCPUs.
    reserved_vcpus: u32,

    /// Memory reserved for the host OS and Syfrah daemon overhead in MB.
    /// Default: 2048 MB (2 GB).
    reserved_memory_mb: u64,

    // --- Overcommit ratios ---
    /// CPU overcommit ratio. Default: 2.0.
    /// A ratio of 2.0 means a 128-thread machine can allocate 256 vCPUs.
    /// Set to 1.0 for dedicated/performance-sensitive workloads.
    overcommit_cpu: f32,

    /// Memory overcommit ratio. Default: 1.0 (no overcommit).
    /// Memory overcommit is dangerous — OOM kills are destructive.
    /// Only increase if workloads are known to be memory-sparse.
    overcommit_memory: f32,
}
```

**Why memory overcommit defaults to 1.0**: CPU overcommit is safe — the Linux scheduler time-slices transparently. Memory overcommit is not — when physical memory is exhausted, the OOM killer terminates processes. In a cloud platform where tenants expect VM isolation, OOM-killing a tenant's VM because a neighbor overallocated is unacceptable. Memory overcommit is available for operators who understand the risk (e.g., dev/test environments with bursty workloads) but defaults to conservative.

**Capacity update frequency**: Forge updates `AllocatableCapacity` on every reconciliation cycle (default 10 seconds). The updated capacity is reported via gossip so the scheduler has a near-real-time view.

### 6. HypervisorState lifecycle

A hypervisor progresses through a well-defined state machine:

```
                      register
     ───────────────────────────────────► Available
                                            │
                             drain          │         undrain
                       ◄───────────────── │ ────────────────►
                     Draining              │             Available
                       │                   │
                       │ all VMs           │ maintenance
                       │ migrated/stopped  │
                       ▼                   ▼
                    Available         Maintenance
                                           │
                                           │ decommission
                                           ▼
                                     Decommissioned
                                       (terminal)
```

```rust
enum HypervisorState {
    /// Ready to accept new VM placements.
    /// The scheduler includes this hypervisor in its placement pool.
    Available,

    /// No new VMs will be scheduled here. Existing VMs are being
    /// live-migrated or gracefully stopped.
    /// Transitions to Available when all VMs are gone (or undrained).
    Draining,

    /// Operator-initiated maintenance window. No new VMs, no drain
    /// activity — the hypervisor is being serviced (firmware update,
    /// hardware replacement, OS upgrade).
    Maintenance,

    /// Permanently removed from the platform. The hypervisor record
    /// is retained for audit but is never scheduled again.
    /// Terminal state — no transitions out.
    Decommissioned,
}
```

**State transition rules:**

| From | To | Trigger | Precondition |
|------|----|---------|--------------|
| (new) | Available | `hypervisor register` | Node has KVM, hardware detected |
| Available | Draining | `hypervisor drain` | — |
| Draining | Available | `hypervisor undrain` | — |
| Draining | Available | (automatic) | All VMs migrated/stopped, drain complete |
| Available | Maintenance | `hypervisor maintenance` | — |
| Maintenance | Available | `hypervisor undrain` | — |
| Available | Decommissioned | `hypervisor decommission` | No running VMs (must drain first) |
| Maintenance | Decommissioned | `hypervisor decommission` | No running VMs |
| Decommissioned | (none) | — | Terminal state |

**Scheduler behavior by state:**

| State | Accepts new VMs | Existing VMs | In gossip | In placement pool |
|-------|----------------|--------------|-----------|-------------------|
| Available | Yes | Running | Yes | Yes |
| Draining | No | Being migrated/stopped | Yes | No |
| Maintenance | No | Should be drained first | Yes | No |
| Decommissioned | No | None (enforced) | No | No |

### 7. Taints and tolerations

Inspired by Kubernetes, taints and tolerations control which VMs can be scheduled on which hypervisors.

**Taint** — applied to a hypervisor. Repels VMs unless they explicitly tolerate the taint.

```rust
struct Taint {
    /// Taint key (e.g., "dedicated", "gpu-only", "maintenance").
    key: String,

    /// Taint value (e.g., "org-acme", "true"). Optional — some taints are key-only.
    value: Option<String>,

    /// Effect determines what happens to VMs that don't tolerate this taint.
    effect: TaintEffect,
}

enum TaintEffect {
    /// Don't schedule new VMs on this hypervisor unless they tolerate the taint.
    /// Existing VMs are unaffected.
    NoSchedule,

    /// Like NoSchedule, but also evict existing VMs that don't tolerate the taint.
    /// Used for maintenance and decommission taints.
    NoExecute,
}
```

**Toleration** — applied to a VM spec. Allows the VM to be scheduled on a tainted hypervisor.

```rust
struct Toleration {
    /// Must match the taint key.
    key: String,

    /// Must match the taint value. None means "match any value."
    value: Option<String>,

    /// Must match the taint effect.
    effect: TaintEffect,
}
```

**Matching rules:**
- A toleration matches a taint if `key` matches, `effect` matches, and (`value` matches OR toleration value is None).
- A VM can be scheduled on a hypervisor only if it tolerates ALL taints on that hypervisor.
- A VM with no tolerations can only be scheduled on untainted hypervisors.

**Built-in taints:**

| Taint | Applied when | Effect | Purpose |
|-------|-------------|--------|---------|
| `syfrah.io/draining` | `hypervisor drain` | NoSchedule | Prevent new VMs during drain |
| `syfrah.io/maintenance` | `hypervisor maintenance` | NoExecute | Evict VMs for maintenance |
| `syfrah.io/unreachable` | Gossip timeout (60s) | NoSchedule | Scheduler avoids unresponsive nodes |

**Use cases:**

- **Dedicated hypervisors for a specific org** (compliance, noisy-neighbor isolation):
  ```bash
  syfrah hypervisor taint hv-001 --add dedicated=org-acme:NoSchedule
  ```
  Only VMs with `tolerations: [{key: "dedicated", value: "org-acme", effect: "NoSchedule"}]` can land on hv-001.

- **GPU-only hypervisors** (prevent non-GPU workloads from consuming GPU host resources):
  ```bash
  syfrah hypervisor taint hv-003 --add gpu-only=true:NoSchedule
  ```

- **Maintenance window** (auto-applied):
  ```bash
  syfrah hypervisor maintenance hv-002
  # Automatically adds taint: syfrah.io/maintenance:NoExecute
  # Existing VMs are evicted (migrated to other hypervisors)
  ```

### 8. Hypervisor registration

A fabric node becomes a hypervisor through registration. Two paths:

#### Automatic registration

When a node joins the mesh via `syfrah fabric join` (or `syfrah fabric init`), the join process checks for KVM capability:

1. **Detect KVM**: check for `/dev/kvm` and verify it is accessible (open + `KVM_GET_API_VERSION` ioctl).
2. **Detect hardware**: parse system information:
   - `/proc/cpuinfo` → CPU model, physical cores, logical threads
   - `/proc/meminfo` → total memory (MemTotal)
   - `lspci` (or `/sys/bus/pci/devices/`) → GPU model, VRAM, count (filter for VGA/3D controllers with NVIDIA/AMD vendor IDs)
   - `lsblk --json` → disk type (rotational flag, transport), usable capacity (exclude OS partition)
   - `ethtool` or `/sys/class/net/*/speed` → NIC bandwidth
   - `uname -m` → CPU architecture
3. **Create hypervisor record** with detected specs, region/zone inherited from the fabric node's labels.
4. **State**: `Available`.
5. **Persist** the record in the control-plane store (redb in bootstrap mode, Raft in distributed mode).

If KVM is not present (`/dev/kvm` does not exist or is inaccessible), the node joins the mesh as a fabric node only. No hypervisor record is created. The node can still run Forge for network transit (VXLAN forwarding, FDB population, bridge management) but is not in the scheduler's placement pool.

#### Manual registration

For nodes where auto-detection is insufficient or where the operator wants to override detected values:

```bash
syfrah hypervisor register \
  --region eu-west \
  --zone eu-west-1 \
  --label gpu=a100 \
  --label tier=premium \
  --overcommit-cpu 1.5 \
  --reserved-vcpus 4 \
  --reserved-memory-mb 4096
```

This creates a hypervisor record on the current node, overriding auto-detected region/zone if specified, and applying custom labels and capacity parameters.

**Registration is idempotent**: running `hypervisor register` on a node that is already a hypervisor updates the record (re-detects hardware, applies new labels/overrides) without creating a duplicate.

### 9. Hypervisor auto-discovery on restart

When Forge starts on a hypervisor node:

1. **Load hypervisor record** from the local store.
2. **Re-detect hardware** — compare with stored `HardwareSpec`. If hardware changed (e.g., RAM upgrade, new GPU), update the record and log the change.
3. **Recompute capacity** — scan running VMs, sum allocated resources, update `AllocatableCapacity`.
4. **Resume state** — if the hypervisor was `Available` before shutdown, it returns to `Available`. If it was `Draining`, it resumes draining (checks if drain is complete). `Maintenance` and `Decommissioned` are sticky.
5. **Begin heartbeat** — start reporting via gossip.

### 10. Relationship to Forge

Forge runs on every node. The hypervisor model determines what Forge does:

| Forge capability | Hypervisor node | Non-hypervisor node |
|-----------------|-----------------|---------------------|
| VM lifecycle (create, start, stop, delete) | Yes | No |
| Capacity tracking and reporting | Yes | No |
| Admission control for VM placement | Yes | No |
| Linux bridges and VXLAN interfaces | Yes | Yes (network transit) |
| FDB population | Yes | Yes |
| nftables rules (security groups) | Yes (per-VM) | No |
| nftables rules (infrastructure) | Yes | Yes |
| Health reporting via gossip | Yes (full `HypervisorReport`) | Yes (basic `NodeReport`) |
| Drain/maintenance lifecycle | Yes | No |

**Forge API path changes:**

The Forge REST API (`http://[fabric_ipv6]:7100/...`) uses `/v1/hypervisor/*` for compute-related endpoints on hypervisor nodes:

| Old path (ADR-003) | New path | Notes |
|--------------------|----------|-------|
| `GET /v1/node/status` | `GET /v1/hypervisor/status` | On hypervisor nodes. Non-hypervisor nodes continue to use `/v1/node/status` for basic health. |
| `GET /v1/node/health` | `GET /v1/hypervisor/health` | Full 4-category health check on hypervisor nodes. |
| `GET /v1/node/capacity` | `GET /v1/hypervisor/capacity` | Only meaningful on hypervisor nodes. Returns 404 on non-hypervisors. |
| `GET /v1/node/metrics` | `GET /v1/hypervisor/metrics` | Compute metrics (VM count, vCPU utilization, etc.). |
| `POST /v1/node/drain` | `POST /v1/hypervisor/drain` | Hypervisor drain lifecycle. |
| `POST /v1/node/undrain` | `POST /v1/hypervisor/undrain` | Resume scheduling. |
| `GET /v1/node/resources` | `GET /v1/hypervisor/resources` | Summary of managed resources. |
| `POST /v1/node/reconcile` | `POST /v1/hypervisor/reconcile` | Trigger immediate reconciliation. |

Non-hypervisor nodes retain a minimal `/v1/node/*` API surface for fabric-level health and network transit status.

### 11. Gossip: HypervisorReport

Hypervisor nodes publish a `HypervisorReport` via gossip (replacing the generic `NodeReport` for compute-capable nodes). The scheduler and control plane consume this report for placement decisions and dashboard display.

```rust
struct HypervisorReport {
    /// The hypervisor this report describes.
    hypervisor_id: HypervisorId,

    /// Fabric node identity (for mesh-level correlation).
    fabric_node_id: NodeId,

    /// Current lifecycle state.
    state: HypervisorState,

    /// Current allocatable capacity snapshot.
    capacity: AllocatableCapacity,

    /// Number of running VMs on this hypervisor.
    vm_count: u32,

    /// Aggregate health status (composite of 4 health categories from Forge).
    health: HealthStatus,

    /// Host-level resource utilization (real, not allocated).
    /// Used for scheduling heuristics — prefer hypervisors with lower actual load.
    host_cpu_percent: f32,
    host_memory_percent: f32,
    host_disk_percent: f32,

    /// Labels (copied from hypervisor record for fast scheduler filtering).
    labels: HashMap<String, String>,

    /// Taints (copied from hypervisor record for fast scheduler filtering).
    taints: Vec<Taint>,

    /// Unix timestamp when this report was generated.
    timestamp: u64,
}
```

Non-hypervisor nodes continue to publish a lightweight `NodeReport` with basic health and network metrics — no capacity, no VM count, no labels/taints.

### 12. CLI

Full CLI surface for hypervisor management:

```bash
# ─── Listing and inspection ───

# List all hypervisors in the mesh
syfrah hypervisor list [--region <region>] [--zone <zone>] [--state <state>] [--json]
# Output:
#   ID       NAME      REGION    ZONE        STATE      VMs  vCPU (used/total)  MEM (used/total)
#   hv-001   par-hv-1  eu-west   eu-west-1   Available  4    12/256             24G/256G
#   hv-002   par-hv-2  eu-west   eu-west-1   Available  2    6/128              12G/128G
#   hv-003   fsn-hv-1  eu-cent   eu-cent-1   Draining   1    4/128              8G/128G

# Get detailed hypervisor info
syfrah hypervisor get <name-or-id>
# Output:
#   Hypervisor: par-hv-1 (hv-01HXYZ...)
#   Region:     eu-west
#   Zone:       eu-west-1
#   State:      Available
#   Fabric:     fd12:3456:7800:a1b2::1
#   Public IP:  203.0.113.10
#
#   Hardware:
#     CPU:      AMD EPYC 7763 (64 cores, 128 threads)
#     Memory:   256 GB
#     Disk:     1.92 TB NVMe
#     GPU:      NVIDIA A100 80GB x2
#     Network:  25 Gbps
#     Arch:     x86_64
#
#   Capacity:
#     vCPUs:    12 used / 256 total (244 allocatable)
#     Memory:   24576 MB used / 260096 MB total (235520 MB allocatable)
#     Disk:     120 GB used / 1800 GB total (1680 GB allocatable)
#     Overcommit: CPU 2.0x, Memory 1.0x
#     Reserved: 2 vCPUs, 2048 MB
#
#   Labels:     gpu=a100, tier=premium
#   Taints:     (none)
#   VMs:        4 running
#   Heartbeat:  2s ago

# ─── Registration ───

# Register this node as a hypervisor (if not auto-detected)
syfrah hypervisor register \
  --region <region> --zone <zone> \
  [--label key=value ...] \
  [--overcommit-cpu <ratio>] \
  [--overcommit-memory <ratio>] \
  [--reserved-vcpus <n>] \
  [--reserved-memory-mb <n>]

# ─── Labels ───

# Set labels (additive — existing labels not in --set are preserved)
syfrah hypervisor label <name-or-id> --set gpu=a100 --set tier=premium

# Remove a label
syfrah hypervisor label <name-or-id> --remove gpu

# ─── Taints ───

# Add a taint
syfrah hypervisor taint <name-or-id> --add dedicated=org-acme:NoSchedule

# Remove a taint
syfrah hypervisor taint <name-or-id> --remove dedicated

# List taints on a hypervisor
syfrah hypervisor taint <name-or-id> --list

# ─── Lifecycle ───

# Drain: stop accepting new VMs, optionally migrate existing VMs
syfrah hypervisor drain <name-or-id> [--timeout 30m] [--force]

# Undrain: resume accepting VMs
syfrah hypervisor undrain <name-or-id>

# Maintenance: mark for operator servicing (auto-adds NoExecute taint)
syfrah hypervisor maintenance <name-or-id>

# Decommission: permanently remove from platform (terminal)
syfrah hypervisor decommission <name-or-id>

# ─── Status (local node) ───

# Show this node's hypervisor status
syfrah hypervisor status

# Show this node's capacity breakdown
syfrah hypervisor capacity
```

### 13. Placement constraints on vm create

The hypervisor model enables rich placement semantics. The scheduler evaluates constraints in order: zone → node-selector → taints → anti-affinity → capacity.

```bash
# Place in a specific zone
syfrah compute vm create --name web-1 --zone eu-west-1 ...

# Place on a hypervisor matching labels (node-selector)
syfrah compute vm create --name gpu-vm --node-selector gpu=a100 ...

# Place on a specific hypervisor (admin only — bypasses scheduler)
syfrah compute vm create --name web-1 --hypervisor hv-001 ...

# Anti-affinity: don't co-locate with VMs in this group
syfrah compute vm create --name web-2 --anti-affinity-group web-tier ...

# Spread across zones (topology-aware scheduling)
syfrah compute vm create --name web-3 --spread-topology zone ...
```

**Placement evaluation order:**

1. **Filter by state**: exclude hypervisors not in `Available` state.
2. **Filter by zone**: if `--zone` specified, only hypervisors in that zone.
3. **Filter by node-selector**: if `--node-selector` specified, only hypervisors whose labels match all selectors.
4. **Filter by taints**: exclude hypervisors with taints not tolerated by the VM spec.
5. **Filter by capacity**: exclude hypervisors without enough allocatable vCPUs, memory, and disk.
6. **Apply anti-affinity**: if `--anti-affinity-group` specified, prefer hypervisors that don't already host VMs in that group. Hard anti-affinity fails if no such hypervisor exists; soft anti-affinity (default) is best-effort.
7. **Apply spread-topology**: if `--spread-topology zone` specified, prefer zones with fewer VMs from this group.
8. **Score remaining candidates**: prefer hypervisors with lower actual utilization (from gossip `host_cpu_percent`, `host_memory_percent`). Bin-packing vs. spreading is configurable per-org (default: spreading for resilience).
9. **Select**: pick the highest-scoring hypervisor. Commit the placement decision via Raft. Send the create request to that hypervisor's Forge API.

### 14. Multi-hypervisor placement example

A realistic multi-region deployment showing how VMs are distributed across hypervisors:

```
Region: eu-west
├── Zone: eu-west-1
│   ├── Hypervisor: hv-001 (64c/128t, 256 GB, NVMe, GPU A100 x2)
│   │   ├── web-1        (2 vCPU, 4 GB)    [web-tier, spread:zone]
│   │   ├── gpu-train-1  (8 vCPU, 32 GB)   [gpu-only toleration]
│   │   └── monitoring-1 (1 vCPU, 2 GB)    [infra, spread:zone]
│   │
│   └── Hypervisor: hv-002 (32c/64t, 128 GB, SSD)
│       ├── web-2        (2 vCPU, 4 GB)    [web-tier, anti-affinity:web-1]
│       └── db-1         (4 vCPU, 16 GB)   [database-tier]
│
├── Zone: eu-west-2
│   └── Hypervisor: hv-003 (32c/64t, 128 GB, SSD)
│       ├── web-3        (2 vCPU, 4 GB)    [web-tier, spread:zone]
│       ├── db-2         (4 vCPU, 16 GB)   [database-tier, anti-affinity:db-1]
│       └── monitoring-2 (1 vCPU, 2 GB)    [infra, spread:zone]
│
└── Zone: eu-west-3
    └── (no hypervisors yet — expansion ready)

Region: us-east
├── Zone: us-east-1
│   └── Hypervisor: hv-010 (64c/128t, 256 GB, NVMe)
│       ├── api-us-1     (4 vCPU, 8 GB)
│       └── cache-us-1   (2 vCPU, 8 GB)
│
└── Zone: us-east-2
    └── Hypervisor: hv-011 (32c/64t, 128 GB, SSD)
        └── api-us-2     (4 vCPU, 8 GB)    [anti-affinity:api-us-1]
```

### 15. Scheduler integration

The scheduler runs in the control plane (Raft leader). It consumes hypervisor data from two sources:

**From gossip (real-time hints):**
- `HypervisorReport` — capacity snapshot, VM count, host utilization, health, labels, taints, state
- Used for candidate filtering and scoring
- Staleness: typically 2-5 seconds behind reality

**From the authoritative store (Raft):**
- Hypervisor records — canonical state, region, zone, labels, taints
- VM placement records — which VMs are on which hypervisors
- Anti-affinity group membership
- Used for constraint evaluation and placement commitment

**Scheduler flow:**

```
   1. Receive VM create request
          │
   2. Read all HypervisorReports from gossip cache
          │
   3. Filter: state == Available
          │
   4. Filter: zone constraint (if specified)
          │
   5. Filter: label selector match (if specified)
          │
   6. Filter: taint/toleration match
          │
   7. Filter: capacity >= requested resources
          │
   8. Score: anti-affinity, spread-topology, utilization
          │
   9. Select best hypervisor
          │
  10. Commit placement to Raft:
      VmPlacement { vm_id, hypervisor_id, ... }
          │
  11. Raft replicates → target hypervisor's Forge
      sees the new VM in its materialized view
          │
  12. Forge reconciliation creates the VM
```

**Admission control (double-check at Forge):**

Even after the scheduler selects a hypervisor, Forge performs local admission control before creating the VM. This guards against race conditions where the gossip capacity was stale and another VM was placed on the same hypervisor concurrently.

If Forge rejects the placement (insufficient capacity), it reports the rejection via gossip. The control plane retries placement on a different hypervisor.

### 16. Drain lifecycle

Draining a hypervisor is a controlled process that moves workloads off the machine before maintenance or decommission.

**Standard drain (`syfrah hypervisor drain hv-001 --timeout 30m`):**

1. Transition state to `Draining`.
2. Add taint `syfrah.io/draining:NoSchedule` — no new VMs.
3. For each running VM on this hypervisor (in priority order):
   a. Request the control plane to reschedule the VM to another hypervisor.
   b. The scheduler picks a new hypervisor (same zone preferred, same region required).
   c. The VM is stopped on the draining hypervisor and started on the new one.
   d. (Future: live migration instead of stop/start.)
4. If all VMs are drained within `--timeout`, transition to `Available` (with drain taint removed).
5. If timeout expires, report the remaining VMs. The operator can `--force` or extend.

**Force drain (`syfrah hypervisor drain hv-001 --force`):**

1. Transition state to `Draining`.
2. Immediately stop all VMs (graceful shutdown with 30-second SIGTERM → SIGKILL).
3. VMs are marked as `Stopped` — NOT rescheduled. The operator must manually restart them elsewhere.
4. Transition to `Available` when all VMs are stopped.

**Undrain (`syfrah hypervisor undrain hv-001`):**

1. Remove taint `syfrah.io/draining:NoSchedule`.
2. Cancel any in-progress drain operations.
3. Transition state to `Available`.
4. The hypervisor re-enters the scheduler's placement pool.

### 17. Deletion guards

Hypervisors are long-lived infrastructure. Accidental deletion or premature decommission can cause data loss and service disruption. The following guards are enforced:

| Operation | Guard | Error message |
|-----------|-------|---------------|
| `hypervisor decommission` | No running VMs | "Cannot decommission hypervisor with N running VMs. Drain first: `syfrah hypervisor drain <id>`" |
| `hypervisor decommission` | Not in `Available` or `Maintenance` state | "Cannot decommission hypervisor in state {state}. Expected Available or Maintenance." |
| `hypervisor drain` without `--timeout` or `--force` | Requires explicit timeout | "Drain requires --timeout <duration> or --force. Example: `syfrah hypervisor drain hv-001 --timeout 30m`" |
| `hypervisor drain --force` | Confirmation prompt | "Force drain will stop N VMs immediately. Type the hypervisor name to confirm:" |
| `hypervisor decommission` | Confirmation prompt | "Decommission is permanent. Type the hypervisor name to confirm:" |

**Decommissioned is terminal.** A decommissioned hypervisor record is retained indefinitely for audit (VM placement history, capacity records, event log). It is never reused, never re-registered. If the same physical server is repurposed, it gets a new hypervisor ID upon re-registration.

### 18. Persistence

| Data | Store | Update frequency | Notes |
|------|-------|-----------------|-------|
| Hypervisor record | redb (bootstrap) / Raft (distributed) | On registration, state change, label/taint mutation | Authoritative. Replicated via Raft to all nodes. |
| HardwareSpec | Embedded in hypervisor record | On registration and restart (re-detect) | Immutable between restarts unless hardware changes. |
| AllocatableCapacity | Local redb (fast path) + gossip | Every reconciliation cycle (10s) | Local copy updated by Forge. Gossip broadcasts snapshot for scheduler. |
| Labels | Embedded in hypervisor record | On operator mutation | Replicated via Raft. |
| Taints | Embedded in hypervisor record | On operator mutation or auto-taint (drain/maintenance) | Replicated via Raft. |
| HypervisorReport | Gossip (in-memory, ephemeral) | Every gossip dissemination cycle | Not persisted. Rebuilt from live state on restart. |

### 19. What changes in existing code

This ADR introduces renames and refactors across existing designs. Every instance is listed:

| Location | Current | New | Reason |
|----------|---------|-----|--------|
| ADR-001: `VmPlacement.hosting_node` | `hosting_node: FabricIpv6` | `hypervisor_id: HypervisorId` | VMs are placed on hypervisors, not generic nodes |
| ADR-003: Forge API paths | `/v1/node/*` | `/v1/hypervisor/*` | Compute endpoints are hypervisor-specific |
| ADR-003: gossip report | `NodeReport` (implicit) | `HypervisorReport` for hypervisors, `NodeReport` for non-hypervisors | Different data models for different node roles |
| ADR-003: Forge projection index | `node_id == this_node` | `hypervisor_id == this_hypervisor` (compute resources) + `node_id == this_node` (network resources) | Projection must distinguish compute vs. network resources |
| Forge capacity reporting | (not formalized) | `AllocatableCapacity` with overcommit, reservations | Formal capacity model required for scheduling |
| `syfrah compute vm list` output | `NODE` column | `HYPERVISOR` column | Reflects the correct concept |
| `syfrah compute vm create` | `--node <node>` (if it existed) | `--hypervisor <hv>` for pinning, `--zone <zone>` for zone constraint | Placement uses hypervisor terminology |
| Fabric layer | No changes | No changes | Fabric stays "node" — the mesh concept is lower level |

**Fabric remains unchanged.** The fabric layer uses "node" and "peer" exclusively. These are mesh-level concepts. The fabric does not know or care whether a node is a hypervisor. This separation is deliberate — it keeps the fabric simple and compute-unaware.

### 20. Hypervisor auto-discovery summary

```
    Node joins mesh (syfrah fabric join)
         │
         ▼
    /dev/kvm exists and accessible?
         │
    ┌────┴────┐
    │ Yes     │ No
    │         │
    ▼         ▼
    Detect    Fabric node only.
    hardware  Forge runs for network
    specs     transit (bridges, VXLAN,
    │         FDB). Not a hypervisor.
    │         Not in scheduler pool.
    │
    ▼
    Create Hypervisor record:
    ├── id: auto-generated (hv-{ulid})
    ├── name: fabric node name
    ├── region: from --region flag
    ├── zone: from --zone flag
    ├── state: Available
    ├── hardware: detected HardwareSpec
    ├── capacity: computed AllocatableCapacity
    ├── labels: empty (operator adds later)
    └── taints: empty
         │
         ▼
    Hypervisor in scheduler pool.
    Forge manages VMs + network + storage.
    HypervisorReport published via gossip.
```

### 21. Open questions and future work

**Decided — not in scope for v1, documented for future ADRs:**

- **Live migration**: currently stop → move volume (S3-backed, no data copy) → start on new hypervisor. Cloud Hypervisor supports live migration — a future ADR will define the protocol, pre-copy/post-copy strategy, and convergence criteria.
- **Overcommit policies per org**: some orgs want dedicated resources (overcommit 1.0), others accept sharing (overcommit 2.0+). This requires per-org overcommit configuration propagated to hypervisor admission control.
- **Topology-aware network cost**: intra-zone traffic is cheaper than cross-region. The scheduler should factor network topology into placement decisions for latency-sensitive workloads.
- **Hypervisor groups / pools**: grouping hypervisors beyond region/zone (e.g., "GPU pool", "high-memory pool") for administrative convenience. Currently achievable via labels, but a formal group concept may be warranted.
- **Automatic rebalancing**: when a new hypervisor joins an under-provisioned zone, automatically migrate VMs to balance load. Requires live migration and a rebalancing policy.

## Rejected alternatives

### 1. Keep using "node" everywhere

Rejected. The ambiguity between "fabric node" and "compute host" causes real confusion in API design, scheduler logic, and documentation. When `VmPlacement.hosting_node` references a fabric node ID, it conflates two abstractions. The hypervisor model makes the boundary explicit and allows each layer to use the appropriate concept.

### 2. Make every node a hypervisor

Rejected. Not every machine in the mesh should host VMs. Dedicated routers, monitoring collectors, control-plane-only nodes, and bastion hosts are legitimate mesh participants that should never appear in the scheduler's placement pool. Forcing every node into the hypervisor model would require artificial workarounds (e.g., a "no-compute" taint on every non-compute node).

### 3. Model hypervisors as labels on nodes (no first-class resource)

Rejected. Labels are unstructured metadata. A hypervisor needs structured fields: hardware specs, capacity tracking, lifecycle state, taints. Modeling this as labels would mean parsing strings for CPU cores, memory, and GPU specs — fragile, untyped, and impossible to validate at the schema level.

### 4. Separate hypervisor ID from fabric node ID

Considered but rejected for v1. In theory, the hypervisor could have a completely independent identity. In practice, every hypervisor IS a fabric node, and the 1:1 relationship is simpler to reason about. The `fabric_node_id` field on the hypervisor record is the explicit link. If a future scenario requires N hypervisors per physical node (nested virtualization), this can be revisited.

### 5. No overcommit — always 1:1 CPU allocation

Rejected. CPU overcommit is standard practice in cloud computing. Most workloads are bursty — a 2-vCPU VM rarely uses both cores simultaneously. A 2x overcommit ratio doubles the effective compute density with minimal performance impact for typical web/API workloads. The operator can set overcommit to 1.0 for performance-sensitive or latency-critical deployments.

### 6. Memory overcommit by default

Rejected. Unlike CPU (where the kernel scheduler transparently time-slices), memory overcommit triggers the OOM killer when physical memory is exhausted. In a multi-tenant platform, OOM-killing a tenant's VM because a neighbor overallocated is a severe isolation violation. Default memory overcommit is 1.0 (no overcommit). The operator can explicitly increase it for environments where the risk is understood (e.g., dev/test with bursty workloads).

## References

- `handbook/ARCHITECTURE.md` — global architecture, stack diagram, design principles
- `handbook/zones-and-regions.md` — region and zone model
- `handbook/adr-001-networking-roadmap.md` — VmPlacement, networking primitives
- `handbook/adr-002-security-groups-route-tables.md` — NIC model, ResourceState
- `handbook/adr-003-forge.md` — Forge per-node orchestrator, API, reconciliation, capacity
- `layers/fabric/README.md` — fabric node concept, mesh topology, zone/region metadata
- `handbook/state-and-reconciliation.md` — reconciliation philosophy
