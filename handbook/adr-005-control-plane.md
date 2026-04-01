# ADR-005: Control Plane — Distributed Coordination

**Status**: Proposed
**Date**: 2026-04-01
**Decided by**: Sacha + team
**Depends on**: ADR-001 (networking), ADR-002 (security groups/routes), ADR-003 (Forge), ADR-004 (hypervisor model)

## Context

Today each hypervisor is independent. IPAM allocates IPs from a local bitmap. FDB entries are distributed via fabric peer announcements. Placement is "whichever server you happen to SSH into." There is no coordination across nodes for any mutation: no global IPAM, no cross-node scheduling, no unified resource registry.

The result:

- Two hypervisors can allocate the same IP if they both process `vm create` concurrently against the same subnet.
- An operator must SSH into a specific hypervisor to create a VM there. There is no "create a VM in zone eu-west-1" command that picks the right machine.
- FDB distribution relies on fabric peer announcements — a broadcast mechanism with no consistency guarantees. If a node misses an announcement, it has no way to recover except by restarting.
- There is no single view of "what VMs exist in the cluster." Each node only knows about its own VMs.

After this ADR is implemented: an operator types `syfrah compute vm create --zone eu-west-1` on ANY node. The system places the VM on the best hypervisor in that zone, allocates a globally unique IP through Raft consensus, commits the placement, the target node's Forge reconciles (creates the VM, wires networking), and FDB entries are derived from the authoritative placement map on every relevant node. The VM boots with connectivity to all other VMs in its VPC across all hypervisors.

This is what turns a collection of independent hypervisors into a unified cloud.

### What this ADR covers

- The two-layer architecture (Raft consensus + SWIM gossip)
- Distributed IPAM via Raft
- The scheduler (placement algorithm, scoring, admission)
- FDB distribution derived from Raft state
- Tenant API routing (any-node access)
- CLI routing (transparent leader forwarding)
- Bootstrap-to-distributed migration
- The full state machine command set
- Failure scenarios and consistency model
- Implementation phases

### What this ADR does NOT cover

- Product-level orchestration (managed databases, load balancers) — future ADR
- IAM role enforcement on API endpoints — covered by api-architecture.md
- Forge internals (reconciliation loop, capacity management, health checks) — ADR-003
- Hypervisor registration and lifecycle — ADR-004
- Security group rule evaluation and nftables generation — ADR-002
- Overlay networking primitives (VXLAN, bridges, TAPs) — ADR-001

## What the Control Plane IS

The control plane is the distributed coordination layer that makes the cluster behave as a single system. It runs on **every node** — there are no dedicated controller machines. Internally, one node is elected Raft leader for write operations. This election is automatic and invisible to operators.

Two protocols, two purposes:

1. **Raft consensus (openraft)** — strongly consistent state for everything that must never conflict: resource definitions, IP allocations, VM placements, hypervisor registry, VPC/subnet/SG configuration, org/project/env hierarchy.

2. **SWIM gossip (foca)** — eventually consistent state for everything that describes what is happening right now: hypervisor capacity reports, node health/heartbeat, drain status, VM runtime status, node reachability.

The control plane does NOT execute anything. It decides what should exist and where. Forge (ADR-003) executes those decisions locally on each node.

## What the Control Plane is NOT

- **Not a separate process.** The control plane is embedded in the syfrah daemon, alongside Forge, the fabric, and the API server. One binary, one process.
- **Not a separate cluster.** There is no etcd cluster, no Consul deployment, no external coordination service. The Raft cluster IS the syfrah cluster.
- **Not the execution layer.** The control plane commits "VM web-1 should run on hv-002." Forge on hv-002 actually creates the VM. The control plane never calls `cloud-hypervisor` or `ip link`.
- **Not an event bus.** Gossip carries advisory state, not commands. The reconciliation loop on each Forge reads Raft state (the materialized view in redb) and acts on it. There is no event-driven dispatch from the control plane to Forge.
- **Not a central API gateway.** Every node can accept API requests. The control plane handles forwarding writes to the Raft leader transparently. There is no single point of entry.

## Design Principles

1. **Embedded, not external.** The control plane ships inside the syfrah binary. No additional infrastructure to deploy, monitor, or maintain. A single node running syfrah has a fully functional control plane (single-node Raft, instant commits, zero overhead).

2. **Raft for consistency, gossip for liveness.** These two protocols solve different problems. Never mix them. Gossip tells you "what's happening now." Raft tells you "what should exist." If losing or duplicating a piece of state would violate a user-facing invariant (double IP, orphan VM, unauthorized access), it goes in Raft. If staleness just means slightly degraded scheduling decisions, it goes in gossip.

3. **Reads are local, writes go through the leader.** Every node has a local copy of the full Raft state machine (redb). Reads are served instantly from local state. Writes are forwarded to the Raft leader, committed with majority quorum, then applied to every node's local redb.

4. **The scheduler runs on the leader.** Only the leader has the most up-to-date committed state. The scheduler consumes gossip for capacity hints, but the placement decision is a Raft write — atomic, replicated, authoritative.

5. **Derived state is recomputed, not distributed.** FDB entries, nftables rules, ARP proxy tables — all are computed locally by each Forge from the Raft state machine. There is no separate distribution protocol for derived state.

6. **The CLI does not change.** The operator types the same commands on any node. Forge transparently routes mutations to the Raft leader. The CLI has no concept of Raft, leader, or consensus.

7. **Migration is incremental.** Bootstrap mode (local redb) already works. Adding openraft means writes go through the Raft log before reaching redb. The read path (Forge reads redb) is unchanged. The migration is adding a write path, not rewriting the system.

## Two-Layer Architecture: Raft + Gossip

```
┌──────────────────────────────────────────────────────────────────┐
│  Raft Consensus (openraft)                                       │
│  Strongly consistent — linearizable writes, sequential reads     │
│                                                                  │
│  What goes here:                                                 │
│  - Org / Project / Environment definitions                       │
│  - VPC / Subnet / VNI allocations                                │
│  - Security Group definitions and rules                          │
│  - Route Table definitions and routes                            │
│  - NAT Gateway definitions                                       │
│  - IPAM bitmaps (IP allocations per subnet)                      │
│  - VM definitions (desired spec)                                 │
│  - VM placement decisions (VM → hypervisor mapping)              │
│  - Volume definitions and attachments                            │
│  - Hypervisor registry (static config, state, labels, taints)    │
│  - VPC peering relationships                                     │
│  - Cluster membership (which nodes are in the Raft group)        │
│                                                                  │
│  Protocol: leader-based. Writes → leader → replicate to          │
│  majority → commit → apply to each node's redb state machine.    │
├──────────────────────────────────────────────────────────────────┤
│  SWIM Gossip (foca)                                              │
│  Eventually consistent — converges in O(log N) rounds            │
│                                                                  │
│  What goes here:                                                 │
│  - HypervisorReport (capacity, utilization, VM count, health)    │
│  - NodeReport (basic health for non-hypervisor nodes)            │
│  - Drain status                                                  │
│  - Forge version, uptime                                         │
│  - Node reachability                                             │
│                                                                  │
│  Protocol: decentralized. Each node probes peers, piggybacks     │
│  state updates on protocol messages. No leader, no quorum.       │
└──────────────────────────────────────────────────────────────────┘
```

### Source of truth table

Every piece of state has exactly one authoritative source. This table is the definitive reference.

| State | Source of truth | Stored in | Consistency | Notes |
|---|---|---|---|---|
| Org/Project/Env definitions | Tenant API → Raft | Raft log → redb | Strong | Authoritative hierarchy |
| VPC definition (VNI, CIDR) | Tenant API → Raft | Raft log → redb | Strong | VNI allocated by Raft (uniqueness) |
| Subnet definition (CIDR, gateway) | Tenant API → Raft | Raft log → redb | Strong | Within VPC CIDR |
| IP allocations (bitmap + records) | Raft (leader processes sequentially) | Raft log → redb | Strong | Must never double-allocate |
| MAC addresses | Derived from IP | Computed (`02:00:{ip_hex}`) | N/A | Deterministic, not stored |
| Security group definitions + rules | Tenant API → Raft | Raft log → redb | Strong | Authoritative |
| Route table definitions + routes | Tenant API → Raft | Raft log → redb | Strong | Authoritative |
| NAT Gateway definitions | Tenant API → Raft | Raft log → redb | Strong | Authoritative |
| VM desired spec (vCPU, memory, image, network) | Tenant API → Raft | Raft log → redb | Strong | What the user asked for |
| VM placement (VM → hypervisor) | Scheduler → Raft | Raft log → redb | Strong | One hypervisor per VM |
| Volume definitions + attachments | Tenant API → Raft | Raft log → redb | Strong | Must be consistent (one VM at a time) |
| Hypervisor registry (specs, state, labels, taints) | Registration → Raft | Raft log → redb | Strong | Authoritative canonical record |
| VPC peering relationships | Tenant API → Raft | Raft log → redb | Strong | Authoritative |
| Cluster membership | Raft internal | Raft log | Strong | Who is in the Raft group |
| Hypervisor capacity (allocatable) | Forge (local redb) → gossip | Gossip (in-memory) | Eventual | Advisory for scheduler |
| Hypervisor health/heartbeat | Forge → gossip | Gossip (in-memory) | Eventual | Advisory |
| Hypervisor utilization (CPU/mem %) | Forge → gossip | Gossip (in-memory) | Eventual | Scheduling hint |
| VM runtime status (running/stopped/error) | Forge → gossip | Gossip (in-memory) | Eventual | Observed reality |
| Drain status | Forge → gossip | Gossip (in-memory) | Eventual | Scheduling exclusion |
| VXLAN bridges (per node) | Derived from Raft (VPC + VM placement) | Forge creates locally | Derived | Exists only if node has VMs in VPC |
| FDB entries (MAC → VTEP) | Derived from Raft (IP allocation + VM placement) | Forge populates locally | Derived | Recomputed from Raft state |
| ARP proxy entries | Derived from Raft (IP allocation) | Forge populates locally | Derived | Recomputed from Raft state |
| nftables rules | Derived from Raft (SG rules + VM/NIC mapping) | Forge applies locally | Derived | Recomputed from Raft state |
| DNS records | Derived from Raft (IP allocation + VM name) | CoreDNS zone files (local) | Derived | Generated by Forge from Raft state |

**The invariant**: Raft is truth. Derived state is recomputed from Raft. Gossip is advisory. If derived state and observed state disagree, Forge reconciles.

## Raft Consensus (openraft)

### Why Raft

The control plane needs a consensus algorithm that provides:

- **Linearizable writes.** Two concurrent IP allocations against the same subnet must be serialized. Two concurrent VM placements must not double-book a hypervisor's last available slot.
- **Automatic leader election.** When the leader fails, a new leader must be elected without operator intervention.
- **Log replication.** Every committed mutation must be replicated to a majority of nodes before being acknowledged.
- **Embedded operation.** No external service dependency. The consensus algorithm runs inside the syfrah binary.
- **Single-node degeneracy.** With one node, Raft must work with zero overhead — the node is its own leader, commits are instant.

openraft satisfies all of these. It is a Rust, async, tokio-native Raft implementation used in production by Databend. It supports 1-N nodes, provides the three-trait abstraction (`RaftLogStorage`, `RaftStateMachine`, `RaftNetwork`), and handles leader election, log replication, snapshots, and cluster membership changes.

Alternatives considered are documented in "Rejected Alternatives."

### What goes into Raft

Every mutation that changes the desired state of the cluster. Specifically:

- Resource CRUD: orgs, projects, environments, VPCs, subnets, security groups, rules, route tables, routes, NAT gateways, VMs, volumes, volume attachments, VPC peerings
- IPAM: IP allocation and release (bitmap mutations)
- Scheduling: VM placement decisions (VM → hypervisor mapping)
- Hypervisor lifecycle: registration, activation, draining, decommissioning, label/taint updates
- Cluster membership: adding/removing Raft voters and learners

Nothing else. Gossip data never enters the Raft log. Derived state (FDB, nftables, ARP proxy) is never stored in Raft — it is computed from Raft state by each Forge.

### State Machine

The Raft state machine is the function that takes a committed log entry and applies it to produce updated state. In Syfrah, the state machine backend is **redb** — the same embedded key-value store used in bootstrap mode.

```
Committed Raft log entry
    │
    ▼
State machine apply function
    │
    ├── Validates the command (idempotency check, precondition check)
    ├── Applies mutations to redb tables (atomic transaction)
    ├── Returns the result (success/failure + any allocated values)
    └── Optionally triggers immediate Forge reconciliation
```

The state machine is deterministic: given the same log entries in the same order, every node produces the same redb state. This is the foundation of Raft's consistency guarantee.

**redb tables managed by the state machine:**

| Table | Key | Value | Purpose |
|---|---|---|---|
| `orgs` | `OrgId` | `Org` | Organization definitions |
| `projects` | `ProjectId` | `Project` | Project definitions |
| `environments` | `EnvironmentId` | `Environment` | Environment definitions |
| `vpcs` | `VpcId` | `Vpc` | VPC definitions (VNI, CIDR) |
| `subnets` | `SubnetId` | `Subnet` | Subnet definitions |
| `ipam_bitmaps` | `SubnetId` | `Bitmap` | IP allocation bitmaps |
| `ip_allocations` | `(SubnetId, Ipv4Addr)` | `IpAllocation` | IP allocation records |
| `security_groups` | `SecurityGroupId` | `SecurityGroup` | SG definitions |
| `sg_rules` | `RuleId` | `SecurityGroupRule` | SG rules |
| `route_tables` | `RouteTableId` | `RouteTable` | Route table definitions |
| `routes` | `RouteId` | `Route` | Route entries |
| `nat_gateways` | `NatGatewayId` | `NatGateway` | NAT GW definitions |
| `network_interfaces` | `NicId` | `NetworkInterface` | NIC records |
| `vms` | `VmId` | `Vm` | VM desired state |
| `vm_placements` | `VmId` | `VmPlacement` | VM → hypervisor mapping |
| `volumes` | `VolumeId` | `Volume` | Volume definitions |
| `volume_attachments` | `VolumeId` | `VolumeAttachment` | Volume → VM mapping |
| `hypervisors` | `HypervisorId` | `Hypervisor` | Hypervisor registry |
| `vpc_peerings` | `PeeringId` | `VpcPeering` | VPC peering relationships |
| `vni_counter` | `()` | `u32` | Next available VNI |

These are the same tables Forge reads during reconciliation (ADR-003). In bootstrap mode, Forge reads and writes them directly. In distributed mode, Forge reads them (the state machine output), but writes go through Raft.

### Log Storage

openraft requires a durable log of all uncommitted and recently committed entries. Our implementation:

- **Backend**: dedicated redb table (`raft_log`) or a separate file-backed append-only log.
- **Durability**: every append is fsync'd before acknowledging. A crash between append and apply is safe — Raft replays uncommitted entries on restart.
- **Compaction**: after a snapshot is taken, log entries before the snapshot index are discarded. This bounds log growth.

The log storage is separate from the state machine storage. The log contains raw commands; the state machine (redb tables above) contains the applied result.

### Snapshots

Snapshots are periodic captures of the full state machine. They serve two purposes:

1. **Log compaction.** Without snapshots, the Raft log grows unboundedly. After a snapshot at index N, log entries 1..N can be discarded.
2. **Fast node recovery.** A new or rejoining node receives the latest snapshot instead of replaying the entire log from the beginning.

**Snapshot strategy:**

- Trigger: every 10,000 committed entries, or when a new node joins and needs catch-up.
- Format: a redb database file serialized to bytes (or a structured export of all tables).
- Transfer: the leader sends the snapshot to the joining node over the fabric (HTTP on `syfrah0`).
- Application: the receiving node replaces its local redb with the snapshot, then replays any log entries after the snapshot index.

Snapshots are not incremental in Phase 1. For clusters with moderate state (thousands of VMs, not millions), a full snapshot is small enough (single-digit megabytes) to transfer quickly.

### Leader Election

openraft handles leader election automatically. The key parameters:

| Parameter | Default | Rationale |
|---|---|---|
| Election timeout | 1000-2000ms (randomized) | Must be > heartbeat interval to avoid spurious elections |
| Heartbeat interval | 300ms | Keeps followers aware the leader is alive |
| Election timeout randomization | 1000ms range | Prevents split votes from synchronized timeouts |

**What happens during election:**

1. The current leader stops sending heartbeats (crash, network issue, or graceful shutdown).
2. After the election timeout expires, a follower transitions to candidate state and requests votes.
3. The candidate with the most up-to-date log wins (Raft's log-completeness property).
4. The new leader begins accepting writes and sending heartbeats.
5. Total failover time: 1-3 seconds in normal conditions.

During election, **writes are blocked** (no leader to process them). Reads from local state machines continue uninterrupted — they may be slightly stale (last committed state before the leader failed), but this staleness is bounded and safe.

### Cluster Membership (adding/removing nodes)

Raft cluster membership changes (adding a voter, removing a voter, converting voter to learner) are themselves Raft log entries. openraft supports joint consensus for safe membership transitions.

**Adding a node:**

1. New node joins the WireGuard mesh (fabric layer).
2. Operator (or auto-discovery) triggers `AddLearner(node_id)` — the new node replicates the log but does not vote.
3. The learner catches up to the leader's log.
4. Once caught up, the leader promotes the learner to voter via `ChangeMembership`.
5. The new node now participates in elections and quorum.

**Removing a node:**

1. Operator triggers `ChangeMembership` removing the node from the voter set.
2. The node becomes a learner (still replicates, doesn't vote).
3. Optionally, the node is fully removed from the Raft group.
4. Quorum requirement adjusts automatically.

**Safety:** Raft guarantees that membership changes are committed with the old configuration's quorum before the new configuration takes effect. Split-brain during membership change is impossible.

### Read vs Write Paths

**Write path (linearizable):**

```
Client request (e.g., "create VM")
    │
    ▼
Any node receives the request
    │
    ├── This node IS the leader → process locally
    └── This node is NOT the leader → forward to leader
                                         │
                                         ▼
Leader validates the command
    │
    ▼
Leader appends to local log
    │
    ▼
Leader replicates to followers
    │
    ▼
Majority acknowledge → entry is committed
    │
    ▼
State machine applies the entry to redb (on all nodes)
    │
    ▼
Leader returns result to client
```

**Read path (default: eventually consistent):**

```
Client request (e.g., "list VMs")
    │
    ▼
Any node receives the request
    │
    ▼
Read directly from local redb (state machine output)
    │
    ▼
Return result
```

The local redb may be one or two heartbeats behind the leader. For most reads (listing VMs, showing VPC config, displaying hypervisor status), this staleness is imperceptible and acceptable.

**Read path (linearizable, on request):**

For reads that must reflect the latest committed state (rare — e.g., "did my IP allocation succeed?"), the client can request a linearizable read:

1. The request is forwarded to the leader.
2. The leader confirms it is still the leader (by checking that a majority of followers are responsive).
3. The leader reads from its local state machine (which is guaranteed to be up-to-date).
4. The leader returns the result.

This adds one round-trip to the leader. Most clients never need it — the write path already returns the result of the mutation.

### Consistency Guarantees

| Operation | Guarantee | Mechanism |
|---|---|---|
| Writes (create, update, delete) | Linearizable | Raft leader processes sequentially, commits with majority |
| Default reads | Eventually consistent | Local state machine, bounded staleness (1-2 heartbeats) |
| Linearizable reads (explicit) | Linearizable | Leader read with quorum confirmation |
| Gossip reads | Best-effort eventual | SWIM protocol, O(log N) convergence |

## SWIM Gossip

### What goes into Gossip

Gossip carries **advisory, non-authoritative** state. It is consumed by the scheduler for placement hints and by dashboards for operational visibility. It is never the source of truth for mutations.

| Data | Producer | Consumer | Staleness tolerance |
|---|---|---|---|
| `HypervisorReport` (capacity, utilization, VM count, health, labels, taints, state) | Forge on each hypervisor | Scheduler, dashboards | 2-10 seconds |
| `NodeReport` (basic health for non-hypervisor nodes) | Forge on each node | Dashboards | 2-10 seconds |
| Drain status | Forge (from Raft state) | Scheduler | 2-10 seconds |
| Forge version, uptime | Forge | Dashboards | Minutes |
| Node reachability | SWIM protocol | Scheduler, failure detector | Seconds |

### Protocol (SWIM with suspicion)

The gossip layer uses **SWIM (Scalable Weakly-consistent Infection-style Membership)** via the `foca` crate. The protocol operates over the WireGuard fabric — all gossip messages are encrypted in transit.

**Protocol messages:**

1. **Ping**: direct probe to a random peer. Expects an Ack.
2. **Ping-req**: if Ping times out, ask K random peers to probe the target indirectly.
3. **Ack**: response to Ping/Ping-req.

**Failure detection sequence:**

```
Node A probes Node C (Ping)
    │
    ├── Ack received within timeout → Node C is alive
    │
    └── No Ack → Node A asks Node B to probe Node C (Ping-req)
         │
         ├── Node B gets Ack from C → C is alive (reported to A)
         │
         └── No Ack → Node C is marked as Suspect
              │
              ├── Suspect timeout expires, no refutation → Node C is Dead
              │
              └── Node C sends any message → Suspect cleared, C is Alive
```

**Tuning parameters:**

| Parameter | Default | Effect |
|---|---|---|
| Protocol period | 1 second | How often each node probes a random peer |
| Ping timeout | 500ms | Time to wait for a direct Ack |
| Suspicion timeout | 5 seconds | How long a node stays Suspect before being declared Dead |
| Indirect probes (K) | 3 | Number of peers asked for indirect probe |

With these defaults, failure detection takes 5-10 seconds. This is fast enough for scheduling exclusion but not so aggressive that network jitter causes false positives.

### Dissemination (piggyback on protocol messages)

Gossip updates (HypervisorReport, health changes, member events) are **piggybacked on protocol messages** rather than sent as separate packets. When Node A sends a Ping to Node B, it includes any pending state updates. This provides:

- **Zero extra network overhead** — updates ride on messages that would be sent anyway.
- **O(log N) convergence** — each update reaches all N nodes in O(log N) protocol rounds (~2-5 seconds for clusters of 10-100 nodes).
- **Bounded bandwidth** — the number of piggybacked updates per message is capped. High-priority updates (member events) take precedence over low-priority updates (capacity reports).

### Failure Detection

Gossip-based failure detection is the first line of defense. It is fast but advisory — it does not trigger authoritative state changes on its own.

| Event | Detection time | Action |
|---|---|---|
| Node stops responding | 5-10 seconds | Gossip marks node as Dead. Scheduler stops placing VMs. |
| Node recovers | Next protocol round (1 second) | Gossip marks node as Alive. Scheduler resumes placing VMs. |
| Intermittent failures | Suspicion period (5 seconds) | Node is Suspect — scheduler may still place VMs (with lower score). |

### Relationship to Raft

Gossip and Raft are complementary but independent:

- **Gossip does not participate in Raft.** Gossip membership is not the same as Raft membership. A node can be in the gossip pool (receiving health updates) without being a Raft voter.
- **Raft does not depend on gossip.** If gossip fails entirely, Raft continues operating — writes still commit, reads still work. The scheduler loses capacity hints and falls back to Raft-only data (hypervisor records).
- **Gossip informs Raft decisions.** The Raft leader runs a failure detector that reads gossip state. When gossip reports a node as Dead for >60 seconds, the leader MAY commit a `MarkHypervisorUnreachable` entry to Raft. This is a policy decision by the leader, not an automatic gossip-to-Raft bridge.

```
SWIM Gossip                          Raft Consensus
    │                                     │
    │  "Node C hasn't responded           │
    │   for 15 seconds"                   │
    │          │                          │
    │          ▼                          │
    │  Scheduler stops placing            │
    │  VMs on Node C                      │
    │          │                          │
    │          │  (60 seconds pass)        │
    │          ▼                          │
    │  Leader's failure detector           │
    │  reads gossip state                 │
    │          │                          │
    │          └──────────────────────────▶│
    │                                     │  Leader commits:
    │                                     │  MarkHypervisorUnreachable(C)
    │                                     │
    │                                     │  If HA VMs on C:
    │                                     │  Leader commits:
    │                                     │  RescheduleVm(vm_id, new_hv)
```

## Scheduler

### What the Scheduler Does

The scheduler is the component that decides **where** a new VM runs. It takes a VM creation request with optional placement constraints (zone, labels, anti-affinity) and selects the best hypervisor from the available pool.

The scheduler runs on the **Raft leader only**. This is not an arbitrary choice — the leader has the most up-to-date committed state (VM placements, hypervisor records, IPAM). Running the scheduler on a follower would require forwarding the placement decision to the leader anyway, adding latency without benefit.

The scheduler is invoked on:

- `PlaceVm` — new VM needs a hypervisor
- `RescheduleVm` — existing VM needs to move (node failure, drain)

### Placement Algorithm (scoring model)

The scheduler uses a **filter-then-score** model, consistent with the placement evaluation order defined in ADR-004.

```
VM creation request
    │
    ▼
┌─────────────────────────────────┐
│  Phase 1: Filter                │
│                                 │
│  Start with all hypervisors     │
│  from Raft state                │
│                                 │
│  1. State == Available          │
│  2. Zone constraint (if any)    │
│  3. Label selector match        │
│  4. Taint/toleration match      │
│  5. Capacity >= requested       │
│                                 │
│  Result: candidate set          │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│  Phase 2: Score                 │
│                                 │
│  For each candidate:            │
│  6. Anti-affinity penalty       │
│  7. Spread-topology bonus       │
│  8. Utilization score (gossip)  │
│                                 │
│  Result: ranked candidates      │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│  Phase 3: Commit                │
│                                 │
│  Select top candidate           │
│  Commit PlaceVm to Raft         │
│  Return hypervisor_id to caller │
└─────────────────────────────────┘
```

### Constraint Evaluation Order

The order matters — more restrictive constraints are evaluated first to prune the candidate set early.

1. **State filter**: only hypervisors in `Available` state (from Raft). Excludes `Draining`, `Maintenance`, `Decommissioned`, `Disabled`.
2. **Zone filter**: if `--zone` specified, only hypervisors in that zone (from Raft).
3. **Node-selector filter**: if `--node-selector` specified, only hypervisors whose labels match all selectors (from Raft).
4. **Taint filter**: exclude hypervisors with taints not tolerated by the VM spec (from Raft).
5. **Capacity filter**: exclude hypervisors without enough allocatable vCPUs, memory, and local disk. Capacity is read from gossip (`HypervisorReport`). If gossip data is unavailable for a hypervisor, it is excluded (fail-safe).

If no candidates remain after filtering, the scheduler returns an error: `COMPUTE_INSUFFICIENT_RESOURCES` with a message indicating which constraint eliminated all candidates.

### Anti-affinity and Spread

6. **Anti-affinity**: if `--anti-affinity-group` specified, penalize hypervisors that already host VMs in that group (from Raft's VM placement records). Hard anti-affinity fails if no separate hypervisor is available; soft anti-affinity (default) applies a scoring penalty.
7. **Spread-topology**: if `--spread-topology zone` specified, prefer zones with fewer VMs from this group (from Raft). This distributes replicas across failure domains.

### Resource Scoring (gossip-based)

8. **Utilization score**: from gossip `HypervisorReport`, prefer hypervisors with lower actual utilization (`host_cpu_percent`, `host_memory_percent`). The default strategy is **spreading** (distribute load evenly for resilience). Bin-packing (fill one node before using the next, for cost efficiency) is configurable per-org.

Scoring formula (spreading):
```
score = (1.0 - cpu_utilization) * 0.5 + (1.0 - memory_utilization) * 0.5
```

Gossip staleness: the utilization data may be 2-10 seconds old. This is acceptable for scoring — the scheduler picks the best-known candidate, and Forge's admission control (below) provides the hard guarantee.

### Placement Commit (Raft write)

9. **Select** the highest-scoring hypervisor.
10. **Commit** a `PlaceVm { vm_id, hypervisor_id, ... }` entry to the Raft log.
11. Once committed, the placement is authoritative. Every node's state machine applies it to the `vm_placements` table.
12. The target hypervisor's Forge sees the new VM in its materialized view and begins reconciliation.

### Forge Admission Recheck

Even after the scheduler selects a hypervisor, Forge performs **local admission control** before creating the VM. This guards against race conditions where:

- The gossip capacity was stale (another VM was placed concurrently).
- The hypervisor's actual available resources changed between scheduling and execution.

Forge checks: does this hypervisor have enough allocatable vCPUs, memory, and local disk for this VM? If yes, proceed. If no, reject.

### Retry on Rejection

If Forge rejects the placement:

1. Forge reports the rejection via gossip (capacity updated).
2. The control plane detects the rejection (Forge sets VM phase to `Failed` with reason `AdmissionRejected`).
3. The leader's reconciler observes the failed VM and re-invokes the scheduler.
4. The scheduler excludes the rejected hypervisor (its gossip capacity is now updated) and picks a new one.
5. A new `PlaceVm` is committed to Raft.
6. Maximum retries: 3. After 3 rejections, the VM transitions to `Failed` with `COMPUTE_INSUFFICIENT_RESOURCES`.

### Future: In-flight Reservations

The current design has a TOCTOU gap: between the scheduler reading gossip capacity and Forge checking actual capacity, another placement may consume the resources. The admission recheck + retry mitigates this.

A future optimization is **in-flight reservations**: the scheduler temporarily reserves capacity on the target hypervisor (via a Raft write) before committing the full placement. Forge honors reservations when checking admission. Reservations expire after a configurable timeout (default: 60 seconds, as specified in ADR-003's `reservation_expiry_secs`). This is deferred to Phase 4 to avoid premature complexity.

## Distributed IPAM

### Global Uniqueness via Raft

IP allocation is the canonical example of why distributed consensus exists. Without Raft:

- Node A allocates `10.0.1.5` for VM-1 from its local bitmap.
- Concurrently, Node B allocates `10.0.1.5` for VM-2 from its local bitmap.
- Both succeed locally. Conflict.

With Raft, all IP allocations go through the leader. The leader processes them sequentially. No conflicts are possible.

### Allocation: Raft Write

```
AllocateIp { subnet_id, vm_id }
    │
    ▼
Leader's state machine:
    │
    1. Read bitmap for subnet_id from redb
    2. Find first available bit (scanning from .3)
    3. Set the bit
    4. Create IpAllocation record { ip, subnet_id, vm_id, mac, state: Reserved }
    5. Write updated bitmap + allocation record to redb
    6. Return allocated IP + MAC to caller
```

The entire operation is atomic (single redb transaction within the state machine apply). The bitmap and allocation record are updated together. No partial state.

### Release: Raft Write

```
ReleaseIp { subnet_id, ip }
    │
    ▼
Leader's state machine:
    │
    1. Read bitmap for subnet_id from redb
    2. Clear the bit for the given IP
    3. Delete or update IpAllocation record (state → Released or delete)
    4. Write updated bitmap to redb
```

### No More Per-Node IPAM

In the distributed control plane, there is **no per-node IPAM**. The bitmap lives in the Raft state machine. Every node has a local copy (redb), but only the leader mutates it. This is the critical change from ADR-001's design, which assumed a per-node bitmap.

The per-node bitmap was necessary in bootstrap mode (single node, no coordination). With Raft, the bitmap is globally consistent. A VM on hypervisor A and a VM on hypervisor B in the same subnet will always get unique IPs.

### Migration from Local IPAM

1. **Bootstrap mode (today)**: per-node bitmap in local redb. Works for single-node deployments.
2. **Control plane migration**: the local redb IS the Raft state machine. When openraft is introduced, the existing bitmap becomes the initial Raft state (via snapshot import). No data migration needed — the table structure is unchanged.
3. **Multi-node**: once Raft is running, all IP allocations go through the leader. The local bitmap on each node is updated by the state machine apply (replicated from the leader's committed log entries).

## FDB Distribution

### Derived from Raft State

FDB entries tell each VXLAN bridge where to send frames destined for a given MAC address. In the current design (ADR-001), FDB entries are distributed via fabric peer announcements — a fire-and-forget broadcast mechanism.

With the control plane, FDB entries are **derived from Raft state**. The authoritative data is:

- `vm_placements` table: which VM (and its MAC/IP) is on which hypervisor
- `vpcs` table: which VPCs exist and their VNIs
- `hypervisors` table: each hypervisor's fabric IPv6 (VTEP address)

From these three tables, each Forge can compute the complete FDB table for every VPC that has VMs on its node.

### On Raft Commit of New VM Placement

When a `PlaceVm` entry is committed:

1. Every node's state machine applies it to the `vm_placements` table.
2. Every Forge that manages VMs in the same VPC detects the new placement during reconciliation.
3. Each Forge computes the FDB entry: `MAC → remote hypervisor's fabric IPv6`.
4. Each Forge applies the FDB entry to the local VXLAN bridge: `bridge fdb add {mac} dev syfvx-{vpc_id} dst {hypervisor_fabric_ipv6}`.
5. Each Forge adds the ARP proxy entry: `ip neigh add {vm_ip} lladdr {mac} dev syfvx-{vpc_id} nud permanent`.

### On Raft Commit of VM Deletion

When a `RemoveVm` entry is committed:

1. Every node's state machine removes the entry from `vm_placements`.
2. Every Forge in the same VPC detects the removal during reconciliation.
3. Each Forge removes the FDB entry and ARP proxy entry for that VM.

### Forge Reconciliation Rebuilds FDB from Raft State on Restart

If a node restarts, its Forge rebuilds the entire FDB table from scratch:

1. Read all `vm_placements` for VPCs that have VMs on this node.
2. For each remote VM in those VPCs, compute the FDB entry.
3. Apply all FDB entries to the local VXLAN bridges.

No gossip replay needed. No fabric announcement catch-up needed. The Raft state machine (local redb) has the complete, authoritative placement map. FDB is always rebuildable.

### No Gossip for FDB

This is a deliberate design decision. In the pre-control-plane design (ADR-001), FDB entries are distributed via gossip-like fabric announcements. With the control plane, this is replaced entirely:

- **Before**: Forge creates VM → broadcasts `VmPlacement` announcement to all peers → peers update FDB.
- **After**: Scheduler commits `PlaceVm` to Raft → all nodes apply it to redb → each Forge derives FDB from redb.

The gossip path is eliminated for FDB. This is more reliable (no missed announcements), more consistent (every node derives from the same authoritative state), and simpler (one source of truth instead of two).

## Tenant API

### Where It Runs

The tenant-facing API runs on **every node**, served by axum on the fabric IPv6 address. External access is through designated gateway nodes (see api-architecture.md) that terminate TLS and validate API keys. Internal access (server CLI, node-to-node) uses HTTP/JSON over the WireGuard fabric.

### Request Flow

```
External client (laptop CLI, Terraform, SDK)
    │
    ▼
Gateway node (TLS termination, API key validation, rate limiting)
    │
    ▼
Internal fabric (HTTP/JSON over syfrah0)
    │
    ▼
Any node receives the request
    │
    ├── Read operation → serve from local redb (eventually consistent)
    │
    └── Write operation → forward to Raft leader
         │
         ▼
    Leader validates, appends to Raft log, replicates to majority
         │
         ▼
    Committed → state machine applies to redb on all nodes
         │
         ▼
    Leader returns result → forwarded back to client
```

### Read Path

Reads are served from the local node's redb. This includes:

- `GET /v1/vms` — list VMs
- `GET /v1/vpcs` — list VPCs
- `GET /v1/hypervisors` — list hypervisors
- `GET /v1/subnets/{id}` — get subnet details

The data may be up to one Raft heartbeat behind the leader (300ms in normal operation). For API responses, this staleness is imperceptible.

### Write Path

Writes are forwarded to the Raft leader. This includes:

- `POST /v1/vms` — create VM (scheduler + IPAM + placement)
- `DELETE /v1/vms/{id}` — delete VM
- `POST /v1/vpcs` — create VPC
- `POST /v1/security-groups/{id}/rules` — add SG rule

The forwarding is transparent — the client receives the response as if the local node processed it.

### Optimistic Reads vs Linearizable Reads

By default, reads are optimistic (local state machine). This is the right choice for 99% of use cases. The client that just created a VM can see it immediately on the same node (the leader returned the result, which includes the committed state).

For the rare case where a client needs to read state that was just written by a different client on a different node, a query parameter `?consistency=strong` forces a linearizable read through the leader. This is never needed in normal operation and is provided only for debugging and testing.

## CLI Routing

### Today: CLI → Local Forge Control Socket

```
syfrah compute vm create --name web-1 ...
    │
    ▼
Unix domain socket (~/.syfrah/control.sock)
    │
    ▼
Daemon (proto-Forge) handles locally
```

### Tomorrow: CLI → Local Forge → Raft Leader (if mutation)

```
syfrah compute vm create --name web-1 ...
    │
    ▼
Unix domain socket (~/.syfrah/control.sock)
    │
    ▼
Forge receives the request
    │
    ├── Read operation → serve from local redb
    │
    └── Write operation → forward over fabric to Raft leader
         │
         ▼
    Leader processes (scheduler, IPAM, placement commit)
         │
         ▼
    Result returned to Forge → returned to CLI
```

**The CLI does not know about Raft.** It sends the same request to the same Unix socket. Forge handles routing transparently. Whether the local node is the leader or a follower is invisible to the operator.

**Latency impact:** A write on the leader node: ~1ms (local Raft commit with single-node quorum). A write forwarded to a remote leader: ~1-50ms depending on WireGuard latency between nodes. Both are well within acceptable CLI response times.

## Bootstrap to Distributed Migration

### Phase 1 (today): Local redb, Single-Node Authoritative

Forge reads and writes redb directly. redb is both the desired state store and the execution state store. There is no Raft, no log, no replication. This is bootstrap mode from ADR-003.

```
CLI → Forge → redb (local read + write)
                │
                ▼
         Reconciliation loop reads redb, acts locally
```

### Phase 2 (control plane): openraft Wraps Writes

openraft is introduced. The Raft state machine uses redb as its backend. Reads stay the same (Forge reads redb). Writes go through Raft.

```
CLI → Forge → Raft leader → log replication → commit
                                                │
                                                ▼
                             State machine applies to redb
                                                │
                                                ▼
                             Reconciliation loop reads redb, acts locally
```

### Migration Steps

These are the concrete steps from ADR-003 v4:

1. **Control plane starts.** Import the existing local redb state as the initial Raft snapshot. This is the "genesis" of the Raft log — everything that existed in local redb becomes the starting state of the Raft state machine.

2. **openraft wraps redb writes.** Forge reads redb as before. Mutations are routed through the Raft log: client → Raft leader → log replication → commit → state machine apply → redb.

3. **CLI reroutes.** The control socket handler detects that the control plane is active and forwards mutations to the Raft leader instead of writing to local redb directly.

4. **Add more nodes.** Each new node joins the Raft cluster as a learner, receives the snapshot, catches up, and is promoted to voter. From this point, all mutations are replicated.

### No Downtime During Migration

- Existing VMs continue running throughout the migration. Cloud Hypervisor processes are independent of the daemon.
- The redb tables are unchanged. The same tables, same keys, same values. The only change is how writes reach them (Raft log instead of direct write).
- There is no "migration window." Bootstrap mode works. Distributed mode works. The transition is: start the control plane, import the snapshot, and done.

## Network: openraft over WireGuard

### Raft RPC Transport

Raft messages (vote requests, append entries, install snapshot) are sent as **HTTP/JSON over the WireGuard fabric** (`syfrah0`). This is the same transport used by the Forge API — no new networking layer needed.

| Endpoint | Method | Purpose |
|---|---|---|
| `/raft/vote` | POST | Request vote during election |
| `/raft/append` | POST | Append entries (leader → follower) |
| `/raft/snapshot` | POST | Install snapshot (leader → learner) |
| `/raft/metrics` | GET | Raft metrics (leader, term, commit index) |

### Same Network as Forge API

The Raft RPC server runs alongside the Forge API on the same `syfrah0` bind address. Raft listens on a separate port:

| Service | Port | Transport |
|---|---|---|
| Forge API | 7100 | HTTP/JSON over syfrah0 |
| Raft RPC | 7200 | HTTP/JSON over syfrah0 |

Both are bound exclusively to the fabric IPv6 address. Neither is reachable from the public internet.

### WireGuard Provides Encryption

All Raft messages are encrypted by WireGuard. No additional TLS is needed for Raft RPC in Phase 1. The WireGuard mesh is the trust boundary — only nodes in the mesh can participate in Raft.

In Phase 2 (future), Raft messages may additionally be signed per-request to guard against a compromised mesh member injecting Raft messages. This is defense-in-depth, not a current requirement.

## Failure Scenarios

### Leader Failure → Automatic Re-election

**Scenario:** The Raft leader crashes or loses network connectivity.

**Detection:** Followers notice missing heartbeats after the election timeout (1-2 seconds).

**Recovery:**
1. A follower with the most up-to-date log transitions to candidate.
2. It requests votes from other nodes.
3. If it receives a majority of votes, it becomes the new leader.
4. Total failover: 1-3 seconds.

**Impact during failover:**
- Writes are blocked for 1-3 seconds (no leader to process them).
- Reads continue from local state machines (slightly stale but available).
- Existing VMs are unaffected (Cloud Hypervisor processes are independent).
- In-flight writes that were not committed are retried by the client.

### Network Partition → Minority Side Read-only

**Scenario:** A network partition splits the cluster into two groups.

**Majority side (has quorum):**
- Elects a leader (or keeps the existing one).
- Continues accepting writes.
- Gossip propagates within the majority group.
- VMs keep running. New VMs can be created.

**Minority side (no quorum):**
- Cannot elect a leader. Writes are blocked.
- Local reads continue from stale state machines.
- Gossip propagates within the minority group.
- Existing VMs keep running. No new VMs, no IP allocation, no config changes.

**Healing:**
- When connectivity is restored, Raft replays missed log entries to the minority side.
- Gossip converges within seconds.
- No split-brain: the minority side never made conflicting writes.

### Node Crash → Raft Log Replayed on Restart

**Scenario:** A node crashes (power failure, kernel panic).

**Recovery:**
1. Node restarts, daemon starts.
2. Raft loads the latest snapshot from disk.
3. Raft replays any committed log entries after the snapshot.
4. redb is restored to the last committed state.
5. Forge begins reconciliation against the restored state.
6. If the node was the leader, a new leader was already elected during downtime.

**Data durability:** Every committed log entry was fsync'd to disk before being acknowledged. No committed state is lost.

### Split Brain → Impossible with Raft

Raft's majority quorum requirement prevents split brain by construction. For a cluster of N nodes:

- Quorum = floor(N/2) + 1
- Any two quorums overlap by at least one node
- Two leaders cannot exist simultaneously (the overlapping node would have voted for only one)

| Cluster size | Quorum | Max failures | Split brain possible? |
|---|---|---|---|
| 1 | 1 | 0 | No (single node) |
| 2 | 2 | 0 | No (both needed for any write) |
| 3 | 2 | 1 | No |
| 5 | 3 | 2 | No |
| 7 | 4 | 3 | No |

### Stale Reads → Acceptable for Capacity/Health, Not for Mutations

Gossip data is inherently stale (2-10 seconds). This is acceptable because gossip is advisory:

- A stale capacity report may cause the scheduler to pick a slightly suboptimal hypervisor. Forge's admission recheck catches actual overcommit.
- A stale health report may cause the scheduler to attempt placing a VM on a failing node. The placement will fail, and the scheduler retries.

Mutations always go through Raft, which is linearizable. Stale reads of committed state (from local redb) are bounded by Raft heartbeat interval.

### Gossip Partition → Nodes May Appear Unreachable

**Scenario:** Gossip loses connectivity to some nodes, but Raft can still reach them (different network path or timing).

**Impact:** The scheduler sees some hypervisors as unreachable via gossip and excludes them from placement. The failure detector may incorrectly mark them for rescheduling.

**Mitigation:** The Raft leader's failure detector uses gossip as a signal but waits 60 seconds before committing authoritative state changes (`MarkHypervisorUnreachable`). This gives gossip time to converge. Additionally, the leader can verify node reachability via Raft heartbeats — if Raft can reach a follower but gossip cannot, the node is not dead.

## Consistency Model

| Layer | Guarantee | Mechanism | Implication |
|---|---|---|---|
| Writes | Linearizable | Raft consensus (leader + majority quorum) | Two concurrent writes are serialized. Results reflect a total order. |
| Default reads | Eventually consistent | Local redb (state machine output) | May be up to 1 Raft heartbeat behind leader. Safe for all display/list operations. |
| Linearizable reads | Linearizable | Read through leader with quorum confirmation | Reflects the latest committed state. Adds 1 round-trip. Rarely needed. |
| Gossip | Best-effort eventual | SWIM protocol, O(log N) convergence | 2-10 seconds stale. Used for hints, never for decisions that must be correct. |
| Forge reconciliation | Convergent (eventual consistency) | Periodic loop comparing desired (redb) vs actual (kernel/processes) | Forge always converges toward Raft state. Multiple runs produce the same result. |

## Quorum and Cluster Size

| Nodes | Voters | Quorum | Failures tolerated | Notes |
|---|---|---|---|---|
| 1 | 1 | 1 | 0 | Bootstrap mode. Leader of itself. Writes are instant. Zero overhead. |
| 2 | 2 | 2 | 0 | Both must agree. Fragile — either failing blocks writes. Not recommended. |
| 3 | 3 | 2 | 1 | Minimum for fault tolerance. Recommended starting point. |
| 5 | 5 | 3 | 2 | Recommended for production. |
| 7 | 5-7 | 3-4 | 2-3 | Cap voters at 5-7 for latency. Extra nodes are learners. |
| 10+ | 5-7 | 3-4 | 2-3 | Additional nodes are learners (replicate data, don't vote). |

### Single Node

With 1 node, Raft is degenerate. The node is automatically leader, writes are committed instantly (quorum of 1), and there is zero replication overhead. It behaves exactly like a local database — which is exactly what bootstrap mode is. Most operators start here. When they add nodes 2 and 3, Raft begins replicating automatically.

### Even-Number Nodes

Even-number clusters (2, 4, 6) are allowed but not recommended. The split-vote risk is higher:

- 4 nodes, quorum = 3: tolerates 1 failure (same as 3 nodes, but more hardware).
- 6 nodes, quorum = 4: tolerates 2 failures (same as 5 nodes, but more hardware).

Odd-number clusters get the same fault tolerance with one fewer node.

### Non-Voter Nodes (Learners)

For clusters with more than 7 nodes, not every node should be a Raft voter. Voters participate in leader election and quorum — more voters means higher commit latency (the leader must wait for more acknowledgments).

**Learners** replicate the Raft log and maintain a local state machine (redb) but do not vote. They have the same data as voters — they can serve reads and run Forge reconciliation — but they don't affect quorum or election timing.

A typical large deployment:

```
Voters (5-7 nodes):
    - Participate in elections and quorum
    - Process writes (when leader)
    - Full Raft participation
    - Spread across zones for fault tolerance

Learners (remaining nodes):
    - Replicate log, maintain local state machine
    - Serve reads from local redb
    - Run Forge reconciliation
    - Can be promoted to voter if a voter fails
    - No impact on write latency
```

The operator controls voter/learner designation via:
```bash
syfrah controlplane voter add <node_id>
syfrah controlplane voter remove <node_id>
syfrah controlplane learner add <node_id>
```

## Implementation with openraft

### openraft Crate Overview

`openraft` is an async Raft implementation in Rust. It provides the Raft algorithm (leader election, log replication, snapshots, membership changes) and requires the user to implement three traits for storage and networking.

**Key properties:**
- Async/tokio-native — fits naturally into the syfrah async runtime.
- Supports single-node through multi-node — no code changes needed to scale from 1 to N.
- Snapshot support for log compaction and fast node recovery.
- Joint consensus for safe membership changes.
- Used in production by Databend (cloud data warehouse).

### Three Traits to Implement

#### `RaftLogStorage` — where log entries are stored

```rust
// Our implementation: redb table for log entries
//
// Table: raft_log
// Key: u64 (log index)
// Value: LogEntry (serialized command + metadata)
//
// Operations:
// - append_to_log: insert entries at sequential indices
// - delete_conflict_logs_since: truncate log on leader change
// - purge_logs_upto: discard entries before snapshot
// - get_log_state: return last log index + term
```

Durability: every append calls fsync before returning. A crash between append and commit is safe — Raft replays on restart.

Alternative: a separate append-only file instead of a redb table. Redb is simpler (single database file) but append-only files have slightly better sequential write performance. Decision deferred to implementation — the trait abstraction allows swapping backends.

#### `RaftStateMachine` — how committed entries are applied

```rust
// Our implementation: redb tables (same tables as today)
//
// apply(entries) → for each entry:
//     match entry.command {
//         CreateVpc { .. } => insert into vpcs table,
//         AllocateIp { .. } => update ipam_bitmaps + ip_allocations,
//         PlaceVm { .. } => insert into vm_placements,
//         // ... all state machine commands
//     }
//
// snapshot() → serialize all redb tables to a byte buffer
// install_snapshot(data) → replace redb contents with snapshot data
```

The state machine is deterministic. Given the same log entries in the same order, every node produces identical redb state. This is verified by comparing state machine checksums during snapshot transfer.

#### `RaftNetwork` — how nodes communicate

```rust
// Our implementation: HTTP client over syfrah0
//
// send_vote(target, vote_request) → POST http://[target_ipv6]:7200/raft/vote
// send_append_entries(target, entries) → POST http://[target_ipv6]:7200/raft/append
// send_install_snapshot(target, snapshot) → POST http://[target_ipv6]:7200/raft/snapshot
```

The target address is the node's fabric IPv6 (from the hypervisor/node registry). The transport is HTTP/JSON over WireGuard — encrypted, authenticated by mesh membership.

### State Machine Commands

Every mutation that goes through Raft is encoded as a command in the log entry. The state machine's `apply` function pattern-matches on the command and updates the corresponding redb tables.

```rust
enum StateMachineCommand {
    // Organization hierarchy
    CreateOrg { id: OrgId, name: String },
    DeleteOrg { id: OrgId },
    CreateProject { id: ProjectId, name: String, org_id: OrgId },
    DeleteProject { id: ProjectId },
    CreateEnv { id: EnvironmentId, name: String, project_id: ProjectId, ttl: Option<Duration>, deletion_protection: bool },
    DeleteEnv { id: EnvironmentId },

    // VPC and networking
    CreateVpc { id: VpcId, name: String, cidr: Ipv4Net, vni: u32, owner: VpcOwner },
    DeleteVpc { id: VpcId },
    PeerVpc { id: PeeringId, vpc_a: VpcId, vpc_b: VpcId },
    UnpeerVpc { id: PeeringId },

    // Subnets
    CreateSubnet { id: SubnetId, name: String, vpc_id: VpcId, env_id: EnvironmentId, cidr: Ipv4Net },
    DeleteSubnet { id: SubnetId },

    // IPAM
    AllocateIp { subnet_id: SubnetId, vm_id: VmId },
    ReleaseIp { subnet_id: SubnetId, ip: Ipv4Addr },

    // Security groups
    CreateSg { id: SecurityGroupId, name: String, vpc_id: VpcId },
    DeleteSg { id: SecurityGroupId },
    AddSgRule { id: RuleId, sg_id: SecurityGroupId, rule: SecurityGroupRule },
    RemoveSgRule { id: RuleId },
    AttachSg { nic_id: NicId, sg_id: SecurityGroupId },
    DetachSg { nic_id: NicId, sg_id: SecurityGroupId },

    // Route tables
    CreateRouteTable { id: RouteTableId, vpc_id: VpcId },
    DeleteRouteTable { id: RouteTableId },
    CreateRoute { id: RouteId, table_id: RouteTableId, destination: Ipv4Net, target: RouteTarget },
    DeleteRoute { id: RouteId },

    // NAT gateways
    CreateNatGw { id: NatGatewayId, name: String, vpc_id: VpcId, subnet_id: SubnetId },
    DeleteNatGw { id: NatGatewayId },

    // VM lifecycle
    CreateVm { id: VmId, spec: VmSpec },
    PlaceVm { vm_id: VmId, hypervisor_id: HypervisorId },
    StopVm { vm_id: VmId },
    StartVm { vm_id: VmId },
    DeleteVm { vm_id: VmId },
    RemoveVm { vm_id: VmId },
    RescheduleVm { vm_id: VmId, reason: RescheduleReason },

    // Volume lifecycle
    CreateVolume { id: VolumeId, spec: VolumeSpec },
    AttachVolume { volume_id: VolumeId, vm_id: VmId },
    DetachVolume { volume_id: VolumeId },
    DeleteVolume { id: VolumeId },

    // Hypervisor lifecycle
    RegisterHypervisor { id: HypervisorId, spec: HypervisorSpec },
    EnableHypervisor { id: HypervisorId },
    DrainHypervisor { id: HypervisorId },
    DecommissionHypervisor { id: HypervisorId },
    DisableHypervisor { id: HypervisorId },
    UpdateHypervisorLabels { id: HypervisorId, labels: HashMap<String, String> },
    UpdateHypervisorTaints { id: HypervisorId, taints: Vec<Taint> },
    MarkHypervisorUnreachable { id: HypervisorId },
    MarkHypervisorAvailable { id: HypervisorId },

    // Network interfaces
    CreateNic { id: NicId, vm_id: VmId, subnet_id: SubnetId },
    DeleteNic { id: NicId },

    // Cluster membership
    AddLearner { node_id: NodeId },
    AddVoter { node_id: NodeId },
    RemoveNode { node_id: NodeId },
}
```

Every command must be **idempotent**. Applying the same command twice (e.g., due to a Raft replay after crash) must produce the same state. The state machine checks preconditions before applying:

- `CreateVpc` with an ID that already exists → no-op (return existing VPC).
- `DeleteVm` for a VM that is already `Deleted` → no-op.
- `AllocateIp` for a VM that already has an IP in this subnet → return existing allocation.

### Snapshot Strategy

- **Trigger**: every 10,000 committed log entries OR on demand (new node joining).
- **Format**: serialized redb database (all tables exported to a structured binary format).
- **Size**: proportional to cluster state. 1,000 VMs + 100 VPCs + associated resources ≈ 1-5 MB.
- **Transfer**: HTTP POST to `/raft/snapshot` on the receiving node. Streamed for large snapshots.
- **Application**: receiving node deserializes the snapshot into its local redb, replacing all tables. Then replays any log entries after the snapshot index.
- **Retention**: keep the latest 2 snapshots. Older snapshots are deleted.

## Observability

### Raft Metrics

Exposed at `GET /raft/metrics` on port 7200 and via Prometheus at `/metrics` on the internal HTTP port.

| Metric | Description |
|---|---|
| `syfrah_raft_state` | Current Raft state: leader, follower, candidate, learner |
| `syfrah_raft_current_term` | Current Raft term (monotonically increasing) |
| `syfrah_raft_commit_index` | Index of the last committed log entry |
| `syfrah_raft_last_applied` | Index of the last applied log entry |
| `syfrah_raft_log_entries` | Number of entries in the Raft log (between last snapshot and latest) |
| `syfrah_raft_snapshot_index` | Index of the last snapshot |
| `syfrah_raft_leader_id` | Node ID of the current leader (empty if unknown) |
| `syfrah_raft_proposals_committed_total` | Counter: total committed proposals |
| `syfrah_raft_proposals_failed_total` | Counter: total failed proposals (validation, timeout) |
| `syfrah_raft_append_entries_latency_ms` | Histogram: time to replicate entries to followers |
| `syfrah_raft_apply_latency_ms` | Histogram: time to apply committed entries to state machine |

### Gossip Metrics

| Metric | Description |
|---|---|
| `syfrah_gossip_members_total` | Number of known members in the gossip pool |
| `syfrah_gossip_members_alive` | Members in Alive state |
| `syfrah_gossip_members_suspect` | Members in Suspect state |
| `syfrah_gossip_members_dead` | Members in Dead state |
| `syfrah_gossip_messages_sent_total` | Counter: total protocol messages sent |
| `syfrah_gossip_messages_received_total` | Counter: total protocol messages received |
| `syfrah_gossip_updates_piggybacked_total` | Counter: state updates piggybacked on protocol messages |

### Scheduler Metrics

| Metric | Description |
|---|---|
| `syfrah_scheduler_placements_total` | Counter: total placement decisions |
| `syfrah_scheduler_placements_retried_total` | Counter: placements retried (Forge rejection) |
| `syfrah_scheduler_placements_failed_total` | Counter: placements that exhausted retries |
| `syfrah_scheduler_placement_latency_ms` | Histogram: time from request to Raft commit |
| `syfrah_scheduler_candidates_evaluated` | Histogram: number of candidates evaluated per placement |
| `syfrah_scheduler_no_candidates_total` | Counter: placements with zero candidates after filtering |

## Security

### Raft RPC: WireGuard-Only (Phase 1)

Raft RPC (port 7200) is bound to the fabric IPv6 address (`syfrah0`). Only nodes in the WireGuard mesh can reach it. This provides:

- **Encryption**: WireGuard encrypts all traffic (ChaCha20-Poly1305).
- **Authentication**: only nodes with the mesh secret can join the WireGuard mesh and participate in Raft.
- **No external attack surface**: port 7200 is not reachable from the public internet.

### Signed Requests (Phase 2, future)

In Phase 2, Raft messages will additionally carry a signature (Ed25519, derived from the node's identity key). This guards against a compromised mesh member injecting forged Raft messages. The Raft RPC server validates the signature before processing any message.

This is defense-in-depth. In Phase 1, WireGuard mesh membership is sufficient — a node in the mesh is trusted by definition (the operator approved its join).

### Only Mesh Members Can Participate in Raft

The Raft cluster membership is a subset of the WireGuard mesh membership. A node must first join the mesh (fabric layer, with operator approval) before it can be added to the Raft cluster. There is no way to participate in Raft without being in the mesh.

### Leader Cannot Be Forced Externally

The Raft leader is elected by majority vote among cluster members. There is no API to "set the leader" or "force election." An external attacker who compromises the API gateway cannot influence leader election — it is an internal Raft protocol operation that happens only between Raft members over the fabric.

An operator can influence elections indirectly by removing a voter (which may trigger a new election) or by draining a node (which does not affect Raft voting — drain is a compute-layer concept, not a consensus concept).

## Implementation Phases

### Phase 1: Raft Scaffold + State Machine + Leader Election

**Goal:** openraft running on every node. Leader election works. The state machine applies commands to redb. Single-node clusters work identically to bootstrap mode.

**Deliverables:**
- Implement `RaftLogStorage` (redb table for log entries)
- Implement `RaftStateMachine` (apply commands to redb tables)
- Implement `RaftNetwork` (HTTP/JSON over syfrah0, port 7200)
- Basic state machine commands: `CreateOrg`, `CreateProject`, `CreateEnv`, `CreateVpc`, `CreateSubnet`
- Leader election and automatic failover
- Raft metrics endpoint
- CLI: `syfrah controlplane status` (show leader, term, commit index)
- CLI: `syfrah controlplane voter add/remove`, `syfrah controlplane learner add`

**Estimated: 8-10 issues**

### Phase 2: Distributed IPAM + VM Placement through Raft

**Goal:** IP allocation and VM placement go through Raft. No more per-node IPAM. The scheduler exists (basic version).

**Deliverables:**
- `AllocateIp` / `ReleaseIp` commands through Raft state machine
- `CreateVm` / `PlaceVm` / `DeleteVm` commands through Raft
- Basic scheduler: filter by zone + capacity, score by utilization
- Forge reads VM placements from redb (Raft state machine output)
- Migration: import existing local redb state as initial Raft snapshot
- Write forwarding from non-leader nodes to leader

**Estimated: 8-10 issues**

### Phase 3: FDB Distribution from Raft State

**Goal:** FDB entries are derived from Raft state (vm_placements + hypervisors), not from fabric announcements.

**Deliverables:**
- Forge computes FDB from `vm_placements` + `hypervisors` tables
- On Raft commit of `PlaceVm`: Forge reconciliation adds FDB entries
- On Raft commit of `DeleteVm`: Forge reconciliation removes FDB entries
- Full FDB rebuild on Forge restart from local redb
- Deprecate fabric peer announcement-based FDB distribution
- ARP proxy entries derived from Raft state

**Estimated: 5-6 issues**

### Phase 4: Scheduler (gossip-based scoring + Raft placement commit)

**Goal:** Full scheduler with all placement constraints from ADR-004.

**Deliverables:**
- SWIM gossip integration (foca crate)
- `HypervisorReport` gossip dissemination
- Scheduler: full filter-then-score pipeline (zone, labels, taints, anti-affinity, spread, capacity, utilization)
- Forge admission recheck + rejection + retry
- Gossip metrics
- Scheduler metrics

**Estimated: 8-10 issues**

### Phase 5: Tenant API Routing (any-node access)

**Goal:** API requests can be sent to any node. Writes are transparently forwarded to the Raft leader.

**Deliverables:**
- Write forwarding middleware in axum
- Read path: serve from local redb
- CLI routing: control socket handler detects active control plane, forwards mutations
- `?consistency=strong` query parameter for linearizable reads
- Error handling: leader unknown, leader changed during request, timeout
- Gateway integration: external API → gateway → any node → leader

**Estimated: 5-6 issues**

### Phase 6: Non-Voter Nodes for Large Clusters

**Goal:** Support clusters >7 nodes efficiently by limiting the number of Raft voters.

**Deliverables:**
- Automatic voter/learner management (cap voters at configured max, default 7)
- Learner promotion on voter failure
- CLI: `syfrah controlplane members` (show voters, learners, status)
- Learner-to-voter promotion logic (prefer nodes in underrepresented zones)
- Documentation: operational guide for large clusters

**Estimated: 3-4 issues**

### Estimated Scope

~37-46 issues across 6 phases. Each phase delivers independently useful functionality. Phase 1 alone makes single-node deployments use the same code path as multi-node deployments, validating the architecture without requiring multiple nodes.

## Commercial Value

This ADR delivers the core differentiator of Syfrah: turning independent hypervisors into a unified cloud.

- **Any-node access.** Operators interact with the cluster, not individual machines. `syfrah compute vm create --zone eu-west-1` works from any node. No SSH-per-server workflows.
- **Global IPAM.** IP addresses are unique across the entire cluster, guaranteed by consensus. Two VMs in the same subnet on different hypervisors never collide.
- **Automated placement.** The scheduler picks the best hypervisor based on capacity, topology, constraints, and health — like AWS EC2 or GCP Compute Engine.
- **Zero-config HA.** Add a third node and the cluster is fault-tolerant. No manual leader designation, no external coordination service, no operational burden.
- **Provider-agnostic.** The control plane runs over WireGuard. Mix OVH, Hetzner, and Scaleway servers in the same Raft cluster. Cross-provider VM placement works identically.
- **No external dependencies.** No etcd to deploy, no Consul to monitor, no ZooKeeper to tune. The control plane is the syfrah binary.
- **Incremental migration.** Start with one server. Everything works. Add more servers when you need them. The control plane activates automatically.
- **Derived networking.** FDB entries, ARP proxy, nftables rules — all derived from the control plane's authoritative state. No separate distribution protocols, no convergence delays, no missed updates.

## Rejected Alternatives

### 1. External etcd cluster

**Considered:** Use etcd as the consensus layer, similar to Kubernetes.

**Rejected:** etcd is an external dependency that requires separate deployment, monitoring, and maintenance. It assumes low-latency networking (not guaranteed across WAN WireGuard links). It adds operational complexity (etcd upgrades, backup, restore). Syfrah's design principle is "no external dependencies." openraft provides the same consistency guarantees as an embedded library.

### 2. External Consul

**Considered:** Use Consul for both consensus and service discovery.

**Rejected:** Same dependency concerns as etcd. Additionally, Consul's KV store has a 512KB value limit and is not designed for the volume of state Syfrah manages (IPAM bitmaps, security group rules, route tables). Consul's gossip (Serf) would overlap with our SWIM implementation, adding confusion about which gossip to use for what.

### 3. CRDTs for distributed state

**Considered:** Use Conflict-free Replicated Data Types instead of Raft for coordination.

**Rejected:** CRDTs cannot enforce uniqueness constraints. You cannot build IPAM (globally unique IP allocation) or VNI allocation with CRDTs — they are designed for conflict-free convergence, not conflict prevention. The control plane's primary job is preventing conflicts. Raft's sequential log is the right primitive.

### 4. Gossip for everything (no Raft)

**Considered:** Use gossip for all state, including mutations.

**Rejected:** Gossip is eventually consistent. "Eventually" is not acceptable for IP allocation, VM placement, or resource creation. Two nodes gossiping "I allocated 10.0.1.5" leads to conflicts that no gossip protocol can resolve. Gossip is perfect for health and capacity — it is catastrophically wrong for mutations.

### 5. Raft for everything (no gossip)

**Considered:** Put health and capacity data into Raft.

**Rejected:** Health and capacity change every few seconds on every node. Replicating this through Raft would create enormous log churn — thousands of entries per minute, all requiring majority quorum. This would dominate the Raft log, increase commit latency for real mutations, and waste disk space. Gossip handles high-frequency, low-criticality data efficiently. Raft handles low-frequency, high-criticality data safely.

### 6. Dedicated control plane nodes

**Considered:** Run Raft only on dedicated controller nodes, not on hypervisors.

**Rejected:** This creates a separate failure domain and operational burden. Operators must provision and manage "controller" machines that don't run workloads. It violates the "every node is equal" principle. openraft supports embedded operation with minimal overhead — there is no reason to dedicate hardware to it. For large clusters, the voter/learner distinction provides the same scaling benefit without dedicated machines.

### 7. redb as distributed store

**Considered:** Use redb directly for distributed state coordination across nodes.

**Rejected:** redb is a single-process embedded key-value store with exclusive file locks. It has no replication, no consensus, no multi-writer support. It is excellent as a local state machine backend for Raft (fast reads, ACID transactions, zero-config), but cannot be the distributed coordination layer itself. openraft provides the consensus algorithm; redb provides the local applied-state storage. These are complementary, not interchangeable.

### 8. Separate desired-state store per node

**Considered:** Each node keeps its own desired state in local redb, synced via some protocol.

**Rejected:** This creates a split-brain problem. If a node goes down, its desired state is lost. The authoritative store (openraft-based, with redb as state machine backend) provides the single desired state that survives node failures. Forge is stateless in intent by design — it reads desired state, never owns it.

## References

- `handbook/ARCHITECTURE.md` — global architecture, stack diagram, failure model
- `handbook/adr-001-networking-roadmap.md` — VXLAN, FDB distribution, IPAM bitmap design
- `handbook/adr-002-security-groups-route-tables.md` — security groups, route tables, NAT gateways, NICs
- `handbook/adr-003-forge.md` — Forge design, desired state projection, openraft integration, bootstrap mode
- `handbook/adr-004-hypervisor-model.md` — hypervisor topology, scheduler integration, HypervisorReport, placement algorithm
- `handbook/state-and-reconciliation.md` — source of truth tables, reconciliation philosophy, phase models
- `handbook/api-architecture.md` — API transport, gateway pattern, CLI routing
- `handbook/zones-and-regions.md` — topology metadata, placement semantics
- `layers/fabric/README.md` — WireGuard mesh, peer announcements, gossip dissemination
- `layers/controlplane/README.md` — control plane overview, two-layer architecture, Raft scaling
- `openraft` crate: https://docs.rs/openraft
- `foca` crate: https://docs.rs/foca
- `redb` crate: https://docs.rs/redb
