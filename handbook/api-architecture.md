# Control Plane API Architecture

## Design Principles

This document defines how all layers (fabric, forge, compute, storage, overlay, controlplane, org, iam, products) communicate internally and expose their APIs externally.

These decisions were reached through two rounds of architectural review by five specialists covering systems engineering (Marcus, ex-AWS EC2), type safety (Yuki, ex-Scaleway), distributed systems performance (Dmitri, 10k-node experience), CLI/UX (Leila, platform engineering), and security (Chen, ex-AWS IAM). The second round incorporated requirements for a Terraform provider, remote laptop CLI, and multi-language SDKs.

### Core Principles

1. **The CLI drives the API shape.** Write the `--help` text first, then build the handler. The CLI drives the user-facing API shape, while resource semantics (naming, lifecycle, relationships) remain stable independently of CLI presentation.
2. **One binary per context.** `syfrah` on servers (local daemon control), `syfrah-cli` on laptops (remote API access).
3. **Proto files are the source of truth.** All external APIs defined in `.proto` files. SDKs generated, never hand-written.
4. **Unix sockets for local, gRPC+REST for remote.** Two trust boundaries, two transports.
5. **JSON for humans, protobuf for machines.** Local IPC stays JSON. External clients get both gRPC (typed) and REST/JSON (debuggable).
6. **Least privilege, smallest surface.** External access is a gateway pattern, not a mesh extension.

---

## Architecture Overview

Syfrah exposes three distinct API surfaces, each serving a different audience with different trust models.

### Three API Levels

#### 1. Gateway API — Tenant-Facing

**Audience:** Developers, CI/CD pipelines, Terraform providers, `syfrah-cli` from laptops.

**Purpose:** Deploy and manage resources within a tenant (org/project/environment). Create VMs, VPCs, subnets, security groups. The caller never sees or interacts with individual hypervisors.

**Transport:** gRPC + REST/JSON on port 8443, TLS 1.3, API key authentication.

**Scope:** Scoped to the caller's organization and project. A developer in org "acme" cannot see resources in org "beta-corp".

**IAM Roles:** `developer` (full CRUD on own resources), `viewer` (read-only).

**Example operations:**
- `POST /v1/instances` — create a VM (scheduler picks the hypervisor)
- `GET /v1/instances` — list my VMs across all zones
- `POST /v1/vpcs` — create a VPC
- `POST /v1/security-groups` — create a security group
- `GET /v1/subnets` — list subnets in my environment

**What tenants DON'T see:**
- Individual hypervisors (they see zones, not machines)
- Raft cluster state
- Other tenants' resources
- Forge internal APIs

#### 2. Admin API — Platform Operations

**Audience:** SRE teams, infrastructure admins, Ansible playbooks, capacity planning dashboards.

**Purpose:** Manage the cloud infrastructure itself. Drain hypervisors, view capacity, monitor the control plane, manage Raft membership.

**Transport:** Same gateway endpoint (port 8443), TLS 1.3, API key with admin role.

**Scope:** Full cluster visibility. Can see all orgs, all hypervisors, all VMs.

**IAM Roles:** `admin` (cluster operations), `owner` (admin + IAM management).

**Example operations:**
- `GET /v1/hypervisors` — list all hypervisors with capacity
- `POST /v1/hypervisors/:id/drain` — drain a hypervisor for maintenance
- `GET /v1/controlplane/status` — Raft leader, term, members
- `GET /v1/controlplane/members` — list Raft voters and learners
- `POST /v1/controlplane/promote/:id` — promote a learner to voter
- `GET /v1/metrics` — Prometheus metrics for the cluster
- `GET /v1/hypervisors/:id/capacity` — detailed capacity breakdown

**Key difference from Gateway:** Same endpoint, same TLS, but the IAM role unlocks admin-only endpoints. A `developer` API key calling `/v1/hypervisors` gets 403 Forbidden.

#### 3. Forge API — Machine-to-Machine (Internal)

**Audience:** Hypervisor daemons communicating with each other.

**Purpose:** Raft consensus, scheduler remote placement, FDB synchronization, overlay reconciliation. Never called by humans or external tools.

**Transport:** HTTP/JSON on port 7100, over the WireGuard fabric (`syfrah0` interface). Not exposed on public interfaces.

**Authentication:** WireGuard mesh membership is the trust boundary. Only nodes in the mesh can reach port 7100.

**Example operations (internal):**
- `POST /v1/instances` (with `?direct=true`) — direct VM creation on this hypervisor (called by scheduler)
- `GET /v1/hypervisor/health` — this hypervisor's health
- `GET /v1/hypervisor/capacity` — this hypervisor's capacity
- `POST /raft/append_entries` — Raft log replication
- `POST /raft/vote` — Raft leader election
- `POST /raft/join` — Raft cluster membership

**Key difference:** Forge is per-hypervisor, internal, and never exposed to the internet. The Gateway is the single public entry point for all external access.

### How They Relate

```
                    ┌─────────────────────────────┐
                    │  External (Internet / VPN)   │
                    │                              │
                    │  syfrah-cli, Terraform, SDK  │
                    │  Dashboard, CI/CD            │
                    └──────────┬──────────────────┘
                               │ TLS + API key
                               ▼
                    ┌──────────────────────────────┐
                    │  Gateway (port 8443)          │
                    │                               │
                    │  developer role → tenant API  │
                    │  admin role → admin API        │
                    │                               │
                    │  Forwards mutations to        │
                    │  Raft leader via fabric       │
                    └──────────┬───────────────────┘
                               │ WireGuard fabric
                               ▼
                    ┌──────────────────────────────┐
                    │  Forge (port 7100, per-HV)   │
                    │                               │
                    │  Raft RPCs (7200)             │
                    │  VM creation (direct)          │
                    │  Reconciliation               │
                    │  FDB sync                     │
                    │                               │
                    │  Machine-to-machine only      │
                    └──────────────────────────────┘
```

One physical gateway endpoint. Two levels of human access (tenant vs admin) differentiated by IAM. One internal bus (Forge) that humans never touch.

**Gateway node designation:** In v1, gateway nodes are explicitly designated by the operator via configuration (`--role gateway`). They are not dynamically elected. API key state (hashed keys, CIDR allowlists, rate limit counters) is replicated to all gateway nodes through the Raft-backed IAM state. If a gateway node fails, the operator promotes another node or relies on DNS failover across multiple designated gateways. A future version may automate gateway election based on health and topology.

**Mutation flow:** The gateway forwards every mutating request to the current Raft leader. The leader is the source of truth for state transitions, but execution may occur on follower/worker nodes after the transition is committed. For example, a `vm.create` request is committed as a state transition by the leader; the actual VM creation is executed by the worker node that owns the target hardware.

---

## Transport

### 1. Local: Server CLI → Daemon

**Transport:** Unix domain socket at `~/.syfrah/control.sock`

- Kernel-enforced access control (0600, owner-only)
- Peer credential retrieval via `SO_PEERCRED` (caller UID without tokens)
- Zero network exposure by construction
- 5-15 microsecond round-trip
- Length-prefixed JSON frames (max 64KB, 5s read timeout)

**One socket, daemon dispatches all layers.** The daemon multiplexes through a `LayerRequest` enum. Cost: one enum match (nanoseconds).

**Auth:** None needed. Filesystem permissions are the auth. `SO_PEERCRED` provides caller UID for audit.

### 2. Remote: External Clients → API Gateway

**Transport:** gRPC over HTTP/2 + TLS 1.3

- Primary protocol for Terraform provider, laptop CLI, and SDKs
- Connection multiplexing (100 Terraform resources: ~5s via gRPC vs ~20s via REST/HTTP1.1)
- Streaming support for long operations (VM creation progress, log tailing)
- Typed clients generated from proto files

**REST/JSON gateway** auto-generated from the same `.proto` files via `grpc-gateway`:
- For `curl` debugging, webhooks, and browser JS clients
- Same endpoints, same auth, same rate limits
- One source of truth (proto), two access patterns

**Bind address:** Public IP, TLS-only. Never plaintext. Certificate via Let's Encrypt or operator-provided.

### 3. Internal: Node → Node

**Transport:** HTTP/1.1 + JSON over WireGuard fabric interface (`syfrah0`)

- Internal HTTP relies on WireGuard as the transport security layer; no additional TLS is required in v1
- Bound to mesh IPv6 address only (never public interface)
- Read-only monitoring also served here (`/metrics`, `/v1/{layer}/health`)

### 4. Gossip & Consensus

**Transport:** Purpose-built protocols over WireGuard

- Raft (openraft): log replication between nodes
- Gossip (SWIM/foca): health and membership
- These are NOT HTTP — they use their own binary protocols optimized for their specific access patterns

---

## Message Format

### Local IPC: JSON

All `LayerRequest`/`LayerResponse` types serialized via serde_json. Every public type already derives `Serialize`/`Deserialize`. Debuggable with `socat` and `jq`.

### External API: Protobuf (gRPC) + JSON (REST gateway)

Proto files are the source of truth. The REST gateway automatically serializes proto messages to/from JSON.

### Internal node-to-node: JSON

HTTP/JSON over the fabric. Same serde types as local IPC. Same version, same binary — no compatibility concerns. Internal node-to-node payloads are non-contractual: they are an implementation detail of the current binary and carry no cross-version stability guarantee. If semantic drift between the internal JSON surface and the external proto surface becomes a risk, critical internal paths will be migrated to proto definitions.

### Why both

| Context | Format | Why |
|---------|--------|-----|
| Local IPC | JSON | Already works, debuggable, no migration cost |
| External gRPC | Protobuf | Typed clients, version skew safety, SDK generation |
| External REST | JSON (from proto) | curl/jq debugging, webhooks, browser clients |
| Internal HTTP | JSON | Same binary = no version skew, serde already everywhere |

---

## Proto Files

### Location

Per-layer protos live alongside their layer crate; shared types live in `api/proto/`:

```
layers/fabric/proto/fabric.proto     # Fabric service (peers, topology, peering)
layers/forge/proto/forge.proto       # Forge service (per-node ops)
layers/compute/proto/compute.proto   # Compute service (VMs, images)
layers/overlay/proto/overlay.proto   # Overlay service (VPCs, subnets, security groups)
layers/storage/proto/storage.proto   # Storage service (volumes, snapshots)
layers/org/proto/org.proto           # Organization service (orgs, projects, envs)

api/proto/syfrah/v1/common.proto     # Shared types (pagination, errors, health)

buf.yaml              # buf lint + breaking config
buf.gen.yaml          # code generation config
```

### CI enforcement

- `buf lint` on every PR (style, naming)
- `buf breaking` against the previous release tag (no accidental breaking changes)
- Generated SDK code committed to `sdk/{go,python,js}/`

### SDK generation

| Language | Output | Consumer |
|----------|--------|----------|
| Go | `sdk/go/` (go module) | Terraform provider |
| Rust | `sdk/rust/` (crate) | `syfrah-cli` laptop binary |
| Python | `sdk/python/` (PyPI) | Automation scripts |
| JS/TS | `sdk/js/` (npm) | Web dashboards |

---

## API Naming

### Proto services

```
package syfrah.fabric.v1;

service FabricService {
  rpc ListPeers(ListPeersRequest) returns (ListPeersResponse);
  rpc RemovePeer(RemovePeerRequest) returns (RemovePeerResponse);
  rpc GetTopology(GetTopologyRequest) returns (GetTopologyResponse);
  rpc GetStatus(GetStatusRequest) returns (GetStatusResponse);
}
```

### REST paths (auto-generated from proto)

```
GET    /v1/fabric/peers
DELETE /v1/fabric/peers/{name}
GET    /v1/fabric/topology
GET    /v1/fabric/status
POST   /v1/compute/vms
GET    /v1/compute/vms/{id}
DELETE /v1/compute/vms/{id}
```

### CLI commands (map 1:1)

```bash
syfrah-cli fabric peers list
syfrah-cli fabric peers remove web-3
syfrah-cli fabric topology
syfrah-cli compute vm create --name web-1 --vcpu 2
```

One naming scheme across proto definition, REST path, and CLI command.

---

## Two Binaries

### `syfrah` — Server CLI

- Runs on the server, same machine as the daemon
- Talks to local daemon via Unix socket
- No auth needed (filesystem permissions)
- No network access
- Already built (current binary)

### `syfrah-cli` — Laptop CLI

- Runs on operator's laptop or CI runner
- Talks to remote API gateway via gRPC over TLS
- Auth: API key stored in OS keychain or env var
- Context switching for multiple clusters

```bash
# Setup
syfrah-cli context set --name prod --endpoint https://api.prod.example.com
syfrah-cli context use prod
export SYFRAH_API_KEY=syf_key_proj_...

# Usage (same command structure as server CLI)
syfrah-cli fabric peers list
syfrah-cli fabric topology --json
syfrah-cli compute vm create --name web-1
```

### Shared codebase

```
bin/syfrah/          # Server binary (Unix socket client)
bin/syfrah-cli/      # Laptop binary (gRPC client)
layers/api-client/   # Shared HTTP/gRPC client library
layers/core/         # Shared types (used by both)
sdk/go/              # Generated Go client (Terraform)
sdk/rust/            # Generated Rust client (syfrah-cli uses this)
```

The Terraform provider imports `sdk/go/` and uses the same gRPC API as `syfrah-cli`.

Both binaries share the same command grammar wherever semantics overlap; differences are limited to transport, auth, and context management. For example, `syfrah fabric peers list` and `syfrah-cli fabric peers list` accept the same flags and produce the same output shape. The shared grammar is enforced by a common clap command definition in `layers/core/`.

---

## Authentication & Authorization

### API Keys

> **Prefix convention:** `syf_sk_` = mesh secret (local to the node, never leaves it — used for WireGuard key derivation). `syf_key_` = API key (for external gateway access by laptop CLI, Terraform, SDKs). These are distinct credentials with different trust boundaries; do not confuse them.

**Format:** `syf_key_{project_short_id}_{random_256bit_base58}`

**Properties:**
- Scoped per-project, per-role (owner/admin/developer/viewer)
- Optional CIDR allowlist (`--allowed-cidrs 203.0.113.0/24`)
- Optional TTL (`--ttl 1h` for CI pipelines — key expires automatically)
- Rotation with grace period (`--grace 5m` — old key valid for 5 min after new one issued)
- Server stores SHA-256 hash only, never the raw key
- Last-used IP and timestamp recorded for audit

**Creation:**
```bash
syfrah-cli iam apikey create --project backend --role developer --name ci-deploy --ttl 1h
# → syf_key_bknd_7Fa9x2Qm...
```

**Usage:**
```bash
# Environment variable (recommended for CI/Terraform)
export SYFRAH_API_KEY=syf_key_bknd_7Fa9x2Qm...

# Or stored in OS keychain via syfrah-cli login
syfrah-cli login --endpoint https://api.prod.example.com
```

### Authorization

Every request checks `(caller_key, layer, command) → allow/deny`.

Default: deny all. Initial policy:
- API key with `owner` role: full access
- API key with `developer` role: create/manage VMs, VPCs, volumes. Cannot manage IAM.
- API key with `viewer` role: read-only

### Local auth (server CLI)

No tokens. `SO_PEERCRED` provides caller UID. Daemon checks UID against allowed list (root + daemon owner). Caller UID recorded in every audit entry.

---

## Rate Limiting

For external API only (not local Unix socket, not internal fabric).

| Scope | Default | Configurable |
|-------|---------|-------------|
| Per API key | 100 req/s sustained, 200 burst | Yes |
| Per endpoint aggregate | 1000 req/s | Yes |
| Response | HTTP 429 + `Retry-After` header | — |

Terraform at peak: ~10-20 req/s. Well within limits.

Implementation: in-memory token bucket per gateway instance. No external dependency. Rate limits are enforced per gateway instance in v1; the effective burst for a given API key is `burst × gateway_count`. Global rate limiting across gateways may be added in a future version if needed.

---

## Error Responses

### Format (all transports)

```json
{
  "error": {
    "code": "PEER_NOT_FOUND",
    "message": "No peer named 'web-3' in the mesh. Did you mean 'web-03'?",
    "trace_id": "req-a7f3e29b1c04"
  }
}
```

- **code**: Machine-readable, stable across versions. Scripts and Terraform match on this.
- **message**: Human-readable, may change. Includes actionable next steps.
- **trace_id**: 12-character random hex ID (48 bits of entropy), logged server-side. For incident debugging and cross-node correlation at scale.

### Error codes per layer

Each layer prefixes its error codes with the layer name (e.g. `FABRIC_PEER_NOT_FOUND`, `COMPUTE_VM_NOT_FOUND`). See [external-api.md](external-api.md#error-codes) for the canonical error code table with HTTP status mappings.

---

## Versioning

### External API (proto-based)

Proto field numbers provide wire compatibility:
- **New fields:** Add with new field number. Old clients ignore them. Non-breaking.
- **Removed fields:** Mark as `reserved`. Old clients get default values. Non-breaking.
- **Breaking changes:** New proto package version (`syfrah.fabric.v2`). Both served simultaneously for one release cycle.

CI enforces: `buf breaking` rejects any PR that breaks wire compatibility within a package version.

### Server responds with version

Every response includes an `API-Version` header. Clients can detect incompatibility and emit clear errors.

### Local IPC (serde-based)

CLI and daemon are the same binary — always the same version. No compatibility concern. Additive-only changes with `#[serde(default)]`.

### Deprecation policy

External API changes follow: announce deprecation → 2 minor versions → remove. Terraform users need time to update providers.

---

## Audit

Every mutating API call is logged:

| Field | Source |
|-------|--------|
| Timestamp | Server clock |
| Caller | UID (local) or API key name (remote) |
| Source IP | `SO_PEERCRED` (local) or TCP peer (remote) |
| Layer | `fabric`, `compute`, etc. |
| Command | `peer.remove`, `vm.create`, etc. |
| Result | Success or error code |
| Trace ID | 12-char random hex, returned to client |

In v1, audit entries for mutating actions are replicated through the same consensus path as state transitions for simplicity. A dedicated audit store (e.g., append-only local log with async replication) may be introduced later to decouple audit durability and volumetry from the Raft state machine.

The audit module is a shared dependency used by all layers — not fabric-specific.

---

## Network Exposure Rules

| Resource | Local (same node) | Fabric (mesh nodes) | Public (internet) |
|----------|-------------------|---------------------|-------------------|
| Control socket | ✓ (owner only) | ✗ | ✗ |
| State database (redb) | ✓ (owner only) | ✗ | ✗ |
| Audit log | ✓ (owner only) | ✗ | ✗ |
| Internal HTTP API | ✓ | ✓ (syfrah0 only) | ✗ |
| Prometheus metrics | ✓ | ✓ (syfrah0 only) | ✗ |
| External gRPC + REST API | ✓ | ✓ | ✓ (TLS + API key) |
| WireGuard UDP (51820) | ✓ | ✓ | ✓ |
| Peering TCP (51821) | ✓ | ✓ | ✓ (temporary) |

If someone asks for "remote server CLI access," the answer is SSH — not opening the control socket.

---

## Layer Registration

### Each layer provides

```rust
pub trait LayerHandler: Send + Sync {
    /// Handle local control socket requests
    async fn handle(&self, request: LayerRequest) -> LayerResponse;

    /// Internal control endpoints (node-to-node coordination, state forwarding)
    fn internal_control_routes(&self) -> axum::Router;

    /// Internal observability endpoints (health, metrics — read-only, safe to expose broadly)
    fn internal_observability_routes(&self) -> axum::Router;

    /// gRPC service for external API (generated from proto)
    fn grpc_service(&self) -> tonic::transport::server::Router;
}
```

Each layer crate exports:
1. `{Layer}Request` / `{Layer}Response` — for local Unix socket IPC
2. `{Layer}Handler` implementing `LayerHandler`
3. `{Layer}Command` — clap subcommand for server CLI
4. Proto-generated gRPC service — for external API
5. axum `Router` — for internal observability HTTP

### Daemon composition (compile-time)

```rust
// bin/syfrah/src/main.rs
let daemon = Daemon::new()
    .register(FabricHandler::new(config))
    .register(ForgeHandler::new(config))
    .register(ComputeHandler::new(config))
    .build();

// Three listeners, one daemon
daemon.serve(
    unix_socket,              // Local CLI
    internal_http,            // Node-to-node control + observability (same listener in v1, logically separated)
    external_grpc_and_rest,   // Terraform, laptop CLI, SDKs
).await;
```

Feature flags in `Cargo.toml` control which layers are compiled. Disabled layers are excluded at compile time.

---

## Migration Plan

### Phase 1: Extract shared control protocol (2 days)

Move socket creation, framing, `ControlHandler` trait from `fabric/control.rs` into shared crate. Rename `ControlRequest` → `FabricRequest`, wrap in `LayerRequest`.

### Phase 2: Add error codes and trace IDs (1 day)

Replace `Error { message }` with `Error { code, message, trace_id }`. Add `SO_PEERCRED` caller UID to audit entries.

### Phase 3: Proto definitions (2 days)

Create `layers/fabric/proto/fabric.proto` mirroring current `FabricRequest`/`FabricResponse`. Add `buf.yaml`, CI lint/breaking checks.

### Phase 4: gRPC server (3 days)

Implement tonic gRPC server in fabric layer. Bind to configurable external port with TLS. Add API key validation middleware.

### Phase 5: REST gateway (1 day)

Add grpc-gateway for auto-generated REST/JSON from proto. Same port, path-based routing.

### Phase 6: Laptop CLI (3 days)

Create `bin/syfrah-cli/` with context switching, gRPC client (from generated Rust SDK), keychain-based credential storage.

### Phase 7: Terraform provider (5 days)

Create `terraform-provider-syfrah/` in Go. Import generated Go SDK from `sdk/go/`. Implement resources: `syfrah_mesh`, `syfrah_node`, `syfrah_vpc`, `syfrah_vm`.

### Phase 8: SDK generation CI (2 days)

`buf generate` in CI. Auto-publish Go module, Python package, JS/TS package on each release.

---

## Anti-Patterns

1. **Never hand-write a client library.** Generate from proto files. One change → all SDKs updated.
2. **Never create a separate API gateway process.** The daemon IS the gateway.
3. **Never expose the Unix control socket to the network.** Local only.
4. **Never use bearer tokens for local IPC.** `SO_PEERCRED` is unforgeable.
5. **Never serve plaintext HTTP on public interfaces.** TLS 1.3 or nothing.
6. **Never create one socket per layer.** One socket, one dispatch enum.
7. **Never mix trust boundaries on the same transport.** Local/fabric/public are separate.
8. **Never let the HTTP bind to `0.0.0.0` without TLS.** Enforced in code.
9. **Never version prematurely.** Ship v1, use proto field numbers for additive changes.
10. **Never store API keys in plaintext config files.** OS keychain or env vars.
