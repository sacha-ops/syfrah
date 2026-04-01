# ADR-005: Control Plane — Distributed Consensus, Gossip, and Scheduling

**Status**: Proposed
**Date**: 2026-04-01
**Decided by**: Sacha + team
**Depends on**: ADR-001 (networking), ADR-002 (security groups + route tables), ADR-003 (Forge), ADR-004 (hypervisor model)

## 1. Context and motivation

The data plane is designed. Fabric provides encrypted node-to-node connectivity (implemented). Forge is the per-node resource orchestrator that reconciles local reality against desired state (ADR-003). The hypervisor model formalizes the compute host resource and its placement in the Region → Zone → Hypervisor → VM hierarchy (ADR-004). The overlay delivers VPCs with VXLAN isolation, security groups, route tables, and NAT gateways (ADR-001, ADR-002).

What is missing is the **distributed brain** — the component that turns a collection of independent nodes into a single coherent platform. Today, each node operates in isolation:

- **No shared IPAM.** Two nodes can allocate the same IP address to different VMs. There is no cluster-wide uniqueness guarantee.
- **No FDB distribution.** A node has no knowledge of VMs on other nodes. VXLAN forwarding between nodes is impossible without manually populated FDB entries.
- **No cross-node scheduling.** There is no scheduler to decide which hypervisor should host a given VM. The operator must manually target a node.
- **No single API.** The operator must know which node to talk to for which resource. There is no unified entry point.
- **No coordinated security groups.** A security group change must be manually applied to every node hosting VMs with that SG.
- **No automated failure recovery.** If a node dies, its VMs are lost. There is no mechanism to detect the failure and reschedule workloads.

Without the control plane, Syfrah is a local VM manager that happens to have encrypted tunnels between nodes. With it, Syfrah becomes a distributed cloud platform where the operator manages resources at the cluster level and the platform handles placement, networking, and resilience.

This ADR defines the control plane completely: Raft consensus via openraft, SWIM gossip via foca, the distributed scheduler, the API gateway with transparent leader forwarding, distributed IPAM, FDB derivation from Raft state, cluster health monitoring, failure modes, migration strategy, and implementation phases.

### Why now

Forge (ADR-003) is designed to consume a local materialized view of desired state from redb — the output of the Raft state machine. The overlay (ADR-001, ADR-002) defines FDB entries, IPAM bitmaps, and security group rules that need cluster-wide consistency. The hypervisor model (ADR-004) defines the schedulable compute host that the scheduler places VMs onto. All of these depend on the control plane's state distribution and coordination. Defining the control plane now closes the loop: the full path from operator request to running VM across multiple nodes is specified.

### Relationship to existing decisions

- **ARCHITECTURE.md** — The control plane sits between Forge and the tenant API in the stack diagram. This ADR specifies its internals.
- **ADR-001** — IPAM, FDB, VPC/subnet definitions become Raft state. FDB entries are derived from Raft placements, not gossip announcements.
- **ADR-002** — Security groups, route tables, NAT gateways, and NICs are Raft state. Forge derives nftables rules from the Raft materialized view.
- **ADR-003 (Forge)** — Forge consumes the local redb materialized view produced by the Raft state machine. Forge never reads the Raft log directly. The "desired state projection" defined in ADR-003 is produced by the state machine defined here.
- **ADR-004 (Hypervisor)** — Hypervisor records are Raft state. The scheduler places VMs onto hypervisors. Gossip carries capacity telemetry for scheduling hints.
- **state-and-reconciliation.md** — The reconciliation philosophy (Raft = desired, gossip = observed, Forge reconciles) is the operating model. This ADR defines the Raft and gossip layers that produce the inputs to that model.

## 2. Architecture overview

```
Operator / Terraform / API client
         │
         ▼
   Any node (Raft follower or leader)
         │
         ▼ forward to leader (if write)
   Raft Leader (one node, auto-elected)
         │
         ├── Validate command
         ├── Append to Raft log
         ├── Replicate to majority
         ├── Commit on quorum acknowledgment
         │
         ▼
   State Machine (apply committed entries)
         │
         ▼
   Local redb on EACH node (materialized view)
         │
         ▼
   Forge on each node reads local redb → reconciles local reality
```

The control plane is embedded in every node. There is no dedicated controller. One node is elected Raft leader — this is automatic and invisible to the operator. The operator talks to any node; the platform routes the request correctly.

Two complementary protocols divide the work:

| Protocol | Crate | What it handles | Consistency | Transport |
|----------|-------|-----------------|-------------|-----------|
| **Raft** | `openraft` | IP allocation, VM scheduling, VPC config, SGs, routes, NAT GWs, org/project/env, hypervisor records | Strong (linearizable writes, sequentially consistent reads) | HTTP/JSON over syfrah0 (WireGuard) |
| **Gossip** | `foca` | Node liveness, capacity telemetry, hypervisor status hints, drain status | Eventual (~1-5 seconds) | UDP over syfrah0 (WireGuard) |

Raft state is **prescriptive** — what should exist. Gossip state is **descriptive** — what is actually happening. Forge reconciles the two.

## 3. openraft integration

### The crate

[openraft](https://github.com/databendlabs/openraft) is a Rust Raft implementation built on Tokio. It is async, embeddable, supports single-node through multi-node operation, and is used in production by Databend. It provides the consensus algorithm; we provide three trait implementations that connect it to our storage and network layers.

### Three traits to implement

**`RaftLogStorage`** — The append-only Raft log.

Stores uncommitted and committed log entries. Entries are appended during replication and truncated during compaction. Backed by a dedicated redb database file (`~/.syfrah/raft-log.redb`) separate from the state machine database. This separation ensures that log compaction and state machine operations do not contend on the same file.

Key operations:
- `append` — write entries to the log (called during replication)
- `truncate` — remove entries after a given index (called during conflict resolution)
- `get_log_state` — return the last log index and term
- `read_log_entry` — read a single entry by index
- `purge` — remove entries up to a given index (called after snapshot)

**`RaftStateMachine`** — The state machine that applies committed entries.

When Raft commits an entry (quorum acknowledged), the state machine applies it to the local redb database (`~/.syfrah/state.redb`). This is the materialized view that Forge reads. Every node applies the same entries in the same order, producing identical state on all nodes.

Key operations:
- `apply` — apply a batch of committed entries to redb (single transaction per batch)
- `snapshot` — produce a snapshot of the current state for new node catch-up
- `install_snapshot` — install a snapshot received from the leader (full state transfer)
- `get_snapshot_builder` — return a builder that can produce a snapshot asynchronously

The apply function is the heart of the system. It receives a `RaftCommand` (defined in section 5), validates it against the current state, and mutates redb tables within a single ACID transaction. If validation fails (e.g., duplicate IP allocation), the command is applied as a no-op with an error result stored in the response.

**`RaftNetwork`** — Network transport between Raft nodes.

Implements point-to-point communication between Raft members. Uses HTTP/JSON over the WireGuard fabric interface (syfrah0). Each node exposes Raft RPC endpoints on a dedicated port (default 7200, configurable).

Endpoints:
- `POST /raft/vote` — RequestVote RPC
- `POST /raft/append` — AppendEntries RPC
- `POST /raft/snapshot` — InstallSnapshot RPC

All traffic is encrypted by WireGuard. No additional TLS layer is needed.

### Raft group membership

Every node that joins the fabric becomes a Raft member. The membership model:

- **Voter nodes**: participate in leader election and quorum. Default for the first 5 nodes.
- **Non-voter (learner) nodes**: replicate the full Raft log and maintain an identical redb, but do not vote. Used for clusters larger than 5-7 nodes to keep quorum fast while still distributing state everywhere.

The Raft group is the full set of nodes. Every hypervisor and every non-hypervisor node participates. Non-hypervisor nodes (routers, monitoring boxes) still need the Raft state for FDB population, SG enforcement, and API serving.

### Leader election

openraft handles leader election automatically. Configuration:

- **Heartbeat interval**: 500ms
- **Election timeout**: 1000-2000ms (randomized to prevent split votes)
- **Minimum election timeout**: 1000ms

When the leader stops sending heartbeats (crash, network issue), followers wait for the election timeout to expire, then start an election. A new leader is typically elected within 1-2 seconds.

### Log replication

The leader appends entries to its local log, then replicates to all followers in parallel. Once a majority of voters acknowledge, the entry is committed. The leader then responds to the client.

Replication is pipelined — the leader does not wait for one batch to be acknowledged before sending the next. This keeps throughput high even with cross-datacenter latency.

### Snapshot and compaction

Over time, the Raft log grows unboundedly. Snapshots solve this:

1. Periodically (every 10,000 entries or 1 hour, whichever comes first), the leader creates a snapshot of the current redb state.
2. The snapshot is a compressed copy of the redb database file.
3. Log entries up to the snapshot's last applied index are purged.
4. When a new node joins (or a node falls far behind), the leader sends the snapshot instead of replaying the entire log.

Snapshot transfer uses the same HTTP transport, streamed in chunks to avoid memory pressure.

### Transport: HTTP over WireGuard fabric

All Raft traffic travels over the WireGuard mesh interface (syfrah0). This provides:

- **Encryption**: ChaCha20-Poly1305 (WireGuard)
- **Authentication**: only mesh members can participate
- **WAN tolerance**: WireGuard handles NAT traversal, keepalives, and roaming
- **IPv6 addressing**: each node is reachable at its deterministic fabric IPv6 address

The Raft network implementation maps each Raft node ID to its fabric IPv6 address. Address resolution uses the fabric peer list, which is already maintained by the fabric layer.

## 4. State machine — what goes into Raft

The state machine owns **everything that must be consistent across the cluster**. The guiding rule: if losing or duplicating a piece of state would violate a user-facing invariant (double IP, orphan VM, unauthorized access, inconsistent security policy), it goes into Raft.

### Authoritative state (stored in Raft, replicated to all nodes via redb)

| Category | Resources | Why Raft |
|----------|-----------|----------|
| **Organization** | Orgs, Projects, Environments (name, TTL, deletion protection, labels) | Hierarchy must be consistent for validation and cost tracking |
| **VPC** | VPC definitions (name, CIDR, VNI, owner, shared flag) | VNI uniqueness must be guaranteed cluster-wide |
| **Subnet** | Subnet definitions (name, VPC, env, CIDR, gateway) | CIDRs must not overlap within a VPC |
| **IPAM** | IP allocations (subnet → bitmap), allocation records (IP → VM, MAC, state) | **Double allocation is catastrophic.** Single-threaded log application guarantees uniqueness. |
| **Security Groups** | SG definitions, rules (direction, protocol, port range, source/dest), SG-to-NIC attachments | Must be consistent — a stale SG rule is a security hole |
| **Route Tables** | Route table definitions, routes (destination CIDR, target type + ID), subnet associations | Routing inconsistency causes packet loss or misrouting |
| **NAT Gateways** | NAT GW definitions (subnet, public IP, state) | Must be consistent for internet egress to work |
| **Hypervisor records** | Registration, hardware spec, state, labels, taints | Scheduler needs consistent view of available hosts |
| **VM placement** | PlaceVm decisions (vm_id, hypervisor_id, subnet_id, IP, MAC) | The authoritative answer to "where does this VM run?" |
| **Instance definitions** | VM spec (vCPU, memory, image, volumes, network, placement constraints) | The authoritative desired state for each VM |
| **NIC records** | Network interface definitions (VM, subnet, VPC, IP, MAC, SG attachments) | SG enforcement requires knowing which SGs apply to which NIC |
| **VPC Peering** | Peering records (vpc_a, vpc_b, status) | Cross-VPC routing must be consistent |
| **Volume definitions** | Volume spec (size, env), attachment (volume → VM) | Attachment must be exclusive (one VM at a time) |

### Derived state (NOT stored in Raft — computed by each Forge from Raft state)

| Derived state | Computed from | Where it lives |
|---------------|---------------|----------------|
| FDB entries (MAC → VTEP) | VM placements in Raft | `bridge fdb` on each node's kernel |
| ARP proxy entries | IP allocations in Raft | `ip neigh` on each node |
| nftables rules | SG rules + NIC attachments in Raft | nftables ruleset on each node |
| Linux bridges | VPC definitions + VM placements in Raft | kernel interfaces on each node |
| VXLAN interfaces | VPC VNI + VM placements in Raft | kernel interfaces on each node |
| TAP/veth devices | Instance definitions + placements in Raft | kernel interfaces on each node |
| NAT/masquerade rules | NAT GW + route table associations in Raft | nftables on each node |
| DNS zone files | Instance names + IP allocations in Raft | CoreDNS config on each node |
| MAC addresses | Deterministic from IP (`02:00:{ip_hex}`) | Computed, not stored |

### Observed state (NOT stored in Raft — reported via gossip)

| Observed state | Source | Used by |
|----------------|--------|---------|
| VM runtime status (running, stopped, error) | Forge on the hosting hypervisor | Dashboards, health alerts |
| Node capacity (free vCPU, memory, disk) | Forge capacity tracker | Scheduler (as hints) |
| Node liveness (up, suspect, down) | SWIM protocol | Scheduler (avoid down nodes) |
| Hypervisor status (reachable, forge version, uptime) | Forge health check | Operational awareness |
| Drain status | Forge (operator-initiated) | Scheduler (stop placing new VMs) |

**The rule**: Raft is truth. Derived state is recomputed from Raft. Observed state is reported upward. If derived state and observed state disagree, Forge reconciles.

## 5. State machine operations

Every mutation to the cluster's desired state is encoded as a `RaftCommand` and appended to the Raft log. When committed, the state machine applies it to redb within a single ACID transaction. All nodes apply the same commands in the same order, producing identical state.

```rust
/// Every command that mutates cluster state.
/// Serialized with serde, appended to the Raft log.
enum RaftCommand {
    // ── Organization ────────────────────────────────────────
    CreateOrg { name: String },
    DeleteOrg { org_id: OrgId },

    // ── Project ─────────────────────────────────────────────
    CreateProject { name: String, org_id: OrgId },
    DeleteProject { project_id: ProjectId },

    // ── Environment ─────────────────────────────────────────
    CreateEnv {
        name: String,
        project_id: ProjectId,
        ttl: Option<Duration>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    },
    DeleteEnv { env_id: EnvironmentId },

    // ── VPC ─────────────────────────────────────────────────
    CreateVpc {
        name: String,
        cidr: Ipv4Net,
        owner: VpcOwner,         // Project(id) | Org(id)
        shared: bool,
    },
    DeleteVpc { vpc_id: VpcId },
    AttachVpc { vpc_id: VpcId, project_id: ProjectId },
    DetachVpc { vpc_id: VpcId, project_id: ProjectId },

    // ── Subnet ──────────────────────────────────────────────
    CreateSubnet {
        name: String,
        vpc_id: VpcId,
        env_id: EnvironmentId,
        cidr: Ipv4Net,
    },
    DeleteSubnet { subnet_id: SubnetId },

    // ── IPAM ────────────────────────────────────────────────
    AllocateIp {
        subnet_id: SubnetId,
        vm_id: VmId,
    },
    ReleaseIp {
        subnet_id: SubnetId,
        ip: Ipv4Addr,
    },

    // ── Network Interface ───────────────────────────────────
    CreateNic {
        vm_id: VmId,
        subnet_id: SubnetId,
        vpc_id: VpcId,
        ip: Ipv4Addr,
        mac: MacAddr,
    },
    DeleteNic { nic_id: NicId },

    // ── Security Group ──────────────────────────────────────
    CreateSg {
        name: String,
        vpc_id: VpcId,
        description: String,
    },
    DeleteSg { sg_id: SecurityGroupId },
    AddSgRule {
        sg_id: SecurityGroupId,
        direction: Direction,     // Ingress | Egress
        protocol: Protocol,       // Tcp | Udp | Icmp | All
        port_range: Option<PortRange>,
        source: RuleTarget,       // Cidr(Ipv4Net) | Sg(SecurityGroupId) | Any
    },
    RemoveSgRule { sg_id: SecurityGroupId, rule_id: RuleId },
    AttachSg { sg_id: SecurityGroupId, nic_id: NicId },
    DetachSg { sg_id: SecurityGroupId, nic_id: NicId },

    // ── Route Table ─────────────────────────────────────────
    CreateRouteTable { name: String, vpc_id: VpcId },
    DeleteRouteTable { route_table_id: RouteTableId },
    AddRoute {
        route_table_id: RouteTableId,
        destination: Ipv4Net,
        target: RouteTarget,      // NatGateway(id) | VpcPeering(id) | Local | Blackhole
    },
    RemoveRoute { route_table_id: RouteTableId, route_id: RouteId },
    AssociateSubnet { route_table_id: RouteTableId, subnet_id: SubnetId },
    DisassociateSubnet { route_table_id: RouteTableId, subnet_id: SubnetId },

    // ── NAT Gateway ─────────────────────────────────────────
    CreateNatGw {
        name: String,
        subnet_id: SubnetId,
        public_ip: Ipv4Addr,
    },
    DeleteNatGw { nat_gw_id: NatGatewayId },

    // ── VPC Peering ─────────────────────────────────────────
    CreatePeering { vpc_a: VpcId, vpc_b: VpcId },
    DeletePeering { peering_id: PeeringId },

    // ── Hypervisor ──────────────────────────────────────────
    RegisterHypervisor {
        name: String,
        fabric_node_id: NodeId,
        region: String,
        zone: String,
        public_ip: String,
        fabric_ipv6: String,
        hardware: HardwareSpec,
    },
    DeregisterHypervisor { hypervisor_id: HypervisorId },
    UpdateHypervisorState {
        hypervisor_id: HypervisorId,
        state: HypervisorState,
    },
    UpdateHypervisorLabels {
        hypervisor_id: HypervisorId,
        labels: HashMap<String, String>,
    },
    UpdateHypervisorTaints {
        hypervisor_id: HypervisorId,
        taints: Vec<Taint>,
    },

    // ── VM Placement ────────────────────────────────────────
    PlaceVm {
        vm_id: VmId,
        hypervisor_id: HypervisorId,
        subnet_id: SubnetId,
        ip: Ipv4Addr,
        mac: MacAddr,
    },
    RemovePlacement { vm_id: VmId },

    // ── Instance Lifecycle ──────────────────────────────────
    CreateInstance {
        spec: InstanceSpec,       // vCPU, memory, image, volumes, network, constraints
    },
    DeleteInstance { vm_id: VmId },
    StartInstance { vm_id: VmId },
    StopInstance { vm_id: VmId },

    // ── Volume ──────────────────────────────────────────────
    CreateVolume {
        name: String,
        size_gb: u32,
        env_id: EnvironmentId,
    },
    DeleteVolume { volume_id: VolumeId },
    AttachVolume { volume_id: VolumeId, vm_id: VmId },
    DetachVolume { volume_id: VolumeId },

    // ── Cluster Membership ──────────────────────────────────
    AddRaftMember { node_id: NodeId, fabric_ipv6: String, voter: bool },
    RemoveRaftMember { node_id: NodeId },
    PromoteToVoter { node_id: NodeId },
    DemoteToLearner { node_id: NodeId },
}
```

### Command processing pipeline

1. **Receive**: API handler on any node receives the command.
2. **Forward**: if this node is not the leader, forward to the leader via HTTP.
3. **Validate**: the leader validates the command against current state (e.g., check that the subnet has free IPs, the hypervisor exists, the VPC is not being deleted).
4. **Propose**: the leader proposes the command to openraft.
5. **Replicate**: openraft replicates the entry to followers.
6. **Commit**: once a majority acknowledges, the entry is committed.
7. **Apply**: the state machine on EVERY node applies the committed entry to its local redb within a single transaction.
8. **Respond**: the leader returns the result to the client.

Validation happens twice: once on the leader before proposal (fast rejection of invalid commands) and once during apply (definitive check, because state may have changed between proposal and commit). The apply-time validation is authoritative.

### Idempotency

Commands include a client-supplied idempotency key. The state machine maintains a deduplication journal mapping recent keys to their results. If the same key is applied twice (e.g., client retry after timeout), the state machine returns the cached result without re-executing. See section 19 (Request Idempotency) for the full mechanism, TTL, and CLI behavior.

## 6. Materialized views per hypervisor

Every committed Raft entry is applied to the local redb on every node. This means every node has the **complete cluster state**. Forge on each node reads the full state but only reconciles resources relevant to its node.

### What each Forge needs

| Data | Why |
|------|-----|
| Resources where `hypervisor_id == this_node` | Forge manages local VMs, NICs, TAPs, bridges |
| VPC/subnet/SG/route definitions for VMs on this node | Forge derives nftables rules, bridge configs, gateway IPs |
| Remote VM placements for VPCs present locally | Forge populates FDB entries for cross-node VXLAN forwarding |
| Remote VTEP addresses (fabric IPv6 of nodes hosting VMs in local VPCs) | Forge configures VXLAN remote endpoints |
| Global org/project/env hierarchy | Forge validates requests and displays context |

### Index structure

The state machine maintains secondary indexes in redb for efficient Forge queries:

```
hypervisor_placements    hypervisor_id → [vm_id]
vpc_placements           vpc_id → [(vm_id, hypervisor_id, ip, mac)]
subnet_nics              subnet_id → [nic_id]
sg_nics                  sg_id → [nic_id]
vpc_hypervisors          vpc_id → [hypervisor_id]  (which nodes have VMs in this VPC)
```

When Forge runs its reconciliation loop, it:

1. Reads `hypervisor_placements[this_node]` to get local VM IDs.
2. For each local VM, reads the full instance spec, NIC, SG rules, subnet, VPC.
3. Reads `vpc_placements[vpc_id]` for each local VPC to get remote VM placements (for FDB).
4. Reads `vpc_hypervisors[vpc_id]` for each local VPC to get remote VTEP addresses (for VXLAN).
5. Reconciles: creates missing resources, removes stale ones, updates drifted ones.

### Why full state on every node

An alternative design would replicate only relevant subsets to each node. We rejected this because:

- openraft replicates the full log to all members — there is no per-node filtering at the Raft level.
- Full state allows any node to serve read requests for any resource (API gateway).
- Full state makes leader transfer seamless — the new leader already has everything.
- The total state size for a 10,000-VM cluster is estimated at ~50-100 MB in redb. This is negligible for dedicated servers with 32-256 GB RAM.

Forge reads selectively (via indexes), but every node stores everything.

## 7. SWIM gossip protocol

### Purpose

Gossip is the fast, decentralized health and telemetry layer. It runs alongside Raft but serves a fundamentally different purpose: Raft provides consistency, gossip provides speed and failure detection.

### What gossip carries

| Data | Update frequency | Consumer |
|------|-----------------|----------|
| Node liveness (Alive, Suspect, Down) | Continuous (SWIM probing) | Scheduler, Raft leader (for node failure handling) |
| CPU/memory/disk capacity hints | Every 10 seconds | Scheduler (for placement decisions) |
| Hypervisor state hints (draining, maintenance) | On change | Scheduler (to avoid placing new VMs) |
| VM count per hypervisor | Every 10 seconds | Scheduler (spread scoring) |

### What gossip does NOT carry

- **VM placements** — authoritative in Raft, not gossip
- **FDB entries** — derived from Raft state, not gossip
- **Security group rules** — Raft state
- **IP allocations** — Raft state
- **Any correctness-critical data** — gossip is a hint, never a contract

### Implementation

The gossip layer uses the `foca` crate, a transport-agnostic SWIM (Scalable Weakly-consistent Infection-style Membership) implementation in Rust.

**Transport**: UDP datagrams over the WireGuard fabric (syfrah0 IPv6 addresses). Port 7300 (configurable). Encrypted by WireGuard — no additional encryption needed.

**Protocol mechanics**:

1. **Ping**: each node probes a random subset of known members every 1 second.
2. **Ping-req** (indirect probe): if a direct ping fails, the node asks K other members to probe the suspect on its behalf. K = 3 by default.
3. **Suspect**: if both direct and indirect probes fail, the member is marked suspect. Suspicion is disseminated via piggybacked protocol messages.
4. **Confirm down**: if a suspect member does not refute the suspicion within the suspicion timeout (default 5 seconds), it is declared Down.
5. **Refute**: a suspected node that is actually alive broadcasts a refutation (protocol message with higher incarnation number).

State names align with the foca SWIM implementation: Alive, Suspect, Down.

**Failure detection timing**:

| Event | Timing |
|-------|--------|
| Direct ping | Every 1 second |
| Suspect declared | After 1 failed direct + 3 failed indirect probes (~2-3 seconds) |
| Down declared | Suspect timeout (5 seconds) after suspicion |
| Total detection time | ~5-10 seconds (best case) to ~15 seconds (worst case) |

**Custom metadata**: each node piggybacks a `NodeReport` on its protocol messages:

```rust
struct NodeReport {
    hypervisor_id: Option<HypervisorId>,   // None if not a hypervisor
    available_vcpus: u32,
    available_memory_gb: u32,
    available_disk_gb: u32,
    vm_count: u32,
    state_hint: NodeStateHint,             // Available | Draining | Maintenance
    forge_version: String,
}
```

This metadata is updated locally every 10 seconds and disseminated passively via SWIM protocol messages. No dedicated telemetry broadcast — the data travels on the same protocol messages used for failure detection.

**Membership list**: foca maintains the membership list automatically. When a new node joins the Raft cluster, the gossip layer is informed and adds it to the membership list. When a node is removed from Raft, it is removed from gossip.

## 8. Scheduler

The scheduler runs on the Raft leader only. It is invoked when a `CreateInstance` command arrives and a placement decision is needed.

### Scheduler guarantees and limitations

**What the scheduler guarantees:**
- Global uniqueness of IP addresses (via Raft IPAM)
- Placement decision recorded in Raft (authoritative)
- Forge admission recheck prevents overcommit

**What the scheduler does NOT guarantee (Phase 1-3):**
- Optimal placement under high contention (no reservations — multiple schedulers may pick the same hypervisor)
- Bounded placement latency (retries under contention)
- Fair distribution under burst (hotspot possible until gossip converges)

Without reservations, placement is optimistic. Strong uniqueness is guaranteed for identifiers and allocations (IPs, VNIs, NICs), but compute admission is only guaranteed at Forge execution time. Under concurrent burst creates, retry storms are possible.

Future (Phase 6+): in-flight reservations at the scheduler level reduce contention from O(N²) retries to O(1) placement.

### Scheduling pipeline

```
CreateInstance arrives at the leader
         │
         ▼
1. Parse placement constraints from the instance spec
   - region/zone affinity (--region eu-west, --zone eu-west-1)
   - node selector labels (--node-selector gpu=a100)
   - anti-affinity rules (--anti-affinity app=web spread across zones)
   - toleration for taints
         │
         ▼
2. Query gossip for hypervisor capacity hints
   - available vCPU, memory, disk per hypervisor
   - node liveness (alive only — exclude suspect and down)
         │
         ▼
3. Filter: build candidate list
   a. State filter: hypervisor state == Available (from Raft)
   b. Liveness filter: gossip reports node as alive
   c. Region/zone filter: match requested region/zone
   d. Label filter: node labels satisfy node-selector
   e. Taint filter: node taints must be tolerated by VM spec
   f. Capacity filter: gossip-reported capacity >= requested resources
         │
         ▼
4. Score: rank candidates
   a. Available capacity score (higher = more room = better spread)
   b. Zone spread score (prefer zones with fewer VMs of this type)
   c. Anti-affinity score (penalize colocation with conflicting VMs)
   d. Data locality score (prefer nodes that already have the VM image cached)
         │
         ▼
   **Gossip staleness:** The scheduler uses gossip capacity reports that may be up to 30 seconds stale. Under burst conditions, the reported available capacity may not reflect in-flight reservations or recent placements not yet propagated. The Forge admission recheck is the true capacity gate — gossip is scoring input, not a guarantee.
         │
         ▼
5. Select: pick the highest-scoring hypervisor
         │
         ▼
6. Commit to Raft:
   - AllocateIp { subnet_id, vm_id }
   - CreateNic { vm_id, subnet_id, vpc_id, ip, mac }
   - PlaceVm { vm_id, hypervisor_id, subnet_id, ip, mac }
   All three are committed atomically in a single Raft log batch.
         │
         ▼
7. Forge on the selected hypervisor reads the placement from redb
   → creates bridge, VXLAN, TAP, nftables rules, boots VM

8. Forge on EVERY node with VMs in the same VPC reads the new placement
   → adds FDB entry pointing the new VM's MAC to the selected hypervisor's VTEP
```

### Admission control

After the scheduler selects a hypervisor, Forge on that node performs admission control before actually provisioning the VM. This is the definitive capacity check — gossip hints may be stale. If the hypervisor no longer has capacity (another VM was placed between scheduling and admission), Forge rejects the placement and the scheduler retries with a different hypervisor.

This two-phase approach (gossip hints for fast filtering, Forge admission for correctness) avoids both false rejections and overbooking.

### Scheduler policy

The default scheduler is **spread-first with bin-packing tiebreak**:

1. Prefer spreading VMs across zones (resilience).
2. Within a zone, prefer the hypervisor with the most available resources (room for growth).
3. If all else is equal, prefer the hypervisor with fewer total VMs (bin-packing balance).

The scheduler is implemented as a trait, allowing future custom policies:

```rust
trait Scheduler: Send + Sync {
    fn select_hypervisor(
        &self,
        spec: &InstanceSpec,
        candidates: &[HypervisorCandidate],
    ) -> Result<HypervisorId>;
}
```

### Rescheduling

The scheduler is also invoked when:

- **A hypervisor is declared down** (gossip down for >60 seconds, Raft leader commits hypervisor state change). VMs on the down hypervisor with `restart_on_failure: true` are rescheduled to healthy hypervisors.
- **A hypervisor enters draining state** (operator-initiated). The scheduler migrates VMs to other hypervisors one at a time (stop, reschedule, start).
- **A placement is rejected by admission control**. The scheduler retries with the next best candidate.

## 9. Placement Fencing

Every VM placement carries a monotonically increasing `placement_generation` (u64), assigned by the Raft state machine at commit time.

### The problem: stale placements after reschedule

When a hypervisor becomes unreachable and a VM is rescheduled:
1. Leader commits `RescheduleVm { vm_id, from: hv-002, to: hv-005, generation: 42 }`
2. hv-005 Forge starts the VM with generation 42
3. hv-002 comes back online with the OLD VM still running (generation 41)

Without fencing, hv-002's Forge would continue reconciling the stale VM — resulting in a double-run.

### The invariant

**A Forge must refuse to reconcile any VM whose local placement generation is older than the Raft placement record.**

On every reconciliation cycle, Forge compares:
- `local_generation` (from its last known placement)
- `raft_generation` (from the materialized desired state)

If `local_generation < raft_generation`: the VM has been rescheduled elsewhere. Forge MUST:
1. Stop the local VM process immediately
2. Clean up all local resources (TAP, nftables, FDB)
3. Release the local IPAM reservation (if any)
4. Log: "fenced stale VM {id} (local gen {N} < raft gen {M})"

If `local_generation == raft_generation` AND `hypervisor_id != this_node`: same — the VM doesn't belong here.

### Double-run protection

This is the primary defense against double-run. Without it:
- Stateless VMs: two copies running = split brain for clients
- Stateful VMs: two copies writing to the same volume = data corruption
- Network: two VMs with the same IP = ARP conflicts, FDB confusion

Fencing is not optional. It is a correctness requirement.

## 10. API gateway — single entry point

Every node runs an axum HTTP server on its fabric IPv6 address, port 7100 (the Forge API port, as defined in ADR-003). The operator, Terraform provider, or any API client talks to ANY node.

### Request routing

```
Operator → POST /v1/instances to Node C (a follower)
         │
         ▼
   Node C checks: am I the Raft leader?
   ├── No → forward the request to the leader (Node A)
   │        Node C acts as a reverse proxy:
   │        - Copies all headers and body
   │        - Sends to http://[Node-A-fabric-ipv6]:7100/v1/instances
   │        - Streams the response back to the operator
   │        - The forwarding is transparent to the operator
   │
   └── Yes → process the request locally
              (validate, propose to Raft, wait for commit, respond)
```

### Read vs. write routing

| Request type | Routing | Consistency |
|-------------|---------|-------------|
| **Write** (POST, PUT, DELETE) | Always forwarded to Raft leader | Strongly consistent (committed to quorum) |
| **Read** (GET) — default | Served locally from redb | Eventually consistent (bounded by replication lag, typically <100ms) |
| **Read** (GET) with `?consistent=true` | Forwarded to leader, leader confirms it is still leader before responding | Strongly consistent (linearizable) |

Most reads are served locally. This scales horizontally — adding nodes adds read capacity. Writes always go through the leader, which is the bottleneck, but write throughput is sufficient for control plane operations (not data plane).

### Leader discovery

Each node tracks the current Raft leader via openraft's notifications. When a follower needs to forward a request:

1. Check the locally known leader ID (from the last Raft message).
2. If the leader is known, forward immediately.
3. If the leader is unknown (e.g., election in progress), return `503 Service Unavailable` with a `Retry-After: 1` header.
4. If the forward fails (leader crashed), return `502 Bad Gateway` — the client retries to any node.

### External API (future)

The fabric-internal API (HTTP on syfrah0) is the foundation. A future external API (gRPC or HTTPS on a public interface) will proxy to the internal API. This ADR does not define the external API — it is a separate concern.

## 11. IPAM becomes distributed

### The problem today

Each node maintains its own IPAM bitmap in its local redb. Two nodes can independently allocate the same IP address to different VMs in the same subnet. There is no coordination.

### The solution

IPAM is a Raft command. The full flow:

```
1. CreateInstance arrives at the leader
2. Scheduler selects a hypervisor
3. Leader proposes AllocateIp { subnet_id, vm_id }
4. State machine checks the IPAM bitmap for the subnet:
   a. Find the next free IP (scan bitmap for first zero bit)
   b. Set the bit
   c. Create an IpAllocation record { ip, subnet_id, vm_id, mac, state: Reserved }
   d. Return the allocated IP
5. The AllocateIp entry is committed to all nodes
6. ALL nodes' state machines update their local bitmap and allocation table
7. No other node can allocate the same IP — the bitmap on every node is identical
```

The IPAM bitmap is part of the Raft state machine. Only the leader modifies it (via committed log entries). All nodes see the same bitmap. Double allocation is structurally impossible.

### Allocation lifecycle

```
Reserved → Assigned → Released
    │                    │
    └── Orphaned ────────┘
         (reclaimed)
```

- **Reserved**: IP allocated by `AllocateIp`, VM not yet running.
- **Assigned**: VM is running, NIC is active.
- **Orphaned**: IP was reserved but VM creation failed. Detected by the reconciliation loop (reservation older than 5 minutes with no corresponding running VM). Reclaimed via `ReleaseIp` command.
- **Released**: IP returned to the pool via `ReleaseIp`.

### MAC derivation

MAC addresses are deterministically derived from IPs: `02:00:{IP octets in hex}`. For example, `10.1.0.5` produces `02:00:0a:01:00:05`. This eliminates the need for a MAC allocation service. The MAC is computed, never stored as authoritative state (though it appears in placement records for convenience).

## 12. FDB becomes derived from Raft state

### The problem today

FDB entries are not distributed. A node has no knowledge of VMs on other nodes. VXLAN forwarding between nodes is impossible.

### The solution

FDB entries are **derived from VM placement records in Raft**. Every `PlaceVm` entry contains the VM's MAC, IP, and hosting hypervisor. Each Forge reads the global placement state and populates its local FDB table.

### Derivation strategy

**Cold rebuild (on Forge start / restart):**
Full scan of all VM placements in the local VPCs. O(total VMs in VPC). Acceptable at startup — happens once.

**Incremental reconciliation (normal operation):**
When a new Raft entry is committed (PlaceVm, RemoveVm), Forge applies only the delta:
- PlaceVm on remote hypervisor: add one FDB entry + one ARP proxy
- RemoveVm on remote hypervisor: remove one FDB entry + one ARP proxy
- PlaceVm on this hypervisor: no FDB needed (local bridge forwarding)

The reconciliation loop (every 5s) verifies the FDB set matches the expected set derived from Raft state. If drift is detected, only the drifted entries are corrected — not the entire table.

This keeps the steady-state cost at O(1) per placement change, not O(N) per reconciliation cycle.

### Consistency

FDB entries are eventually consistent with Raft state. The delay is bounded by the reconciliation interval (default 5 seconds) or the Raft apply notification (near-instant). In practice, FDB is updated within 1-2 seconds of a new VM placement being committed.

During this window, packets destined for a newly placed VM may be dropped. This is acceptable — the VM is still booting during this window anyway.

## 13. Cross-node VM networking flow (the endgame)

This is the complete flow that demonstrates every component working together.

```
Operator: syfrah compute vm create --name web-3 --zone eu-west-2 --subnet frontend \
          --project backend --org acme --vcpus 2 --memory 2048

                                    │
                                    ▼
1. Request arrives at Node C's Forge API (Node C is a follower)
   Node C forwards to the Raft leader (Node A)

                                    │
                                    ▼
2. Leader validates:
   - Org "acme" exists
   - Project "backend" exists in org "acme"
   - Subnet "frontend" exists, belongs to the right VPC
   - No naming conflict

                                    │
                                    ▼
3. Scheduler selects hypervisor:
   - Filter: zone == eu-west-2, state == Available, alive in gossip
   - Filter: capacity sufficient (2 vCPU, 2048 MB available)
   - Score: hv-eu-2 has the most headroom
   - Selected: hv-eu-2

                                    │
                                    ▼
4. Leader commits a batch of Raft commands:
   a. CreateInstance { spec: web-3, 2 vCPU, 2048 MB, image: ... }
   b. AllocateIp { subnet: frontend, vm: web-3 } → 10.1.0.5
   c. CreateNic { vm: web-3, subnet: frontend, vpc: prod, ip: 10.1.0.5,
                  mac: 02:00:0a:01:00:05 }
   d. PlaceVm { vm: web-3, hv: hv-eu-2, subnet: frontend,
                ip: 10.1.0.5, mac: 02:00:0a:01:00:05 }

                                    │
                                    ▼
5. ALL nodes apply the committed entries to their local redb.

                                    │
                      ┌─────────────┴──────────────┐
                      ▼                            ▼
   hv-eu-2 (selected hypervisor)         hv-eu-1 (other node with VMs in VPC prod)

   Forge sees PlaceVm for this node:     Forge sees PlaceVm for remote node:
   a. Ensure bridge syfbr-{vpc} exists   a. Add FDB entry:
   b. Ensure VXLAN syfvx-{vpc} exists       bridge fdb add 02:00:0a:01:00:05
   c. Add subnet gateway IP to bridge          dev syfvx-{vpc}
   d. Create TAP syftap-{web-3}                dst [hv-eu-2 fabric IPv6]
   e. Attach TAP to bridge               b. Add ARP proxy:
   f. Apply nftables rules (SGs)            ip neigh add 10.1.0.5
   g. Generate config-drive (IP, GW,          lladdr 02:00:0a:01:00:05
      DNS, MTU=1350)                          dev syfvx-{vpc} nud permanent
   h. Create ZeroFS volume
   i. Boot Cloud Hypervisor process

                                    │
                                    ▼
6. web-3 boots on hv-eu-2 with IP 10.1.0.5, MAC 02:00:0a:01:00:05
   Config-drive injects: IP 10.1.0.5/24, gateway 10.1.0.1, MTU 1350

                                    │
                                    ▼
7. Cross-node connectivity is live:
   web-1 on hv-eu-1 pings 10.1.0.5:
     → TAP → bridge → FDB lookup → VXLAN encap → syfrah0 (WireGuard)
     → internet → hv-eu-2 → WireGuard decrypt → VXLAN decap
     → bridge → TAP → web-3 receives

                                    │
                                    ▼
8. Leader responds to operator: VM web-3 created, IP 10.1.0.5, hypervisor hv-eu-2
```

## 14. Consistency guarantees

| Operation | Guarantee | Mechanism |
|-----------|-----------|-----------|
| **Writes** (create/update/delete any resource) | Strongly consistent (linearizable) | Raft quorum commit. A write is acknowledged only after a majority of nodes have persisted the log entry. |
| **Reads from leader** | Strongly consistent (when using `?consistent=true`) | Leader confirms it still holds the lease before responding. Without the flag, reads are sequentially consistent (served from the leader's latest committed state). |
| **Reads from followers** | Eventually consistent | Served from local redb, which may lag behind the leader by the replication delay (typically <100ms under normal conditions, up to seconds during high load or network issues). |
| **Gossip data** | Eventually consistent | SWIM dissemination in O(log N) rounds. Full convergence in 1-5 seconds for clusters up to 100 nodes. |
| **Forge reconciliation** | Eventual convergence | Within 1-3 reconciliation cycles (30-90 seconds) after state change. Faster when triggered by Raft apply notification (near-instant). |
| **FDB propagation** | Eventual | Derived from Raft state during reconciliation. New entries appear within 1-30 seconds. |

### Read-your-writes

The write response from the leader IS the authoritative confirmation. The client can trust it immediately.

However, subsequent reads from a different node may not reflect the write yet (replication lag, typically <100ms).

Contract:
- **Read from the same node that processed the write**: guaranteed read-your-writes (leader always has latest state)
- **Read from a follower**: eventually consistent (may lag by 1-2 Raft rounds)
- **Read with `?consistency=strong`**: forwarded to leader, linearizable, but higher latency

For Terraform providers and SDK clients: the create/update response contains the full resource state. Clients should use this response rather than immediately re-reading from a potentially stale follower.

### Ordering guarantees

- All Raft commands are totally ordered. Every node sees the same sequence.
- Within a single Raft transaction (batch), all commands are applied atomically.
- Gossip provides no ordering guarantees. Observations may arrive out of order.
- Forge reconciliation is idempotent — ordering of reconciliation cycles does not matter.

## 15. Failure modes

### Leader crash

**Detection**: followers stop receiving heartbeats. After the election timeout (1-2 seconds), an election begins.

**Recovery**: a new leader is elected, typically within 1-2 seconds. The new leader has all committed entries (Raft guarantees this). In-flight writes that were not committed are lost — clients must retry. Writes that were committed but not yet acknowledged to the client may have been applied — the idempotency mechanism (request ID deduplication) ensures retries are safe.

**Impact**: write unavailability for 1-3 seconds. Reads from followers continue uninterrupted. Existing VMs continue running.

### Minority partition

**Scenario**: network split isolates a minority of nodes from the majority.

**Majority side**: continues operating normally. Raft has quorum. Writes succeed. New VMs can be created.

**Minority side**: cannot write (no quorum). Raft followers see no leader heartbeats, start elections, but cannot win (not enough voters). Reads from local redb still work (stale data). Forge continues reconciling existing VMs based on the last known state. VMs on minority nodes keep running.

**Recovery**: when the partition heals, Raft replays missed log entries to the minority side. Gossip converges within seconds. Full operation resumes.

### Majority partition

**Scenario**: the majority side has quorum and continues operating. The minority side is effectively identical to the "minority partition" case above.

### Node crash

**VMs survive**: Cloud Hypervisor runs as separate OS processes. A Forge crash does not terminate VMs.

**Recovery**: Forge restarts, re-reads redb (state machine output), and re-reconciles. Running VMs are reconnected via Cloud Hypervisor's REST API. Missing network resources (nftables rules, FDB entries) are rebuilt from Raft state.

**If the node itself crashes** (hardware failure, kernel panic): VMs on that node are lost. Gossip detects the failure in ~5-15 seconds. The Raft leader waits 60 seconds (configurable), then marks the hypervisor as down. VMs with `restart_on_failure: true` are rescheduled to healthy hypervisors.

### redb corruption

**Scenario**: the state machine database file is corrupted (disk error, incomplete write despite ACID guarantees).

**Recovery**: delete the corrupted redb file. On startup, Forge detects the missing file and requests a full snapshot from the Raft leader. The leader sends the latest snapshot, and Forge rebuilds its local state from it. All subsequent log entries are replayed on top of the snapshot. This is functionally equivalent to adding a new node — openraft handles it natively.

### Network split between AZs

Raft requires a majority across ALL nodes, not per-AZ. Quorum math:

| Cluster size | Quorum | Tolerates | AZ distribution example |
|-------------|--------|-----------|------------------------|
| 3 nodes | 2 | 1 failure | AZ-1: 2 nodes, AZ-2: 1 node. AZ-2 loss = still have quorum. AZ-1 loss = no quorum (minority). |
| 5 nodes | 3 | 2 failures | AZ-1: 2, AZ-2: 2, AZ-3: 1. Any single AZ loss = still have quorum. |
| 7 nodes (5 voters) | 3 | 2 voter failures | Distribute voters across 3+ AZs for best resilience. |

**Recommendation**: for multi-AZ deployments, distribute nodes (especially voters) across at least 3 availability zones. This ensures that no single AZ failure can cause a quorum loss.

## 16. Cluster sizing

| Configuration | Nodes | Voters | Tolerates | Use case |
|--------------|-------|--------|-----------|----------|
| **Single node** | 1 | 1 | 0 failures | Development, testing, single-server deployment |
| **Two nodes** | 2 | 2 | 0 failures | Replication for data safety, but no HA. Either node failing blocks writes. |
| **Three nodes** | 3 | 3 | 1 failure | Minimum production HA. Recommended starting point. |
| **Five nodes** | 5 | 5 | 2 failures | Recommended for multi-AZ production. |
| **Seven+ nodes** | 7+ | 5 voters + N learners | 2 voter failures | Large deployments. Cap voters at 5 (or 7 max) to keep consensus fast. Additional nodes are learners. |

### Voter vs. learner tradeoffs

- **More voters** = higher write latency (more nodes must acknowledge each write) but better fault tolerance.
- **Learners** = receive full state replication, can serve reads, can be promoted to voter without data transfer, but do not participate in elections or quorum.
- **Promotion**: a learner can be promoted to voter at any time via `PromoteToVoter` command. This is useful for planned maintenance — promote a learner before draining a voter.

### Default node roles

- Every joined node runs the control plane runtime (Raft + gossip)
- Every node receives the full replicated state machine (needed for local reads and Forge reconciliation)
- New nodes join as **learners** by default (receive state, don't vote)
- Voter promotion is explicit: `syfrah controlplane promote <node>` or policy-driven (auto-promote up to N voters)
- Non-hypervisor nodes (routers, monitoring) can be voters if desired (they don't host VMs but participate in consensus)
- Recommended: voters are the most stable nodes (longest uptime, best network), not necessarily the busiest hypervisors

### The single-node case

Critical for adoption. A single-node Raft cluster is degenerate — the node is automatically leader, writes are committed locally (no replication latency), and there is zero consensus overhead. It behaves exactly like a local database. When the operator adds a second node, Raft begins replicating. When the third node joins, fault tolerance begins.

The migration path from 1 to N nodes is seamless. No configuration changes, no data migration, no downtime.

## 17. Bootstrap and migration

The migration from the current system (each node independent, local redb per node) to distributed consensus is incremental. No big-bang cutover.

### Phase 1: single-node "cluster" of 1

```
Node A (the only node):
  ┌──────────────────────────────┐
  │  openraft with 1 member       │
  │  Leader = Node A (automatic)  │
  │                               │
  │  All writes go through Raft   │
  │  (even though there's only    │
  │   one node — zero overhead)   │
  │                               │
  │  State machine writes to redb │
  │  Forge reads redb as before   │
  └──────────────────────────────┘
```

Functionally identical to today. The only difference: writes are routed through the Raft state machine instead of directly mutating redb. This validates the full command pipeline on a single node before introducing distribution.

### Migration cutover sequence

No downtime for already running VMs. A short mutation freeze is required during the Raft bootstrap on the initial node.

1. Operator initiates: `syfrah controlplane init`
2. Forge enters migration mode: rejects new mutations (creates, deletes) with `503 Service Unavailable — control plane migration in progress`
3. Running VMs continue operating — reconciliation continues for existing resources
4. Forge snapshots current redb state
5. Initializes single-node Raft cluster with snapshot as initial state
6. Switches write path from direct-redb to Raft
7. Resumes accepting mutations
8. Mutation freeze duration: typically 2-5 seconds (redb snapshot + Raft init)

Subsequent nodes joining the Raft cluster receive the state via Raft snapshot replication — no per-node migration needed.

Key guarantees:
- Running VMs: zero downtime (they are OS processes, independent of Forge)
- Reads: available throughout (local redb still serves)
- Writes: briefly unavailable (2-5s during cutover)
- No data loss: redb snapshot is the complete state

### Phase 2: add second node

```
Node A (leader)    ←── Raft replication ──→    Node B (follower)
  │                                              │
  redb (state)                                 redb (state)
  │                                              │
  Forge                                         Forge
```

- Node B joins the fabric mesh (WireGuard).
- Node B joins the Raft cluster via `AddRaftMember`.
- The leader sends a snapshot to Node B.
- Both nodes now have identical redb state.
- IPAM is shared — no more duplicate IPs.
- FDB entries are derived from shared placement state — cross-node VXLAN forwarding works.

**Limitation**: 2 nodes = no fault tolerance. Either node failing blocks writes. The system is operational but fragile.

### Phase 3: add third node (production minimum)

```
Node A    ←── Raft ──→    Node B    ←── Raft ──→    Node C
  │                         │                         │
  redb                     redb                      redb
  │                         │                         │
  Forge                    Forge                     Forge
```

- Quorum of 2/3 for writes.
- Tolerates 1 node failure.
- Full HA: leader crash results in automatic election, writes resume in 1-3 seconds.
- Cross-node VXLAN forwarding works between all three nodes.
- This is the minimum production configuration.

### Phase 4+: scale out

Additional nodes join the same way: `AddRaftMember`, snapshot transfer, full replication. Beyond 5 voters, new nodes join as learners.

## 18. CLI changes

```bash
# ── Cluster management ───────────────────────────────────────────

syfrah cluster status
# Output:
#   Cluster: syfrah-prod
#   Raft state: Healthy
#   Leader: hv-eu-1 (fd12:...:a1b2)
#   Term: 47
#   Commit index: 12,847
#   Last applied: 12,847
#   Members: 3 voters, 2 learners
#   Gossip: 5 alive, 0 suspect, 0 down

syfrah cluster members
# Output:
#   ID         NAME      ROLE      STATE     REGION      ZONE         FABRIC IPv6
#   hv-001     hv-eu-1   voter     Active    eu-west     eu-west-1    fd12:...:a1b2
#   hv-002     hv-eu-2   voter     Active    eu-west     eu-west-2    fd12:...:c3d4
#   hv-003     hv-eu-3   voter     Active    eu-central  eu-cent-1    fd12:...:e5f6
#   hv-004     hv-us-1   learner   Active    us-east     us-east-1    fd12:...:7890
#   hv-005     hv-us-2   learner   Draining  us-east     us-east-2    fd12:...:abcd

syfrah cluster add-member <node> [--voter | --learner]
# Add a fabric node to the Raft cluster.
# Default: voter if <5 voters exist, learner otherwise.

syfrah cluster remove-member <node>
# Remove a node from the Raft cluster.
# Fails if removal would break quorum.

syfrah cluster transfer-leader <node>
# Manually transfer Raft leadership to the specified node.
# Used for planned maintenance of the current leader.

syfrah cluster promote <node>
# Promote a learner to voter.

syfrah cluster demote <node>
# Demote a voter to learner.

# ── All existing commands work unchanged ─────────────────────────
# They now go through Raft instead of local redb.

syfrah compute vm create --name web-1 --subnet frontend --project backend --org acme
# → any node → forward to leader → scheduler → Raft commit
# → Forge on selected hypervisor reconciles → VM boots

syfrah org create acme
# → any node → forward to leader → Raft commit → all nodes see it

syfrah sg create web-sg --vpc prod --project backend --org acme
# → any node → forward to leader → Raft commit
# → all Forges with VMs in VPC prod re-derive nftables rules
```

## 19. Request Idempotency

All mutating API operations support a client-supplied `Idempotency-Key` header (or `client_token` field in the request body).

### The problem: lost responses

1. Client sends `POST /v1/instances` with `Idempotency-Key: abc-123`
2. Raft leader commits the create
3. Response is lost (network error, timeout)
4. Client retries with same `Idempotency-Key: abc-123`
5. State machine recognizes the key → returns the original result without re-executing

### Implementation

The state machine maintains a deduplication journal:
- Key: idempotency key (string, max 64 chars)
- Value: { command_index: u64, result: CommandResult, expires_at: u64 }
- TTL: 24 hours (after which the key is garbage collected)

On every command application:
1. Check if idempotency key exists in journal
2. If yes and not expired: return cached result (no re-execution)
3. If no: execute command, store result in journal with key

### Scope

- Idempotency keys are scoped per-client (identified by the key itself)
- Keys are stored in the Raft state machine (replicated, survives leader failover)
- The journal is compacted during Raft snapshots (expired entries removed)

### CLI behavior

The CLI auto-generates an idempotency key for every mutation:
`{command}_{timestamp}_{random}` — e.g., `create_vm_1711900000_a1b2c3`

This means CLI retries (e.g., user re-runs the same command after a timeout) may create a NEW key and thus a new resource. To get true retry safety, the user must supply `--idempotency-key` explicitly.

## 20. Performance

| Operation | Latency | Notes |
|-----------|---------|-------|
| Raft write (single command) | 2-10ms | Dominated by WireGuard RTT between nodes. Single datacenter: ~2ms. Cross-datacenter (EU to US): ~80-150ms. |
| Raft write (batch of N commands) | 2-10ms + negligible per-command | Batching amortizes the network round trip. |
| Read from leader (local redb) | <1ms | No network hop. Direct redb read. |
| Read from follower (local redb) | <1ms | No network hop. Eventually consistent. |
| Gossip propagation (cluster-wide) | 1-3 seconds | O(log N) rounds for SWIM. 5 nodes = ~2 rounds. |
| Scheduler decision | <1ms | In-memory computation. No I/O. Gossip data cached in memory. |
| Total VM create (API to running) | 15-30 seconds | Dominated by image pull + ZeroFS volume creation + Cloud Hypervisor boot. Raft + scheduling overhead is <100ms. |
| Snapshot creation | 1-10 seconds | Depends on state size. 50 MB redb = ~2 seconds. |
| Snapshot transfer to new node | 5-60 seconds | Depends on state size + network bandwidth. 50 MB over WireGuard at 100 Mbps = ~5 seconds. |

### Throughput

Raft write throughput is bounded by the leader's ability to replicate to a majority. With pipelined replication:

- **Single datacenter** (2ms RTT): ~1,000-5,000 writes/second
- **Cross datacenter** (100ms RTT): ~50-200 writes/second

Control plane operations (VM create, subnet create, SG update) are infrequent — tens to hundreds per minute in a busy cluster. The Raft throughput is orders of magnitude beyond what the control plane requires.

## 21. Security

### Transport security

All Raft and gossip traffic travels over the WireGuard fabric (syfrah0). WireGuard provides:

- **Encryption**: ChaCha20-Poly1305 for every packet.
- **Authentication**: only nodes with the mesh secret can join the fabric.
- **Integrity**: authenticated encryption prevents tampering.

There is no additional TLS layer. WireGuard's encryption is sufficient and avoids the complexity of certificate management.

### Membership control

- Only fabric members can participate in Raft. A node must first join the WireGuard mesh (manual approval or PIN), then be explicitly added to the Raft cluster via `AddRaftMember`.
- Leader election is restricted to voter nodes. An attacker who compromises a learner node cannot become leader.
- Removing a node from Raft (`RemoveRaftMember`) immediately stops it from receiving log entries. The node's local redb becomes stale and is eventually deleted.

### Command validation

The state machine validates every command before applying:

- **Authorization**: the command includes the requesting principal (user, API key). The state machine checks IAM permissions.
- **Referential integrity**: the command references valid, existing resources in the expected states.
- **Business rules**: deletion guards (cannot delete a VPC with active subnets), uniqueness constraints (no duplicate org names), capacity bounds (no subnet CIDR overlap).

No command can bypass Raft. Forge's API layer enforces that all mutations are routed through the Raft pipeline. Direct redb writes are not exposed.

### What per-request signatures protect against

- Message spoofing from outside the mesh
- Routing errors (message arrives at wrong handler)
- Replay attacks (with nonce/timestamp)
- Audit trail (which node sent what)

### What they do NOT protect against

- A fully compromised mesh member (the attacker has the signing key)
- A malicious voter corrupting the Raft log (Raft assumes honest majority)
- Side-channel attacks on the state machine

Per-request signatures improve message authenticity and auditability, but do not by themselves mitigate a fully compromised cluster member. True defense against compromised nodes requires Byzantine fault tolerance (BFT), which is out of scope.

### State machine determinism

The state machine must be **strictly deterministic**. Given the same log entries, every node must produce the same state. This means:

- No random number generation during apply (random values like ULIDs are generated before proposal, included in the command).
- No external I/O during apply (no network calls, no file reads).
- No dependency on wall clock time during apply (timestamps are included in the command by the proposer).

## 22. Observability

### Prometheus metrics

| Metric | Type | Description |
|--------|------|-------------|
| `syfrah_raft_leader_changes_total` | counter | Number of leader elections observed. |
| `syfrah_raft_term` | gauge | Current Raft term. |
| `syfrah_raft_commit_index` | gauge | Highest committed log index. |
| `syfrah_raft_last_applied` | gauge | Highest applied log index. Lag between commit and applied indicates slow state machine. |
| `syfrah_raft_apply_duration_seconds` | histogram | Time to apply a committed entry to redb. |
| `syfrah_raft_log_entries_total` | gauge | Total entries in the Raft log (before compaction). |
| `syfrah_raft_snapshot_duration_seconds` | histogram | Time to create a snapshot. |
| `syfrah_raft_replication_lag_entries` | gauge (per follower) | How far behind each follower is. |
| `syfrah_gossip_members_total` | gauge | Total members in the gossip membership list. |
| `syfrah_gossip_alive_total` | gauge | Members currently alive. |
| `syfrah_gossip_suspect_total` | gauge | Members currently suspected. |
| `syfrah_gossip_down_total` | gauge | Members declared down. |
| `syfrah_scheduler_placements_total` | counter | Total VM placements made by the scheduler. |
| `syfrah_scheduler_placement_retries_total` | counter | Placements that required retry (admission rejected). |
| `syfrah_scheduler_placement_duration_seconds` | histogram | Time from CreateInstance to PlaceVm commit. |
| `syfrah_api_leader_forwards_total` | counter | Requests forwarded from follower to leader. |

### Structured logging

Every significant event is logged with structured fields (JSON format):

- Raft apply: `{ "event": "raft_apply", "index": 12847, "command": "CreateInstance", "vm_id": "web-3", "duration_us": 340 }`
- Scheduler decision: `{ "event": "schedule", "vm_id": "web-3", "hypervisor": "hv-eu-2", "zone": "eu-west-2", "candidates": 4, "score": 0.87 }`
- Leader election: `{ "event": "leader_elected", "leader": "hv-eu-1", "term": 48, "election_duration_ms": 1200 }`
- Gossip state change: `{ "event": "gossip_state", "node": "hv-eu-3", "old": "alive", "new": "suspect" }`

### CLI health check

`syfrah cluster status` provides a real-time snapshot:

```
Cluster: syfrah-prod
Raft:
  State:          Healthy
  Leader:         hv-eu-1 (term 47, commit 12847)
  Applied:        12847 (lag: 0)
  Log entries:    2,341 (last compacted at 10506)
  Snapshot:       10506 (2 hours ago)
  Members:        3 voters, 2 learners
  
Gossip:
  Members:        5 alive, 0 suspect, 0 down
  Last probe:     0.3s ago
  
Scheduler:
  Placements:     1,247 total (3 retried)
  Last placement: 4m ago (web-3 → hv-eu-2)
```

## 23. Implementation phases

### Phase 1 — Raft core (~15 issues)

Implement the openraft integration: `RaftLogStorage`, `RaftStateMachine`, `RaftNetwork`. Single-node Raft that routes all writes through the log. Bootstrap migration (import existing redb data as initial snapshot). Basic cluster membership commands (`add-member`, `remove-member`). Raft health endpoint.

**Deliverable**: a single node where all writes go through Raft. Functionally identical to today, but via consensus.

### Phase 2 — State machine + IPAM (~10 issues)

Implement the full `RaftCommand` enum with validation. Migrate IPAM from direct redb writes to Raft commands. Implement the secondary indexes (`hypervisor_placements`, `vpc_placements`, etc.). Snapshot creation and installation.

**Deliverable**: IPAM allocations are strongly consistent across nodes. No duplicate IPs.

### Phase 3 — Scheduler + API gateway (~10 issues)

Implement the scheduler (filtering, scoring, selection). Implement API leader forwarding (follower proxies writes to leader). Implement the `syfrah cluster` CLI commands. Implement request ID deduplication.

**Deliverable**: operator can create a VM on any node, the scheduler places it automatically, and the API routes correctly.

### Phase 4 — SWIM gossip (~8 issues)

Integrate the `foca` crate. Implement `NodeReport` metadata. Implement gossip-based failure detection. Connect gossip to the scheduler (capacity hints, liveness checks). Implement the Raft leader's node-failure reconciler (down node leads to VM rescheduling).

**Deliverable**: cluster detects node failures in ~5-15 seconds and reschedules HA workloads.

### Phase 5 — Cross-node networking (FDB distribution) (~5 issues)

Implement FDB derivation from Raft placement state. Implement ARP proxy derivation. Validate end-to-end cross-node VXLAN forwarding. Test: VM on node A pings VM on node B through VXLAN over WireGuard.

**Deliverable**: VMs on different nodes can communicate over the overlay network.

### Phase 6 — Production hardening (~5 issues)

Raft snapshot scheduling and compaction tuning. Prometheus metrics export. Structured logging for all control plane events. Chaos testing (leader kill, network partition, node crash). Performance benchmarking (write throughput, read latency, scheduler decision time). Documentation.

**Deliverable**: production-ready control plane.

## 24. Rejected alternatives

### etcd / Consul (external consensus store)

Rejected. Architecture principle #1: **no external dependencies**. etcd requires a separate cluster of 3-5 nodes, a separate operational burden, a separate failure domain. Consul adds service mesh complexity. Both are external C/Go binaries — the Syfrah binary would depend on external services being available. openraft is embedded, pure Rust, starts with the process, and requires zero external infrastructure.

### CRDTs (Conflict-free Replicated Data Types)

Rejected. CRDTs provide eventual consistency without coordination — attractive for some workloads but **too weak for IPAM**. IP allocation requires strong consistency: two concurrent allocations must not produce the same IP. CRDTs cannot guarantee this without additional coordination, which defeats their purpose. Raft provides the strong consistency IPAM needs, and the performance is more than sufficient for control plane operations.

### Single leader without Raft (custom replication)

Rejected. A hand-rolled leader protocol without Raft's formal guarantees (leader completeness, state machine safety, log matching) is a recipe for data loss and split-brain. Raft is a well-understood, formally verified consensus protocol. openraft is a mature implementation. There is no reason to build a worse version from scratch.

### Gossip-only (no strong consensus)

Rejected. Gossip provides fast propagation but no ordering guarantees and no linearizability. Two nodes gossiping concurrent IPAM allocations can produce conflicting state that is expensive to resolve. Gossip is ideal for health and telemetry (where staleness is acceptable), but not for authoritative state (where conflicts are catastrophic). The two-layer architecture (Raft + gossip) gives each protocol the workload it excels at.

### Paxos

Rejected in favor of Raft. Raft and Paxos provide equivalent safety guarantees, but Raft is significantly easier to understand, implement, and debug. openraft is a production-quality Raft implementation. There is no mature, embeddable, async Paxos implementation for Rust.

### Database replication (PostgreSQL, CockroachDB)

Rejected. External database dependencies violate principle #1. CockroachDB uses Raft internally, but wrapping SQL around what is fundamentally a key-value state machine adds unnecessary complexity. redb is lighter, faster for our access patterns, and has zero external dependencies.

## 25. Commercial value

The control plane transforms Syfrah from a per-node VM manager into a distributed cloud platform. This is the inflection point for commercial viability.

**What it enables:**

- **Multi-node as the default.** Operators add nodes and the platform handles distribution. No manual VM placement, no manual FDB management, no manual IPAM coordination.
- **Automatic failure recovery.** Node dies, VMs reschedule. The operator sleeps through outages that would previously require manual intervention.
- **Cross-provider private networking.** VMs on OVH and Hetzner communicate over encrypted private IPs as if they were in the same datacenter. This is the core value proposition — a unified cloud across providers.
- **Horizontal scalability.** Adding a node adds capacity. The scheduler distributes workloads automatically. The operator grows the platform by renting more servers.
- **Topology-aware placement.** The scheduler respects zones, labels, taints, and anti-affinity. Operators can express "spread my web servers across availability zones" as a placement constraint.
- **API-first management.** Terraform providers, CLI tools, and custom integrations can target any node. The platform routes requests transparently. This is the foundation for a managed service.
- **Operational simplicity.** No etcd cluster to manage, no control plane nodes vs. worker nodes. Every node is equal. The operator manages servers, not infrastructure components.

## 26. References

- `layers/controlplane/README.md` — planned control plane design overview
- `handbook/ARCHITECTURE.md` — global architecture and stack diagram
- `handbook/state-and-reconciliation.md` — reconciliation philosophy, source of truth model, resource phase models
- `handbook/adr-001-networking-roadmap.md` — networking foundation (VPC, VXLAN, IPAM, FDB, config-drive)
- `handbook/adr-002-security-groups-route-tables.md` — security groups, route tables, NAT gateways, NICs
- `handbook/adr-003-forge.md` — Forge per-node orchestrator, materialized view consumption, reconciliation engine
- `handbook/adr-004-hypervisor-model.md` — hypervisor as first-class resource, Region/Zone/Hypervisor/VM topology
- `layers/fabric/README.md` — WireGuard mesh, fabric transport for Raft + gossip
- [openraft](https://github.com/databendlabs/openraft) — Rust Raft implementation
- [foca](https://github.com/caio/foca) — Rust SWIM implementation
- [redb](https://github.com/cberner/redb) — Rust embedded key-value store
- [Raft paper](https://raft.github.io/raft.pdf) — "In Search of an Understandable Consensus Algorithm" (Ongaro, Ousterhout 2014)
- [SWIM paper](https://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf) — "SWIM: Scalable Weakly-consistent Infection-style Process Group Membership Protocol" (Das, Gupta, Motivala 2002)
