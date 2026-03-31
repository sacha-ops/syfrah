# ADR-003: Forge — Per-Node Resource Orchestrator

**Status**: Proposed
**Date**: 2026-03-30
**Decided by**: Sacha + team, after architecture review

## Context

The data plane layers are designed: fabric provides encrypted node-to-node connectivity (implemented), compute manages Cloud Hypervisor VM lifecycles (designed, ADR-001 integrated networking), overlay provides VXLAN-based VPC isolation with security groups and route tables (designed, ADR-002 delivered network policy), org models the tenant hierarchy, and storage will back volumes with S3 via ZeroFS.

What is missing is the **per-node orchestrator** — the component that ties all local resources together, exposes a unified API, and continuously reconciles local reality against the cluster's desired state. Today, the fabric daemon (`layers/fabric/src/daemon.rs`) manages WireGuard peering and a control socket. Compute, overlay, and storage operations are invoked directly through library calls and CLI commands. There is no unified resource lifecycle, no reconciliation engine, no capacity management, and no standard API for the control plane to drive node-level operations.

Every cloud provider has this component. AWS calls it the Nitro host agent. GCP calls it Borglet. Azure has its Host Agent. It is the bridge between the distributed brain (control plane) and the physical machine. Without it, there is no programmable cloud — just a collection of scripts.

Forge is Syfrah's answer. Every node runs a Forge instance. Forge is to a node what EC2's host agent is to a hypervisor in an availability zone: the single entry point for all resource mutations, the reconciliation engine that ensures local reality matches desired state, and the capacity reporter that feeds the scheduler.

This ADR defines Forge completely: its resource model, state machine, API, reconciliation engine, capacity management, health monitoring, security posture, observability, upgrade strategy, and integration with every existing layer. It is designed to be implemented incrementally — each phase delivers working functionality — while the full vision is cloud-provider-grade.

### Why now

The layers below Forge are designed. The layers above (control plane, tenant API) depend on Forge's API contract. Forge is the integration point — defining it now unblocks both directions: implementors can start building Forge against the compute/overlay/storage designs, and control plane designers can target Forge's API for scheduling and reconciliation.

### Relationship to existing decisions

This ADR is consistent with:
- **ARCHITECTURE.md** — Forge sits between fabric and compute/overlay/storage in the stack diagram
- **ADR-001** — networking primitives (VXLAN, bridges, TAPs, FDB, IPAM, config-drive) that Forge orchestrates
- **ADR-002** — security groups, route tables, NAT gateways, NICs as first-class resources that Forge enforces
- **state-and-reconciliation.md** — the reconciliation philosophy (Raft = desired, gossip = observed, Forge reconciles)
- **api-architecture.md** — transport decisions (HTTP/JSON on fabric, Unix socket for local CLI, gRPC for external)

## What is Forge

Forge is the per-node local resource orchestrator. It runs on every node as part of the syfrah daemon process. It exposes an HTTP/JSON REST API bound exclusively to the fabric interface (`syfrah0`), making it reachable only from within the WireGuard mesh — never from the public internet.

Forge owns the full lifecycle of every resource on its node:
- **Compute**: Cloud Hypervisor VM instances (create, start, stop, resize, delete, reconnect)
- **Network**: Linux bridges, VXLAN interfaces, TAP/veth devices, nftables rules (security groups, anti-spoofing, NAT), FDB entries, ARP proxy entries, route table enforcement
- **Storage**: ZeroFS volumes, NBD connections, snapshots (future)
- **Security**: security group rule enforcement, anti-spoofing, infrastructure protection rules

Forge does not make scheduling decisions, does not know about other nodes' workloads, and does not own desired state. It receives desired state from the control plane (via Raft replication to the local redb store), observes actual state from the kernel and running processes, computes the diff, and acts.

```
    ┌─────────────────────────────────────────────────────────────┐
    │  Control Plane (Raft leader on any node)                     │
    │  "VM web-1 should run on Node B with 2 vCPU, 4GB,          │
    │   in VPC prod, subnet frontend, SG web-sg"                  │
    └────────────────────────┬────────────────────────────────────┘
                             │
                             │  Raft log replicated to all nodes
                             │  (each node has local redb replica)
                             ▼
    ┌─────────────────────────────────────────────────────────────┐
    │  Forge (Node B)                                              │
    │                                                              │
    │  REST API: http://[fd12:...:nodeB]:7100/v1/...              │
    │                                                              │
    │  ┌──────────────────────────────────────────────────────┐   │
    │  │  Reconciliation Engine                                │   │
    │  │  desired state (redb) ↔ actual state (kernel/procs)  │   │
    │  │  → compute diff → apply changes → report via gossip   │   │
    │  └──────────────────────────────────────────────────────┘   │
    │                                                              │
    │  ┌─────────────┐ ┌──────────────┐ ┌──────────────────────┐ │
    │  │  Compute    │ │  Overlay     │ │  Storage             │ │
    │  │  VmManager  │ │  NetworkMgr  │ │  VolumeMgr           │ │
    │  │  (CH REST)  │ │  (ip, nft)   │ │  (ZeroFS/NBD)        │ │
    │  └─────────────┘ └──────────────┘ └──────────────────────┘ │
    │                                                              │
    │  syfrah0 (fabric — WireGuard mesh)                          │
    └─────────────────────────────────────────────────────────────┘
```

## Design Principles

1. **Forge manages ONLY local resources.** It creates, modifies, and destroys resources on this node. It never reaches across the fabric to mutate resources on another node. Cross-node coordination is the control plane's job.

2. **Forge is stateless in intent.** Desired state comes from the control plane via Raft, replicated to the local redb store. Forge reads desired state, never writes it. Forge writes only observed state (via gossip) and derived state (kernel interfaces, nftables rules, FDB entries).

3. **Forge exposes a REST API on the fabric interface only.** Bound to the node's `syfrah0` IPv6 address on port 7100 (configurable). HTTP, not HTTPS — WireGuard provides encryption. Consistent with the Forge README's transport decision and api-architecture.md's internal transport.

4. **Forge is the ONLY entry point for resource mutations on a node.** No direct CLI-to-compute or CLI-to-overlay calls for mutation operations. The flow is: CLI → control socket → Forge (local), or CLI → control plane → Forge (remote). Read-only inspection (e.g., `syfrah compute status`) may bypass Forge for debugging.

5. **Every operation is idempotent.** Creating a bridge that already exists is a no-op. Starting a VM that is already running returns success. Applying security group rules that already match is a no-op. The reconciliation loop can run any number of times and produce the same result.

6. **VMs are independent of Forge's process.** Cloud Hypervisor runs as a separate OS process per VM. Forge restart does not affect running VMs. This is the foundation of zero-downtime platform upgrades.

7. **Forge subsumes the current daemon.** The current fabric daemon that runs the mesh, peering, control socket, and compute operations is the proto-Forge. Forge replaces and extends it — not as a separate process, but as the evolution of the daemon.

## Resource Model

### Resource identity

Every resource Forge manages has a globally unique identifier. IDs are assigned by the control plane (via Raft) before the resource reaches Forge. Forge never generates resource IDs — it receives them.

ID format: `{type}-{ulid}` where ULID provides time-ordered, globally unique, URL-safe identifiers.

Examples:
- `vm-01HXYZ...` — a VM instance
- `nic-01HXYZ...` — a network interface
- `br-01HXYZ...` — a Linux bridge
- `vol-01HXYZ...` — a storage volume
- `sg-01HXYZ...` — a security group
- `nat-01HXYZ...` — a NAT gateway
- `rt-01HXYZ...` — a route table

### Resource metadata

Every resource carries:

| Field | Type | Description |
|---|---|---|
| `id` | String | Globally unique identifier (assigned by control plane) |
| `state` | ResourceState | Current lifecycle state (see state machine below) |
| `desired_state` | DesiredState | What the control plane wants (from Raft) |
| `owner` | ResourceOwner | `{ org_id, project_id, env_id }` — the tenant hierarchy |
| `node_id` | NodeId | Which node this resource is on |
| `generation` | u64 | Version counter, incremented on every mutation. For optimistic concurrency. |
| `created_at` | u64 | Unix timestamp of creation in Raft |
| `updated_at` | u64 | Unix timestamp of last state change |
| `last_reconciled_at` | u64 | Unix timestamp of last successful reconciliation |
| `labels` | HashMap<String, String> | User-defined metadata (inherited from environment) |

### Resource types

#### Compute resources

**Instance (VM)**

```
Instance {
    id: VmId,
    name: String,
    spec: VmSpec {
        vcpus: u32,
        memory_mb: u32,
        image: String,
        kernel: Option<String>,
        gpu: GpuMode,
    },
    nics: Vec<NicId>,
    volumes: Vec<VolumeAttachment>,
    node_id: NodeId,
    runtime: Option<VmRuntime {
        pid: u32,
        socket_path: String,
        ch_version: String,
        uptime_seconds: u64,
    }>,
    // ... common metadata fields
}
```

Forge delegates to the compute layer's `VmManager` for Cloud Hypervisor process management. The compute layer handles spawn, monitor, reconnect, and kill chain. Forge orchestrates the full lifecycle: network setup → volume attach → compute spawn → SG apply → FDB announce.

#### Network resources

**Bridge** — one per VPC per node, created on-demand when the first VM in a VPC lands on this node.

```
Bridge {
    id: BridgeId,
    name: String,                    // syfbr-{vpc_id_short}
    vpc_id: VpcId,
    vxlan_interface: String,         // syfvx-{vpc_id_short}
    vni: u32,
    gateway_ips: Vec<(Ipv4Addr, u8)>, // subnet gateways on this bridge
    attached_taps: Vec<String>,
    // ... common metadata fields
}
```

**NetworkInterface (NIC)** — first-class resource per ADR-002. Attachment point for security groups.

```
NetworkInterface {
    id: NicId,
    name: String,                    // syftap-{hash} or syfve-{hash}
    vm_id: Option<VmId>,
    subnet_id: SubnetId,
    vpc_id: VpcId,
    private_ip: Ipv4Addr,
    mac: MacAddr,                    // derived: 02:00:{ip_hex}
    security_groups: Vec<SecurityGroupId>,
    tap_name: String,
    // ... common metadata fields
}
```

**FDB entry** — derived state, not a first-class resource. Forge creates FDB entries from VM placement data and repopulates them on restart.

**nftables rules** — derived state. Generated from security group rules, anti-spoofing config, NAT gateway config, and route table config. Recomputed atomically on any change.

#### Storage resources (future)

**Volume**

```
Volume {
    id: VolumeId,
    name: String,
    size_gb: u32,
    attached_to: Option<VmId>,
    nbd_device: Option<String>,
    s3_key: String,
    // ... common metadata fields
}
```

#### Security resources

**SecurityGroup** — definition and rules stored in redb (from Raft). Forge enforces them as nftables rules on local NICs.

**NatGateway** — per ADR-002. Forge configures nftables masquerade chains for NAT gateways on this node.

**RouteTable** — per ADR-002. Forge programs Linux routing rules derived from route table entries.

## State Machine

Every resource follows a strict lifecycle with explicit, auditable transitions.

### Primary states

```
enum ResourceState {
    Pending,        // resource defined in Raft, not yet acted on by Forge
    Creating,       // Forge is actively provisioning (spawning process, creating interface)
    Active,         // resource is operational and reconciled
    Updating,       // Forge is applying a change (resize, SG update, route change)
    Stopping,       // VM being gracefully stopped
    Stopped,        // VM stopped, resources still allocated
    Deleting,       // Forge is tearing down the resource
    Deleted,        // resource fully cleaned up, record retained for audit
    Failed,         // unrecoverable error, requires operator attention or control plane action
}
```

### Transition diagram

```
Pending ──→ Creating ──→ Active
                │            │
                ▼            ├──→ Updating ──→ Active
              Failed         │
                │            ├──→ Stopping ──→ Stopped ──→ Starting ──→ Active
                │            │                    │
                │            │                    ├──→ Deleting ──→ Deleted
                │            │                    │
                │            │                    └──→ Failed
                │            │
                │            ├──→ Deleting ──→ Deleted
                │            │
                │            └──→ Failed
                │
                └──→ Deleting ──→ Deleted
```

### Transition rules

| From | To | Trigger | Who |
|---|---|---|---|
| Pending | Creating | Forge begins provisioning | Forge (reconciliation loop) |
| Creating | Active | All provisioning steps succeeded | Forge |
| Creating | Failed | Provisioning step failed after retries | Forge |
| Active | Updating | Spec change detected (resize, SG update) | Forge (reconciliation loop) |
| Updating | Active | Update applied successfully | Forge |
| Updating | Failed | Update failed after retries | Forge |
| Active | Stopping | Stop requested in Raft desired state | Forge (reconciliation loop) |
| Stopping | Stopped | VM process exited cleanly | Forge |
| Stopping | Failed | Kill chain exhausted, process still alive | Forge |
| Stopped | Creating | Restart requested in Raft desired state | Forge (reconciliation loop) |
| Stopped | Deleting | Delete requested in Raft desired state | Forge (reconciliation loop) |
| Active | Deleting | Delete requested in Raft desired state | Forge (reconciliation loop) |
| Active | Failed | Runtime failure (process crash, interface disappeared) | Forge (monitor) |
| Failed | Deleting | Delete requested in Raft desired state | Forge (reconciliation loop) |
| Deleting | Deleted | All cleanup completed | Forge |
| Deleting | Failed | Cleanup failed (e.g., resource stuck) | Forge |

**Any transition not in this table is an error.** The state machine is enforced in code. Invalid transitions are logged and rejected.

### Desired state vs observed state

The control plane writes **desired state** to Raft:
- "VM web-1 should be Running with 2 vCPUs, 4GB, on Node B, in VPC prod"
- "Security group web-sg should have rules [TCP 80, TCP 443 from 0.0.0.0/0]"
- "NAT Gateway nat-1 should exist in VPC prod, subnet frontend"

Forge observes **actual state** from the kernel and running processes:
- "Cloud Hypervisor process for vm-01HX is alive, PID 12345"
- "Bridge syfbr-100 exists with TAPs syftap-abc, syftap-def"
- "nftables chain vm_abc_in has rules [TCP 22, ICMP from 0.0.0.0/0]"

The reconciliation engine computes the diff and drives actual state toward desired state.

## API Design

### Transport

- **Protocol**: HTTP/1.1 + JSON
- **Bind address**: Node's fabric IPv6 address (`syfrah0`) — never `0.0.0.0` or `::`
- **Port**: 7100 (configurable via `[forge] port` in `config.toml`)
- **Encryption**: WireGuard provides encryption at the fabric layer. No TLS on top. This matches the Forge README design and is consistent with Kubernetes kubelet, AWS Nitro host agent, and other cloud provider host agents.
- **Reachability**: Only from within the mesh. A port scan from the internet will never find Forge. The binding itself is the access control.

### Authentication and authorization

**Phase 1 (pre-control plane)**:
- Any mesh node can call any other node's Forge API. Acceptable for a single-operator mesh.
- The fabric's WireGuard authentication (only nodes with the mesh secret can reach `syfrah0`) is the access control.

**Phase 2 (with control plane)**:
- Bearer token derived from mesh secret. The control plane leader includes the token when sending commands.
- Role separation: only the Raft leader (or nodes acting on its behalf) can call mutation endpoints. Other nodes can only call read-only endpoints (`/v1/node/*`, `GET` on any resource).
- All mutation requests carry a `raft_term` and `raft_index` to prevent stale commands from a deposed leader.

### Versioning

URL-based: `/v1/...`. All endpoints within a major version are backward-compatible. Breaking changes require `/v2/...`, served alongside `/v1/` for one release cycle.

### Request conventions

Every request may include:

| Header / Field | Purpose |
|---|---|
| `X-Request-Id` | Client-generated unique ID for tracing. If absent, Forge generates one. |
| `X-Idempotency-Key` | For create operations. Same key returns existing resource instead of creating a duplicate. |
| `X-Raft-Term` | Raft leader term (phase 2). Stale terms are rejected. |
| `X-Raft-Index` | Raft log index that authorized this operation (phase 2). |

### Response conventions

Every response includes:

```json
{
  "request_id": "req-a7f3e29b1c04",
  "resource": { ... },
  "generation": 42,
  "timestamp": 1711555200
}
```

Error responses:

```json
{
  "request_id": "req-a7f3e29b1c04",
  "error": {
    "code": "FORGE_INSUFFICIENT_RESOURCES",
    "message": "Cannot create VM: node has 1 vCPU available, 2 requested",
    "details": {
      "requested_vcpus": 2,
      "available_vcpus": 1
    },
    "retry_after": null
  }
}
```

Error code prefix: `FORGE_` for all Forge-level errors. Consistent with api-architecture.md's `{LAYER}_` error code convention.

### Endpoints

#### Instance (Compute)

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/instances` | Create a VM instance |
| `GET` | `/v1/instances` | List all VM instances on this node |
| `GET` | `/v1/instances/:id` | Get instance details (spec, state, runtime, NICs, volumes) |
| `DELETE` | `/v1/instances/:id` | Delete instance (triggers full cleanup) |
| `POST` | `/v1/instances/:id/start` | Start a stopped instance |
| `POST` | `/v1/instances/:id/stop` | Stop a running instance (graceful shutdown) |
| `POST` | `/v1/instances/:id/reboot` | Reboot instance |
| `PATCH` | `/v1/instances/:id/resize` | Resize CPU/memory (hot-plug if supported, else stop+start) |

**Create instance** orchestration (the full flow):

```
POST /v1/instances
{
  "id": "vm-01HXYZ...",
  "name": "web-1",
  "spec": { "vcpus": 2, "memory_mb": 4096, "image": "ubuntu-24.04" },
  "network": {
    "nic_id": "nic-01HXYZ...",
    "vpc_id": "vpc-prod",
    "subnet_id": "subnet-frontend",
    "ipv4": "10.0.1.5",
    "mac": "02:00:0a:00:01:05",
    "security_groups": ["sg-web"]
  },
  "volumes": [],
  "owner": { "org_id": "org-acme", "project_id": "proj-backend", "env_id": "env-prod" }
}
```

Forge executes the following steps (any failure triggers rollback of completed steps):

1. **Admission**: check node capacity (vCPUs, memory). If insufficient, reject with `409 Conflict`.
2. **Reserve resources**: mark vCPUs and memory as reserved in local tracker. Reservation expires after 60s.
3. **Network setup**: ensure VPC bridge + VXLAN exist (create if first VM in VPC on this node). Create TAP device. Attach TAP to bridge. Add subnet gateway IP to bridge if needed.
4. **Security**: apply nftables rules — anti-spoofing (source MAC/IP = assigned values), ingress rules from security groups, egress rules, conntrack. Use the per-VM chain architecture from ADR-002.
5. **FDB + ARP**: add local FDB entry. Add ARP proxy entry. Store `VmPlacement` record. Announce to fabric peers (gossip-based distribution).
6. **Config-drive**: generate cloud-init ISO with IP, gateway, DNS, MTU=1350, SSH keys.
7. **Storage**: if volumes are requested, connect ZeroFS NBD and attach to VM config.
8. **Compute**: spawn Cloud Hypervisor process, create VM via CH REST API, boot.
9. **Confirm**: verify VM is running (ping CH API). Release resource reservation (now counted as used). Transition state to Active. Gossip status.

**Delete instance** orchestration:

1. Transition state to `Deleting`.
2. Stop VM (graceful shutdown chain: ACPI → power button → SIGTERM → SIGKILL).
3. Announce `VmPlacement{Remove}` to all fabric peers.
4. Remove FDB + ARP proxy entries.
5. Remove nftables chains for this VM.
6. Delete TAP device.
7. Remove subnet gateway IP from bridge (if no other VMs on this subnet on this node).
8. Delete bridge + VXLAN (if no other VMs in this VPC on this node).
9. Release IPAM allocation.
10. Detach and disconnect volumes.
11. Clean up compute runtime directory (`/run/syfrah/vms/{id}/`).
12. Transition state to `Deleted`.

#### Network

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/networks/bridges` | Ensure bridge + VXLAN for a VPC on this node |
| `DELETE` | `/v1/networks/bridges/:id` | Remove bridge + VXLAN (only if no attached TAPs) |
| `GET` | `/v1/networks/bridges` | List bridges on this node |
| `GET` | `/v1/networks/bridges/:id` | Bridge details (attached TAPs, gateway IPs, VNI) |
| `POST` | `/v1/networks/interfaces` | Create NIC (TAP/veth), attach to bridge |
| `DELETE` | `/v1/networks/interfaces/:id` | Remove NIC, flush nftables rules |
| `GET` | `/v1/networks/interfaces` | List NICs on this node |
| `GET` | `/v1/networks/interfaces/:id` | NIC details (IP, MAC, SGs, state) |
| `POST` | `/v1/networks/sg/apply` | Apply security group rules for a NIC |
| `POST` | `/v1/networks/sg/check` | Verify nftables rules match expected SG rules |
| `POST` | `/v1/networks/nat-gw` | Ensure NAT gateway masquerade chain |
| `DELETE` | `/v1/networks/nat-gw/:id` | Remove NAT gateway masquerade chain |
| `POST` | `/v1/networks/routes/apply` | Apply route table rules for a subnet |
| `GET` | `/v1/networks/fdb` | List FDB entries on this node |
| `POST` | `/v1/networks/fdb` | Add or remove FDB entry |
| `GET` | `/v1/networks/fdb/:vpc_id` | FDB entries for a specific VPC |

#### Storage (future)

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/volumes` | Create volume (ZeroFS + NBD) |
| `DELETE` | `/v1/volumes/:id` | Delete volume |
| `GET` | `/v1/volumes` | List volumes on this node |
| `GET` | `/v1/volumes/:id` | Volume details |
| `POST` | `/v1/volumes/:id/attach` | Attach volume to a VM |
| `POST` | `/v1/volumes/:id/detach` | Detach volume from a VM |

#### Node

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/node/status` | Node health (healthy/degraded/unhealthy), uptime, pending operations |
| `GET` | `/v1/node/health` | Detailed health checks (each subsystem pass/fail) |
| `GET` | `/v1/node/capacity` | Total vs used vs reserved resources |
| `GET` | `/v1/node/metrics` | CPU, memory, disk, network utilization |
| `GET` | `/v1/node/resources` | Summary of all managed resources by type and state |
| `POST` | `/v1/node/drain` | Start draining (stop accepting new VMs) |
| `POST` | `/v1/node/undrain` | Resume accepting VMs |
| `POST` | `/v1/node/reconcile` | Trigger immediate reconciliation cycle (debug) |
| `GET` | `/v1/node/reconcile/status` | Last reconciliation result (drift detected, changes applied) |

#### Tasks (for async operations)

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/tasks/:id` | Status of an async operation (creating VM, etc.) |
| `GET` | `/v1/tasks` | List recent tasks |

Long-running operations (VM create, VM delete) return immediately with a `task_id`. The caller polls `/v1/tasks/:id` for completion. The reconciliation loop also drives these operations forward, so polling is optional — the control plane can rely on gossip to learn when operations complete.

### Idempotency

All operations are idempotent:

| Operation | Duplicate behavior |
|---|---|
| Create instance with same ID | Return existing instance (if spec matches), or conflict error (if spec differs) |
| Create bridge for VPC that already has one | Return existing bridge (no-op) |
| Apply SG rules that already match | No-op, return current state |
| Delete already-deleted resource | Return success (already deleted) |
| Start already-running VM | Return success (no-op) |
| Stop already-stopped VM | Return success (no-op) |

The `X-Idempotency-Key` header provides additional protection for create operations: if the same key is used twice, the second request returns the resource created by the first, even if the caller crashed before receiving the response.

Generation-based optimistic concurrency: every mutation response includes a `generation` counter. If a caller sends a mutation with an outdated generation, Forge rejects it with `409 Conflict`. This prevents lost updates when multiple control plane operations target the same resource.

## Reconciliation Engine

The reconciliation engine is Forge's core. It runs continuously, comparing desired state (from Raft/redb) with actual state (from the kernel and running processes), and acting on the differences.

### Architecture

```
    ┌─────────────────────────────────────────────────┐
    │  Local redb replica (synced from Raft)           │
    │  Contains: VMs, NICs, SGs, VPCs, subnets,       │
    │  routes, NAT GWs scheduled for this node         │
    └────────────────────┬────────────────────────────┘
                         │ read
                         ▼
    ┌─────────────────────────────────────────────────┐
    │  Reconciliation Engine                           │
    │                                                  │
    │  1. Read desired state (from redb)              │
    │  2. Observe actual state (from kernel/procs)    │
    │  3. Compute diff                                │
    │  4. Apply changes (create/update/delete)        │
    │  5. Report observed state (via gossip)          │
    │                                                  │
    │  Runs every 5s (configurable) + on Raft events  │
    └────────────────────┬────────────────────────────┘
                         │ write
                         ▼
    ┌─────────────────────────────────────────────────┐
    │  Local reality                                   │
    │  Cloud Hypervisor processes, Linux bridges,      │
    │  TAP devices, VXLAN interfaces, nftables rules, │
    │  FDB entries, ARP proxy, ZeroFS connections      │
    └─────────────────────────────────────────────────┘
```

### Reconciliation loop

At a configurable interval (default 5 seconds, set via `[forge] reconcile_interval_secs` in `config.toml`), the reconciliation loop executes:

**Step 1 — Read desired state**

Query the local redb for all resources scheduled on this node:
- VMs with `node_id == this_node` and desired state `Running` or `Stopped`
- VPCs that have VMs on this node (to ensure bridges exist)
- Subnets, NICs, security groups, route tables, NAT gateways for those VPCs
- Volumes attached to VMs on this node

**Step 2 — Observe actual state**

Inspect the local system:
- Running Cloud Hypervisor processes: scan `/run/syfrah/vms/*/meta.json`, verify PID alive, ping CH API
- Linux bridges: `ip link show type bridge` (prefix `syfbr-`)
- VXLAN interfaces: `ip link show type vxlan` (prefix `syfvx-`)
- TAP devices: `ip link show type tun` (prefix `syftap-`)
- nftables rules: `nft list ruleset` or `nft -j list ruleset` (JSON)
- FDB entries: `bridge fdb show` on each VPC bridge
- ARP proxy: `ip neigh show` on VXLAN interfaces
- NAT masquerade: nftables nat chains
- Route rules: `ip rule` and `ip route` for VPC routing

**Step 3 — Compute diff**

For each resource type, compare desired vs actual:

| Desired | Actual | Action |
|---|---|---|
| VM should be Running | No CH process exists | Create TAP, attach bridge, spawn CH, boot |
| VM should be Running | CH process alive | No action (converged) |
| VM should be Stopped | CH process alive | Stop VM (graceful shutdown chain) |
| VM should be Deleted | CH process exists | Delete (full cleanup) |
| No VM desired | CH process running | Stop and cleanup (orphaned) |
| VPC has VMs here | No bridge exists | Create VXLAN + bridge |
| VPC has no VMs here | Bridge exists | Delete bridge + VXLAN (cleanup) |
| SG rules changed | nftables rules stale | Recompute and atomically replace chains |
| FDB entry missing | Remote VM exists in VPC | Add FDB entry |
| FDB entry present | Remote VM deleted | Remove FDB entry |
| NAT GW desired | No masquerade chain | Configure masquerade |
| NAT GW deleted | Masquerade chain present | Remove masquerade |
| Route table changed | Linux routes stale | Recompute and apply routes |

**Step 4 — Apply changes**

Changes are applied in dependency order:
1. Network infrastructure first (bridges, VXLANs) — these must exist before VMs can be created
2. NICs (TAP devices) — must exist before VMs boot
3. Security (nftables rules) — applied before VM traffic flows
4. Compute (VM lifecycle) — depends on network and storage
5. FDB + ARP — propagated after VM is running
6. Storage (volume attach/detach) — can happen after VM is running

Each change is logged with: resource ID, action taken, duration, success/failure.

**Step 5 — Report observed state**

After reconciliation, Forge reports to the gossip layer:
- State of each VM (Running, Stopped, Failed, with error details)
- Node health and available capacity
- Reconciliation summary (drift detected, changes applied, errors)

### Event-driven reconciliation

In addition to the periodic loop, reconciliation is triggered immediately when:
- A Raft log entry is committed that affects this node (new VM scheduled, SG updated, etc.)
- A VM crash is detected by the process monitor
- A network interface disappears (detected by netlink monitoring)
- An API call creates or deletes a resource

This ensures sub-second response to state changes while the periodic loop catches anything missed.

### Drift detection

Forge detects and corrects the following drift scenarios:

| Drift | Detection | Correction |
|---|---|---|
| VM process died | `kill(pid, 0)` fails or CH API ping fails | Mark Failed, report via gossip. Control plane may reschedule. |
| Bridge missing | `ip link show syfbr-{vpc}` not found | Recreate bridge + VXLAN, reattach TAPs |
| VXLAN missing | `ip link show syfvx-{vpc}` not found | Recreate VXLAN, reattach to bridge |
| TAP missing | `ip link show syftap-{hash}` not found | Recreate TAP, reattach to bridge. If VM was using it, mark VM Failed. |
| nftables rules drifted | Compare generated rules vs `nft list` | Atomic replacement of per-VM chain |
| FDB entry missing | Compare redb placements vs `bridge fdb show` | Re-add missing FDB entries |
| ARP proxy entry missing | Compare IPAM vs `ip neigh show` | Re-add ARP proxy entries |
| IP address missing from bridge | Compare subnets vs `ip addr show` | Re-add gateway IP |
| NAT masquerade missing | Compare NAT GW state vs nftables nat | Re-apply masquerade chain |
| Orphaned process (CH running, no desired state) | PID exists in `/run/syfrah/vms/` but not in redb | Stop and cleanup |
| Orphaned interface (kernel, no desired state) | `syfbr-*`/`syftap-*` exists but not in redb | Log warning, delete |

### Convergence guarantees

- **Eventually consistent**: after a bounded number of reconciliation cycles, actual state matches desired state for all non-failed resources. The bound is: at most 3 cycles for a single resource (1 to detect, 1 to act, 1 to verify).
- **Idempotent**: applying the same desired state any number of times produces the same result.
- **Safe**: Forge never deletes a resource it did not create. It identifies Syfrah-managed resources by naming convention (`syfbr-*`, `syftap-*`, `syfvx-*`) and metadata in redb.
- **Observable**: every reconciliation cycle produces a structured log entry with: cycle ID, duration, resources checked, drift detected, changes applied, errors encountered.
- **Bounded**: each reconciliation cycle has a deadline (default 30 seconds). If a cycle exceeds the deadline, it logs the incomplete work and resumes in the next cycle. Resources are processed in priority order (running VMs first, then network, then cleanup).

### Failure handling

When reconciliation fails for a resource:

1. **Retry with backoff**: transient failures (network blip, temporary disk full) are retried up to 3 times with exponential backoff (1s, 2s, 4s).
2. **Mark as Failed**: if retries are exhausted, the resource's observed state is set to Failed with a structured error (code, message, details).
3. **Report via gossip**: the control plane sees the failure and can decide: alert the operator, reschedule to another node, or retry later.
4. **Move on**: the reconciliation loop continues with other resources. One failed resource does not block reconciliation of others.
5. **Never silently drop**: a failed resource stays in desired state (Raft) until explicitly deleted or the control plane decides to reschedule.

### What the reconciliation loop does NOT do

- **Does not make scheduling decisions** — the control plane scheduler decides which node runs which VM.
- **Does not write to Raft** — it only reads Raft (via local redb replica) and writes to gossip.
- **Does not coordinate with other nodes** — each Forge reconciles independently. Cross-node coordination (e.g., FDB distribution) is handled by the gossip protocol, triggered by Forge.
- **Does not retry forever** — after N retries, it marks the resource as Failed and moves on.

## Node Capacity and Resource Management

### Resource tracking

Forge tracks the capacity of its node:

| Resource | Total | Source |
|---|---|---|
| vCPUs | Physical cores (from `/proc/cpuinfo`) | Detected at startup, cached |
| Memory | Total RAM (from `/proc/meminfo`) | Detected at startup, cached |
| Disk | Filesystem space (from `statvfs` on `/opt/syfrah/`) | Checked periodically |
| Network NICs | Count of TAP devices | Tracked dynamically |

Resource accounting:

```
Available = Total - System_Reserved - Used - Pending_Reserved

Where:
  System_Reserved = configurable amount reserved for the host OS and Syfrah itself
                    (default: 2 vCPUs, 4GB RAM, 20GB disk)
  Used = sum of all Active VM allocations
  Pending_Reserved = sum of all in-flight reservations (creating VMs)
```

### Overcommit policy

| Resource | Default ratio | Configurable | Rationale |
|---|---|---|---|
| CPU | 2:1 | Yes (`[forge] cpu_overcommit_ratio`) | Most workloads are bursty. 2:1 is conservative. |
| Memory | 1:1 (no overcommit) | Yes (`[forge] memory_overcommit_ratio`) | Memory overcommit leads to OOM kills. Default to safe. |
| Disk | 1:1 (no overcommit) | No | Disk overcommit leads to data loss. Never overcommit. |

With 2:1 CPU overcommit, a 32-core node can host VMs totaling 64 vCPUs. The operator can adjust this based on workload characteristics.

### Resource reporting

Forge reports capacity to the gossip layer every 10 seconds (configurable via `[forge] capacity_report_interval_secs`):

```json
{
  "node_id": "node-01HX...",
  "total_vcpus": 32,
  "used_vcpus": 18,
  "reserved_vcpus": 4,
  "available_vcpus": 42,
  "total_memory_mb": 131072,
  "used_memory_mb": 65536,
  "reserved_memory_mb": 8192,
  "available_memory_mb": 57344,
  "total_disk_gb": 1000,
  "used_disk_gb": 350,
  "available_disk_gb": 630,
  "instance_count": 12,
  "instance_count_by_state": { "Active": 10, "Creating": 1, "Failed": 1 },
  "health": "healthy",
  "draining": false,
  "timestamp": 1711555200
}
```

The scheduler (in the control plane) uses this data to place VMs. Gossip data is a hint, not a guarantee — the scheduler commits placement decisions through Raft, and Forge performs admission control locally.

### Admission control

When a create request arrives at Forge:

1. **Check capacity**: compare requested resources against available capacity (accounting for overcommit).
2. **Reject if insufficient**: return `409 Conflict` with details about what's unavailable.
3. **Reserve if sufficient**: atomically mark resources as reserved. This prevents double-booking when multiple VMs are being created concurrently.
4. **Create**: proceed with resource creation.
5. **On success**: convert reservation to used allocation.
6. **On failure**: release reservation, resources become available again.

Reservation expiry: if a creation does not complete within 60 seconds, the reservation expires automatically. The reconciliation loop detects expired reservations and releases them.

### Double-booking prevention

The scheduler may concurrently place two VMs on the same node. Without local admission control, both could succeed even if the node only has capacity for one. Forge's admission control is the authoritative capacity check:

- Scheduler places VM-A and VM-B on Node 1 (based on gossip capacity)
- VM-A create arrives first, reserves 4 vCPUs
- VM-B create arrives second, only 2 vCPUs available → rejected with `409 Conflict`
- Control plane reschedules VM-B to Node 2

This is the standard pattern in cloud providers: the scheduler is optimistic, the node agent is authoritative.

## Health Monitoring

### Self-health checks

Forge monitors its own operational health:

| Check | Method | Failure means |
|---|---|---|
| Fabric reachable | `syfrah0` interface exists and has IPv6 address | Node disconnected from mesh |
| Database operational | redb read/write test | Local state inaccessible |
| System commands available | `ip`, `nft`, `bridge` binaries exist in PATH | Cannot manage network resources |
| Cloud Hypervisor binary | CH binary exists at configured path | Cannot spawn VMs |
| KVM available | `/dev/kvm` accessible | VM mode unavailable (fallback to container mode) |
| Disk pressure | `statvfs` on `/opt/syfrah/` and `/run/syfrah/` | Risk of VM creation failure |
| Memory pressure | `/proc/meminfo` available > 5% of total | Risk of OOM |
| CPU pressure | Load average vs core count | Degraded performance |

### VM health checks

For each VM managed by Forge:

| Check | Method | Frequency | Failure action |
|---|---|---|---|
| Process alive | `kill(pid, 0)` | Every 5 seconds | Mark Failed, emit Crashed event |
| CH API responsive | `GET /vmm.ping` on Unix socket | Every 15 seconds | Mark Failed if unresponsive for 30s |
| TAP device exists | `ip link show {tap_name}` | Every reconciliation cycle | Recreate TAP, potentially mark VM Failed |
| nftables rules intact | Compare generated vs applied | Every reconciliation cycle | Re-apply rules |

### Health endpoint

`GET /v1/node/health` returns:

```json
{
  "status": "healthy",
  "checks": [
    { "name": "fabric", "status": "pass", "detail": "syfrah0 up, IPv6 assigned" },
    { "name": "database", "status": "pass", "detail": "redb read/write OK" },
    { "name": "kvm", "status": "pass", "detail": "/dev/kvm accessible" },
    { "name": "ch_binary", "status": "pass", "detail": "v43.0 at /usr/local/lib/syfrah/cloud-hypervisor" },
    { "name": "disk_pressure", "status": "pass", "detail": "65% used, 350GB free" },
    { "name": "memory_pressure", "status": "pass", "detail": "50% used, 64GB free" }
  ],
  "uptime_seconds": 86400,
  "last_reconciliation": {
    "timestamp": 1711555200,
    "duration_ms": 45,
    "drift_detected": 0,
    "changes_applied": 0,
    "errors": 0
  },
  "pending_operations": 0,
  "instance_count": 12,
  "draining": false
}
```

Status values:
- `healthy` — all checks pass, reconciliation is current
- `degraded` — some checks fail but VMs are running (e.g., high disk pressure)
- `unhealthy` — critical checks fail (no fabric, no database, no KVM)

## Drain and Maintenance

### Node drain

Drain is the standard mechanism for planned maintenance (OS upgrade, hardware replacement, Forge upgrade on cautious deployments).

1. Operator (or control plane automation) sends `POST /v1/node/drain` to Forge.
2. Forge marks itself as draining: `draining = true`.
3. Forge rejects all new instance creation requests with `503 Service Unavailable` (body: "node is draining").
4. Forge reports `draining: true` via gossip.
5. The scheduler stops placing new VMs on this node.
6. The control plane reschedules existing VMs to other nodes (stop → move volume → start).
7. Forge waits for all VMs to be migrated or stopped.
8. When `instance_count == 0`, Forge reports `drained: true`.
9. The operator can now safely perform maintenance.

### Node undrain

1. Operator sends `POST /v1/node/undrain`.
2. Forge clears the draining flag.
3. Forge reports `draining: false` via gossip.
4. The scheduler can place new VMs here again.

### Drain with force

`POST /v1/node/drain` with `{"force": true}` skips waiting for graceful migration. VMs are stopped immediately (via the shutdown chain). Use only when the node is being decommissioned.

### Drain timeout

If drain does not complete within a configurable timeout (default 30 minutes, `[forge] drain_timeout_secs`), Forge logs a warning and continues draining. The operator can:
- Wait longer
- Force drain
- Undrain (cancel)
- Investigate stuck VMs

## Upgrade Strategy

### Zero-downtime Forge upgrade

Since VMs are independent OS processes (Cloud Hypervisor), upgrading Forge does not affect running workloads:

1. New syfrah binary deployed to disk (e.g., via `syfrah update`).
2. Old Forge process receives `SIGTERM`.
3. Old Forge stops accepting new API requests (returns `503`).
4. Old Forge completes in-flight operations (grace period: 30 seconds).
5. Old Forge exits.
6. New Forge process starts.
7. New Forge scans `/run/syfrah/vms/*/meta.json` and reconnects to all running VMs (compute layer reconnect).
8. New Forge reconciles: re-discovers bridges, TAPs, VXLAN, nftables from kernel state + redb.
9. New Forge reports healthy via gossip.

**Key property**: VMs continue running throughout this process. They are not children of the Forge process — they are independent Cloud Hypervisor processes with their own PID, managed via REST API on Unix sockets.

**Graceful shutdown protocol**:
- `SIGTERM` → stop accepting requests, drain in-flight
- `SIGINT` → same as SIGTERM
- `SIGQUIT` → dump state and exit immediately (debug)
- After grace period, exit regardless of in-flight operations (they will be resumed by the new Forge via reconciliation)

### Cloud Hypervisor version management

When the syfrah binary is updated, it may include a new version of the Cloud Hypervisor binary:

- Existing VMs continue running with the CH version that spawned them (the binary is loaded in memory).
- New VMs use the new CH version from disk.
- Forge logs the version mismatch: "3 VMs running with CH v42.0, current is v43.0".
- The operator decides when to rolling-restart VMs to pick up the new CH version.
- No automatic rolling restart. This is an explicit operator decision.

## Security

### Attack surface

| Surface | Mitigation |
|---|---|
| Forge API | Bound to `syfrah0` only — unreachable from public internet |
| Fabric access | Only WireGuard-authenticated mesh members can reach `syfrah0` |
| API authentication | Mesh secret verification (phase 1), bearer token + Raft term (phase 2) |
| Input validation | All inputs validated before execution. Resource IDs: alphanumeric + hyphen. IPs: parsed and range-checked. Names: regex-validated. |
| Command execution | Forge runs pre-defined operations (ip, nft, bridge). No arbitrary shell commands from API. No `exec` with user-provided strings. |

### Process security

Forge runs as root because it needs:
- `NET_ADMIN` capability (manage WireGuard, bridges, VXLAN, nftables)
- `/dev/kvm` access (spawn Cloud Hypervisor VMs)
- cgroup management (resource limits on VMs)

Future hardening:
- Drop all capabilities except `NET_ADMIN`, `SYS_ADMIN`, and KVM access after startup
- Seccomp filter on the Forge process itself
- Read-only root filesystem for the Forge binary

### Tenant isolation

Forge enforces tenant isolation through multiple layers:

1. **VPC isolation**: different VNIs = separate L2 domains. VMs in different VPCs cannot communicate.
2. **Security groups**: per-NIC nftables chains. Default-deny ingress.
3. **Anti-spoofing**: source MAC and IP validated on every egress packet. No VM can impersonate another.
4. **IPAM**: addresses are centrally allocated. No VM chooses its own IP.
5. **Subnet isolation**: VMs in different subnets within the same VPC are isolated by default (ADR-002 route tables control inter-subnet traffic).
6. **Resource ownership**: every resource has an owner (org/project/env). Forge validates ownership on every operation.

### Audit trail

Every API call is logged with:
- Caller identity (node ID in phase 1, bearer token identity in phase 2)
- Operation (HTTP method + path)
- Resource ID
- Result (success or error code)
- Duration
- Request ID (for correlation)

Every reconciliation cycle is logged with:
- Cycle ID
- Duration
- Resources checked
- Drift detected (with details)
- Changes applied (with details)
- Errors encountered

Logs are structured JSON, written to the daemon log file (`~/.syfrah/syfrah.log`). In production, these should be ingested by a log aggregation system.

## Observability

### Metrics (Prometheus exposition format)

Forge exposes metrics at `GET /metrics` on the internal HTTP API (same address as the Forge API).

#### Instance metrics

```
forge_instances_total{state="active"} 10
forge_instances_total{state="creating"} 1
forge_instances_total{state="stopped"} 2
forge_instances_total{state="failed"} 1
forge_instance_create_duration_seconds_bucket{le="1"} 5
forge_instance_create_duration_seconds_bucket{le="5"} 12
forge_instance_create_duration_seconds_bucket{le="10"} 14
forge_instance_create_duration_seconds_sum 45.2
forge_instance_create_duration_seconds_count 14
```

#### Reconciliation metrics

```
forge_reconciliation_duration_seconds{quantile="0.5"} 0.045
forge_reconciliation_duration_seconds{quantile="0.9"} 0.12
forge_reconciliation_duration_seconds{quantile="0.99"} 0.5
forge_reconciliation_cycles_total 17280
forge_reconciliation_drift_detected_total 42
forge_reconciliation_changes_applied_total 38
forge_reconciliation_errors_total 4
```

#### API metrics

```
forge_api_requests_total{method="POST",path="/v1/instances",status="201"} 50
forge_api_requests_total{method="GET",path="/v1/instances",status="200"} 1200
forge_api_request_duration_seconds_bucket{method="POST",path="/v1/instances",le="1"} 48
forge_api_request_duration_seconds_bucket{method="POST",path="/v1/instances",le="5"} 50
```

#### Node resource metrics

```
forge_node_vcpus_total 32
forge_node_vcpus_used 18
forge_node_vcpus_reserved 4
forge_node_vcpus_available 42
forge_node_memory_bytes_total 137438953472
forge_node_memory_bytes_used 68719476736
forge_node_memory_bytes_available 60129542144
forge_node_disk_bytes_total 1073741824000
forge_node_disk_bytes_used 375809638400
forge_node_disk_bytes_available 676457349120
```

#### Health metrics

```
forge_health_check{check="fabric"} 1
forge_health_check{check="database"} 1
forge_health_check{check="kvm"} 1
forge_health_check{check="disk_pressure"} 1
forge_health_check{check="memory_pressure"} 1
forge_node_healthy 1
```

### Structured logging

All Forge logs are structured JSON:

```json
{
  "timestamp": "2026-03-30T14:00:00.123Z",
  "level": "info",
  "target": "forge::reconcile",
  "message": "reconciliation cycle completed",
  "cycle_id": "cyc-01HX...",
  "duration_ms": 45,
  "resources_checked": 24,
  "drift_detected": 1,
  "changes_applied": 1,
  "errors": 0,
  "node_id": "node-01HX..."
}
```

Log levels:
- `error` — unrecoverable failures (resource marked Failed, reconciliation error)
- `warn` — recoverable issues (transient retry, orphaned resource detected, capacity low)
- `info` — normal operations (reconciliation summary, resource state changes, API calls)
- `debug` — detailed execution (individual checks, diff computation, nftables rule generation)
- `trace` — verbose (every kernel call, every redb read, every HTTP request/response)

### Tracing

OpenTelemetry spans for:
- Every API request (method, path, status, duration)
- Every reconciliation cycle (resources checked, drift, changes)
- Every resource operation (create VM, apply nftables, add FDB entry)
- Every subsystem call (compute → CH API, overlay → ip/nft commands)

Trace ID propagated from control plane → Forge API → individual operations. The `X-Request-Id` header carries the trace ID for cross-node correlation.

## Integration with Existing Layers

### Forge and Fabric

The fabric layer provides:
- WireGuard mesh connectivity (`syfrah0` interface)
- Peer list (for FDB distribution — which nodes exist)
- Peering protocol (node join/leave)
- Gossip transport (for capacity reporting and status)

Forge uses fabric's peer list to:
- Know which remote nodes exist (for FDB entry creation)
- Discover VTEP addresses (remote nodes' fabric IPv6 for VXLAN encapsulation)
- Distribute VM placement announcements

Forge runs alongside fabric in the same daemon process. They share the `syfrah0` interface but have distinct ports: fabric peering on 51821 (TCP), Forge API on 7100 (HTTP).

### Forge and Compute

Today (ADR-001 architecture): CLI → control socket → daemon → VmManager.
With Forge: Control Plane → Forge API → VmManager (in-process).

Forge embeds the compute layer's `VmManager`. It calls compute methods directly:
- `VmManager::create(spec)` — spawn Cloud Hypervisor process
- `VmManager::boot(id)` — boot the VM
- `VmManager::shutdown_graceful(id)` — ACPI shutdown
- `VmManager::info(id)` — VM status
- `VmManager::delete(id)` — stop and clean up runtime artifacts
- `VmManager::reconnect()` — reconnect to surviving VMs after restart
- `VmManager::resize(id, vcpus, memory)` — hot-resize (beta)

Compute remains a pure runtime driver. It does not know about VPCs, subnets, security groups, or IPAM. Forge provides the orchestration context.

### Forge and Overlay

Forge calls the overlay layer's `NetworkBackend` trait directly:
- `create_vxlan(name, vni, local_ip, port)` — create VXLAN interface
- `create_bridge(name)` — create Linux bridge
- `add_bridge_ip(bridge, gateway, prefix_len)` — add subnet gateway
- `create_tap(name)` — create TAP device
- `attach_to_bridge(interface, bridge)` — wire TAP to bridge
- `apply_vm_rules(tap, mac, ip)` — apply nftables rules (being replaced by SG model per ADR-002)
- `add_fdb_entry(bridge, mac, vtep)` — add FDB entry
- `add_arp_proxy(vxlan, ip, mac)` — add ARP proxy entry
- `apply_nat(bridge, subnet)` — apply NAT masquerade
- `apply_peering_rules(bridge_a, bridge_b)` — inter-VPC routing

With ADR-002, Forge also calls the security group rule engine directly to generate and apply nftables rules from the SG model.

### Forge and Org

Forge reads org/project/environment/VPC/subnet state from its local redb replica (synced from Raft). Forge validates that every resource operation references a valid owner in the org hierarchy.

Writes to org state (create org, create project, etc.) go through the control plane → Raft → replicated to all nodes. Forge never writes org state.

### Forge and the Control Socket (CLI)

The existing Unix domain socket at `~/.syfrah/control.sock` continues to serve CLI commands. The daemon dispatches CLI requests to the appropriate handler:

- Fabric commands (`syfrah fabric *`) → FabricHandler (existing)
- Compute commands (`syfrah compute *`) → Forge, which delegates to VmManager
- Network commands (`syfrah vpc *`, `syfrah subnet *`, `syfrah sg *`) → Forge, which delegates to overlay
- Org commands (`syfrah org *`, `syfrah project *`, `syfrah env *`) → Forge, which reads/writes via Raft

In the pre-control plane phase, CLI commands that mutate state go through Forge locally. In the post-control plane phase, mutation commands go through the control plane API, which routes to the appropriate node's Forge.

### The daemon today becomes Forge tomorrow

The current fabric daemon (`layers/fabric/src/daemon.rs`) is the proto-Forge. The migration path:

1. The daemon already manages: WireGuard mesh, peering, control socket, peer health.
2. The daemon will be extended with: REST API (axum on port 7100), compute integration (VmManager), overlay integration (NetworkBackend), reconciliation engine, capacity tracker, health monitor.
3. The result IS Forge. There is no separate "Forge process." The daemon evolves into Forge.

## Configuration

Forge configuration lives in `~/.syfrah/config.toml` under the `[forge]` section:

```toml
[forge]
# REST API port (default 7100)
port = 7100

# Reconciliation interval in seconds (default 5)
reconcile_interval_secs = 5

# Maximum reconciliation cycle duration in seconds (default 30)
reconcile_deadline_secs = 30

# Capacity report interval to gossip in seconds (default 10)
capacity_report_interval_secs = 10

# CPU overcommit ratio (default 2.0)
cpu_overcommit_ratio = 2.0

# Memory overcommit ratio (default 1.0, no overcommit)
memory_overcommit_ratio = 1.0

# System-reserved resources (not available for VMs)
system_reserved_vcpus = 2
system_reserved_memory_mb = 4096
system_reserved_disk_gb = 20

# Drain timeout in seconds (default 1800 = 30 minutes)
drain_timeout_secs = 1800

# Graceful shutdown grace period in seconds (default 30)
shutdown_grace_secs = 30

# Resource reservation expiry in seconds (default 60)
reservation_expiry_secs = 60
```

All configuration values have sensible defaults. A node can run Forge with zero configuration.

## Migration Path

### From current daemon to Forge

The migration is incremental. Each phase adds functionality without breaking existing behavior.

**Step 1 — Add HTTP API scaffold**

Add an axum HTTP server to the existing daemon, bound to `syfrah0:7100`. Start with read-only endpoints:
- `GET /v1/node/status` — node health
- `GET /v1/node/capacity` — resource summary
- `GET /v1/node/health` — detailed health checks
- `GET /metrics` — Prometheus metrics

This can be done without changing any existing functionality.

**Step 2 — Compute endpoints**

Add instance CRUD endpoints that wrap the existing VmManager:
- `POST /v1/instances` — create (full orchestration: network + security + compute)
- `GET /v1/instances` — list
- `GET /v1/instances/:id` — details
- `DELETE /v1/instances/:id` — delete (full cleanup)
- `POST /v1/instances/:id/start|stop|reboot`

The control socket continues to work for CLI. API calls go through the new HTTP endpoints.

**Step 3 — Network endpoints**

Add network resource endpoints:
- Bridge, VXLAN, NIC CRUD
- SG rule application (ADR-002 model)
- FDB management
- NAT gateway management

**Step 4 — Reconciliation engine**

Add the core reconciliation loop:
- Periodic desired vs actual comparison
- Drift detection and correction
- Convergence reporting via gossip

**Step 5 — Capacity management**

Add resource tracking, overcommit policy, admission control, and reservation system.

**Step 6 — Drain and maintenance**

Add drain/undrain endpoints and the drain coordination protocol.

**Step 7 — Observability and hardening**

Add Prometheus metrics, OpenTelemetry tracing, structured logging, graceful shutdown protocol.

**Step 8 — Deprecate direct CLI-to-compute path**

Once the Forge API is stable, route all CLI compute commands through Forge (local control socket → Forge handler) instead of directly calling VmManager. This makes Forge the single entry point.

## Implementation Phases

### Phase 1 — API scaffold + compute endpoints (8-10 issues)

- Axum HTTP server on `syfrah0:7100`
- Health, capacity, and metrics endpoints
- Instance CRUD (create, list, get, delete, start, stop, reboot)
- Full create orchestration (network setup → SG apply → FDB → config-drive → compute)
- Full delete orchestration (stop → FDB remove → nftables flush → TAP delete → bridge cleanup)
- Task tracking for async operations
- Admission control (basic capacity check)
- Structured error responses with FORGE_ prefix

### Phase 2 — Network endpoints + reconciliation (8-10 issues)

- Bridge/VXLAN management endpoints
- NIC management endpoints
- SG application endpoints (ADR-002 model)
- NAT gateway endpoints
- Route table enforcement
- FDB management endpoints
- Reconciliation engine (periodic + event-driven)
- Drift detection for all resource types
- Convergence reporting via gossip

### Phase 3 — Capacity management + drain (4-5 issues)

- Full resource tracking with overcommit policy
- Reservation system with expiry
- Capacity reporting to gossip (scheduler integration)
- Node drain/undrain protocol
- Drain timeout and force drain
- Double-booking prevention via local admission

### Phase 4 — Observability + production hardening (4-5 issues)

- Prometheus metrics (instances, reconciliation, API, node resources, health)
- Structured JSON logging for all Forge operations
- OpenTelemetry tracing (API requests, reconciliation cycles, subsystem calls)
- Graceful shutdown protocol (SIGTERM handling, in-flight completion)
- Configuration hot-reload for non-disruptive tuning
- Reconciliation deadline enforcement

### Estimated total: ~25-30 issues across 4 phases

## Commercial Value

Forge enables the transition from "a collection of scripts that manage VMs" to "a programmable cloud node with a standard API." This is the foundation for:

1. **Automated operations**: the control plane can drive node-level operations without SSH. Scheduling, scaling, failover, and maintenance are API calls, not manual procedures.

2. **Self-healing infrastructure**: the reconciliation engine automatically detects and corrects drift — crashed VMs, missing network interfaces, stale firewall rules. The operator is notified; the system has already fixed itself.

3. **Multi-tenant safety**: security group enforcement, anti-spoofing, VPC isolation, and resource ownership are enforced at the node level, not just at the API level. A compromised or buggy tenant VM cannot affect other tenants.

4. **Capacity optimization**: overcommit ratios, resource tracking, and admission control let operators maximize utilization while preventing overload. The scheduler makes informed placement decisions based on real-time capacity data.

5. **Zero-downtime operations**: Forge upgrades do not restart VMs. Node drain enables planned maintenance without workload interruption. This is table stakes for production cloud infrastructure.

6. **Observability from day one**: Prometheus metrics, structured logs, and distributed tracing are built in, not bolted on. Operators can monitor, alert, and debug from the first deployed node.

7. **Standard API for ecosystem integration**: the REST API enables Terraform providers, CI/CD pipelines, custom automation, and monitoring integrations to target individual nodes or (via the control plane) the entire cluster.

## Rejected Alternatives

### 1. Forge as a separate process from the daemon

**Considered**: run Forge as an independent binary that communicates with the fabric daemon over IPC.

**Rejected**: adds IPC overhead, complicates deployment (two binaries to manage), creates a coordination problem (who owns the WireGuard interface?), and splits the control socket (which process handles CLI commands?). The daemon IS the proto-Forge. Extending it is simpler and more reliable than splitting it.

### 2. gRPC instead of HTTP/JSON for the internal Forge API

**Considered**: use gRPC for Forge's node-to-node API for type safety and SDK generation.

**Rejected**: internal node-to-node APIs are non-contractual (same binary on all nodes, no version skew). HTTP/JSON is simpler to debug (`curl` works), consistent with the existing internal HTTP API (api-architecture.md), and avoids a proto compilation dependency for what is fundamentally an internal interface. The external tenant API uses gRPC (via the gateway). Internal stays simple.

### 3. mTLS on the Forge API

**Considered**: add mTLS to authenticate callers at the Forge API level.

**Rejected for phase 1**: WireGuard already provides mutual authentication (only nodes with the mesh secret can reach `syfrah0`). Adding mTLS on top adds certificate management complexity with no security gain — it would be encrypting an already-encrypted channel and authenticating already-authenticated peers. Phase 2 adds bearer token verification for defense-in-depth when the control plane exists.

### 4. Forge manages desired state directly (no Raft)

**Considered**: Forge could be the source of truth for its node's resources, with cross-node coordination via direct API calls.

**Rejected**: this creates a split-brain problem. If a node goes down, its desired state is lost. Raft provides the single authoritative desired state that survives node failures. Forge is stateless in intent by design — it reads desired state, never owns it.

### 5. Push-based reconciliation only (no periodic loop)

**Considered**: only reconcile when Raft notifies of a change (event-driven, no periodic scan).

**Rejected**: event-driven reconciliation misses drift caused by external factors (operator manually deletes a bridge, kernel drops an interface, nftables rules are flushed by another tool). The periodic loop is the safety net that catches everything the event-driven path misses. Both are needed: events for responsiveness, periodic for completeness.

### 6. Docker/containerd as the container runtime

**Considered**: use Docker or containerd for the container fallback mode (when KVM is unavailable).

**Rejected**: Docker adds a daemon dependency and significant surface area. containerd is lighter but still complex. The compute layer chose `crun + gVisor` for minimal overhead with strong isolation. Forge does not need to second-guess this — it delegates to compute, which selects the appropriate runtime.

### 7. Port 9443 for Forge API

**Considered**: using a non-standard high port to avoid conflicts.

**Rejected**: the Forge README already specifies port 7100. Changing it creates inconsistency with existing documentation and mental models. 7100 is fine — it is only bound to `syfrah0`, not to a public interface, so conflicts with other services are unlikely.

## References

- `handbook/ARCHITECTURE.md` — full stack vision, Forge's position in the stack
- `layers/forge/README.md` — original Forge concept document
- `layers/fabric/README.md` — fabric layer that Forge runs on top of
- `layers/compute/README.md` — compute layer that Forge orchestrates
- `layers/overlay/README.md` — overlay primitives that Forge manages
- `layers/org/README.md` — organization model that Forge respects
- `handbook/adr-001-networking-roadmap.md` — networking decisions and primitives
- `handbook/adr-002-security-groups-route-tables.md` — security groups, route tables, NAT gateways, NICs
- `handbook/state-and-reconciliation.md` — reconciliation philosophy and phase models
- `handbook/api-architecture.md` — API transport and authentication decisions
- `handbook/external-api.md` — tenant-facing API gateway design
