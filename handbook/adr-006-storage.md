# ADR-006: Storage — S3-Backed Block Devices via ZeroFS

**Status**: Proposed
**Date**: 2026-04-01
**Decided by**: Sacha + team
**Depends on**: ADR-003 (Forge), ADR-004 (hypervisor model), ADR-005 (control plane — Raft state, fencing, reschedule safety)

## 1. Context and motivation

Every cloud provider offers persistent block storage (AWS EBS, GCP Persistent Disks, Azure Managed Disks). Without it, VMs are ephemeral — data dies with the machine. The traditional approach is a distributed storage cluster: Ceph, GlusterFS, or a custom replicated block store. These systems require dedicated infrastructure, complex operations, and a minimum of three nodes just for storage redundancy.

Syfrah's operators rent dedicated servers from OVH, Hetzner, Scaleway, and similar providers. Every one of these providers also offers cheap S3-compatible object storage. This is the leverage point: the provider already handles durability and replication at the object storage level. Building another replication layer on top is redundant complexity.

The storage layer defined here turns provider-managed S3 buckets into block devices that VMs consume as standard `/dev/vdX` devices. [ZeroFS](https://github.com/Barre/ZeroFS) is the engine that makes this possible — it exposes NBD (Network Block Device) endpoints backed by S3, with a local SSD+memory cache that absorbs the vast majority of I/O.

This ADR defines the complete storage architecture: the volume resource model, state machine, snapshot model, attachment semantics, cache architecture, migration mechanics, Raft integration, fencing, CLI, Forge integration, performance characteristics, failure scenarios, and limitations.

### Why now

The compute layer (ADR-001, compute README) defines `virtio-block` integration and expects block device paths from the storage layer. Forge (ADR-003) defines `VolumeMgr` as a placeholder for ZeroFS/NBD management. The control plane (ADR-005) includes `CreateVolume`, `DeleteVolume`, `AttachVolume`, and `DetachVolume` as Raft commands but does not specify their semantics. The hypervisor model (ADR-004) explicitly notes that remote volumes (ZeroFS/S3) are not hypervisor-bounded — they attach over the network and backed by S3. This ADR fills the gap.

### Relationship to existing decisions

- **ARCHITECTURE.md** — Storage is a vertical layer in the stack, alongside compute and overlay. ZeroFS + S3 is the chosen approach. No Ceph, no GlusterFS, no storage cluster.
- **ADR-003 (Forge)** — Forge manages local ZeroFS instances via `VolumeMgr`. Forge creates/destroys NBD devices, attaches/detaches to Cloud Hypervisor, monitors cache utilization, and reports storage capacity via gossip.
- **ADR-004 (Hypervisor)** — The scheduler does NOT filter by remote volume capacity. Remote volumes are not hypervisor-bounded. Local disk capacity is tracked for cache sizing only.
- **ADR-005 (Control plane)** — Volume records (metadata) are Raft state. `placement_generation` fencing applies to volumes during VM reschedule to prevent split-brain writes. Stateful VMs with exclusive-write volumes require fencing before reschedule.
- **Compute README** — Cloud Hypervisor receives block device paths (`/dev/nbd*`) via `virtio-block`. Rate limiting (bandwidth + IOPS) is applied at the `virtio-block` layer before the NBD device.

## 2. Design decision: S3 as durability layer — not a distributed storage cluster

This is the core architectural choice. The storage layer delegates durability entirely to the provider's S3 service. ZeroFS handles the block device abstraction, caching, compression, and encryption. No data replication between Syfrah nodes. No storage cluster to operate.

| Requirement | Syfrah (ZeroFS + S3) | Ceph | GlusterFS |
|---|---|---|---|
| Durability backend | Provider's S3 (managed) | Own OSDs (self-managed) | Own bricks (self-managed) |
| Minimum nodes for storage | 1 | 3+ | 3+ |
| Operational complexity | Single binary, no cluster | Complex cluster, MON/OSD/MDS | Complex cluster, trusted pool |
| Encryption at rest | Always on (XChaCha20-Poly1305) | Optional | Optional |
| Cache layer | Built-in (memory + SSD) | Separate OSD cache tier | Separate |
| Block device exposure | NBD | RBD (kernel module) | No (file only) |
| Data replication | Delegated to S3 | 2-3x within cluster | 2-3x within cluster |
| Cost per GB/month | S3 pricing (~€0.005-0.01/GB) | Raw disk + replication overhead | Raw disk + replication overhead |
| Language | Rust | C++ | C |

The trade-off is clear: the storage layer depends on an external service (S3). If S3 is down, writes eventually stall. This is acceptable because:

1. S3 is designed for 99.99% availability. Provider-managed S3 is more reliable than a self-operated Ceph cluster on rented hardware.
2. The local cache absorbs I/O during short S3 outages (minutes). Only sustained outages cause stalls.
3. The operator chooses their S3 provider. They can use the same provider as their servers (lowest latency) or a separate one (isolation).

### How ZeroFS works

ZeroFS is an open-source Rust project that turns S3-compatible object storage into usable block devices.

1. Data is split into **256KB chunks**, compressed (LZ4), and encrypted (XChaCha20-Poly1305)
2. Chunks are managed by **SlateDB**, an LSM-tree engine that handles memtables, WAL, compaction, and flush to S3
3. A two-tier **cache** (memory + SSD) absorbs reads and writes — hot data never touches S3
4. ZeroFS exposes **NBD (Network Block Device)** endpoints — each volume appears as a `/dev/nbd*` device on the host

The NBD device is a standard Linux block device. Cloud Hypervisor attaches it via `virtio-block`. The guest VM sees `/dev/vda` or `/dev/vdb` and formats it with whatever filesystem it wants. The guest has no knowledge of ZeroFS, S3, or caching.

## 3. Durability Contract

### Definitions

- **Write acknowledged**: ZeroFS accepted the write into its memory buffer. The VM's write syscall returned. Data is NOT durable yet.
- **Write durable**: The write has been persisted to the S3 WAL (Write-Ahead Log). Survives ZeroFS crash, hypervisor crash, memory loss.
- **Write committed**: The write has been compacted from WAL into permanent SST chunks on S3. Survives S3 WAL expiry/rotation.

### Guarantees

| Operation | Guarantee |
|-----------|-----------|
| `write()` without fsync | Acknowledged but NOT durable. Lost on crash. Equivalent to write-back cache semantics. |
| `write()` + `fsync()` | Durable after fsync returns. WAL entry persisted to S3. Survives hypervisor crash. |
| `detach` returns success | ALL durable writes (fsynced) have reached S3. Non-fsynced writes may be lost. |
| Hypervisor crash | Non-fsynced writes since last fsync are LOST. Fsynced writes are recoverable from S3 WAL. |
| ZeroFS crash | Same as hypervisor crash — WAL replay recovers fsynced data. |
| S3 outage during write | fsync BLOCKS until S3 is reachable. If timeout exceeded: fsync returns EIO to guest. |

### What this means for workloads

This is **asynchronous durability** — NOT synchronous replicated block storage.

- **Write-back semantics**: like a disk with a write cache and battery backup, EXCEPT the "battery" is the S3 WAL, not a capacitor. fsync flushes to WAL.
- **NOT equivalent to**: AWS EBS (synchronous replication to multiple AZs), local NVMe (persistent on write), or Ceph RBD (synchronous quorum write).
- **Equivalent to**: a local SSD with write-back cache where fsync goes to a remote WAL.

### Honest statement

"Syfrah volumes provide block device emulation over object storage with asynchronous durability semantics. Durability is guaranteed only for writes explicitly fsynced by the guest. Non-fsynced writes may be lost on crash. This is a deliberate trade-off: operator simplicity and zero-infrastructure storage in exchange for weaker durability guarantees compared to synchronous replicated block storage."

## 4. Volume resource model

A volume is a first-class resource managed by the control plane (Raft) and executed by Forge (local ZeroFS).

```rust
struct Volume {
    /// Globally unique identifier. Format: vol-{ulid}.
    /// Assigned at creation time. Immutable.
    id: VolumeId,

    /// Human-readable name. Unique within an environment.
    name: String,

    /// Size in gigabytes. Set at creation, can be resized (requires detach).
    size_gb: u32,

    /// Current desired state in the volume lifecycle.
    desired_state: VolumeDesiredState,

    /// The VM this volume is attached to, if any.
    attached_to: Option<VmId>,

    /// The hypervisor where the NBD device is currently active.
    /// Set when attached, cleared when detached.
    hypervisor_id: Option<HypervisorId>,

    /// S3 key prefix where this volume's data lives.
    /// Format: volumes/{id}/gen-{generation}/
    /// All SST files, WAL segments, and metadata for this volume
    /// are stored under this prefix in the configured S3 bucket.
    s3_prefix: String,

    /// Encryption key identifier for this volume.
    /// In v1, all volumes in a region share the same encryption key
    /// (derived from the operator-configured passphrase).
    encryption_key_id: String,

    /// Snapshots taken from this volume.
    snapshot_ids: Vec<SnapshotId>,

    /// Organization, project, and environment this volume belongs to.
    org_id: OrgId,
    project_id: ProjectId,
    env_id: EnvId,

    /// Placement generation. Incremented on every attach/detach/reschedule.
    /// Used for fencing (see §12) and S3 prefix isolation.
    placement_generation: u64,

    /// Whether this volume is a root volume (auto-created with a VM)
    /// or a data volume (created independently).
    volume_type: VolumeType,

    /// Timestamps.
    created_at: u64,
    updated_at: u64,
}

enum VolumeType {
    /// Root volume: created automatically when a VM is created.
    /// Lifecycle is tied to the VM — deleted when the VM is deleted.
    Root,
    /// Data volume: created independently via CLI/API.
    /// Lifecycle is independent of any VM.
    Data,
}
```

### Source of truth table

| Data | Source of truth | Why |
|---|---|---|
| Volume record (id, name, size, state, attachment) | Raft (redb materialized view) | Must be consistent across the cluster |
| Volume data (chunks, SST files, WAL) | S3 | Durability is delegated to the provider |
| NBD device state (`/dev/nbd*` active or not) | Local kernel (observed by Forge) | Runtime artifact, not persisted |
| Cache contents (memory + SSD) | Local hypervisor | Ephemeral, rebuilt on demand |
| Snapshot record (id, SST file list) | Raft (redb materialized view) | Must be consistent across the cluster |
| S3 configuration (endpoint, bucket, keys) | Raft (redb materialized view) | Per-region config, set by operator |

## 5. Volume State — Desired vs Observed

### Desired state (in Raft)

```rust
enum VolumeDesiredState {
    /// Volume should exist and be available for attachment.
    Available,
    /// Volume should be attached to the specified VM on the specified hypervisor.
    AttachedTo { vm_id: VmId, hypervisor_id: HypervisorId },
    /// Volume should be deleted. All data removed from S3.
    Deleted,
}

struct VolumeDesiredRecord {
    target: VolumeDesiredState,
    generation: u64,
}
```

### Observed state (per-hypervisor, in Forge)

```rust
struct VolumeObservedState {
    /// Whether the NBD device is connected and serving I/O.
    nbd_connected: bool,
    /// Bytes currently in the local cache (memory + SSD).
    cache_bytes: u64,
    /// Bytes written but not yet flushed to S3 WAL.
    dirty_bytes: u64,
    /// Whether Cloud Hypervisor has the device attached as virtio-block.
    ch_attached: bool,
    /// Timestamp of last observation.
    last_observed: u64,
}
```

### Reported state (to API/CLI)

The reported state is derived from the combination of desired and observed state:

| Reported state | Meaning |
|----------------|---------|
| Creating | Desired: Available, Observed: not yet initialized on S3 |
| Available | Desired: Available, Observed: NBD disconnected, no dirty data |
| Attaching | Desired: Attached, Observed: NBD connecting or CH not yet attached |
| Attached | Desired: Attached, Observed: NBD connected AND CH attached |
| Detaching | Desired: Available, Observed: flushing cache, disconnecting NBD |
| Resizing | Desired: Available with new size, Observed: resize in progress |
| Deleting | Desired: Deleted, Observed: S3 objects being removed |
| Deleted | Desired: Deleted, Observed: all S3 data removed. Record retained for audit (TTL-based cleanup). |
| Error | Desired != Observed AND reconciliation failed after N retries |

### State diagram

```
Creating ──► Available ──► Attaching ──► Attached ──► Detaching ──► Available
    │                                       │                          │
    ▼                                       ▼                          ▼
  Error                                   Error                      Error

Available ──► Deleting ──► Deleted

Available ──► Resizing ──► Available
                 │
                 ▼
               Error
```

### Transition rules

1. **Attach requires `Available`**. You cannot attach a volume that is already attached, being deleted, or in error.
2. **Detach requires `Attached`**. Detaching an available volume is a no-op (idempotent).
3. **Delete requires `Available`**. You must detach before deleting. This is deliberate — no "force delete while attached" to prevent accidental data loss.
4. **Delete is blocked during in-flight snapshot or restore operations**. The volume cannot transition to Deleting while a snapshot create or snapshot restore references it.
5. **Resize requires `Available`**. No live resize in v1. Detach → resize → reattach.
6. **Error recovery**: an `Error` volume can transition to `Available` (if the underlying issue is resolved) or `Deleting` (to clean up).

### Invalid transitions are rejected

Any transition not listed above returns a `VolumeTransitionError` with the current state and the attempted target state. The state machine is enforced in the Raft state machine's `apply` handler — not in Forge, not in the API layer. Raft is the single point of enforcement.

## 6. Snapshot Protocol

### Crash-consistent only (v1)

Snapshots are crash-consistent, NOT application-consistent. Equivalent to pulling the power cord — the filesystem will be in whatever state the last fsync left it.

For app-consistent snapshots, the guest must:
1. Freeze I/O (e.g., `fsfreeze --freeze`)
2. Request snapshot via API
3. Thaw I/O

Syfrah does NOT automatically freeze/thaw. This is the tenant's responsibility.

### Snapshot resource model

```rust
struct Snapshot {
    /// Globally unique identifier. Format: snap-{ulid}.
    id: SnapshotId,

    /// The volume this snapshot was taken from.
    source_volume_id: VolumeId,

    /// Current state.
    state: SnapshotState,

    /// Size in gigabytes (same as the source volume at snapshot time).
    size_gb: u32,

    /// S3 prefix where snapshot metadata is stored.
    /// Points to the set of SST files that represent this snapshot.
    s3_prefix: String,

    /// Organization context (inherited from source volume).
    org_id: OrgId,
    project_id: ProjectId,
    env_id: EnvId,

    /// Timestamps.
    created_at: u64,
}

enum SnapshotState {
    /// ZeroFS is flushing pending writes and recording the SST file list.
    Creating,
    /// Snapshot is available for restore.
    Available,
    /// SST file references are being removed. Actual SST files are
    /// garbage-collected only if no other volume/snapshot references them.
    Deleting,
    /// Terminal.
    Deleted,
}
```

### Snapshot creation protocol

1. ZeroFS pauses new compactions (but continues accepting writes to WAL)
2. ZeroFS records the current manifest: list of SST files + WAL position
3. Manifest is committed to Raft as `SnapshotManifest { volume_id, generation, sst_files: Vec<String>, wal_position: u64 }`
4. ZeroFS resumes compactions
5. Snapshot is "taken" at the WAL position — data before that position is in the snapshot, data after is not

### Snapshot restore protocol

1. Create a new volume with a new generation
2. Initialize ZeroFS with the snapshot's manifest (SST files + WAL position)
3. New volume reads from the snapshot's SST files (shared, not copied)
4. New writes go to the new volume's own prefix

### Why this is cheap

In a traditional block storage system, a snapshot requires either a full copy or a copy-on-write mechanism. With ZeroFS + S3:

- SST files are immutable once written to S3
- A snapshot is a list of SST file keys — tens of bytes per entry
- Creating a snapshot costs one metadata write + Raft commit
- Restoring a snapshot costs one metadata read + one ZeroFS init
- Storage cost: zero additional bytes (SST files are shared until divergence)

## 7. Garbage Collection

### What gets garbage collected

- SST files no longer referenced by any volume or snapshot manifest
- WAL segments older than the oldest active manifest
- Orphaned writes from fenced generations

### GC safety invariant

**An S3 object (SST file) must NOT be deleted if any active volume or snapshot references it in its manifest.**

### GC safety rules

1. **Never delete an SST referenced by ANY manifest** (volume or snapshot)
2. **Never delete during in-flight operations** (snapshot create, restore, compaction)
3. **GC is monotonic**: once an SST is marked unreachable, it stays unreachable (no resurrection)
4. **GC runs on the Raft leader only** (single writer to Raft metadata)
5. **Physical deletion is asynchronous**: mark as deletable in Raft, background worker removes from S3
6. **Two-phase delete**: mark → wait grace period (1 hour) → delete. This protects against Raft metadata lag.

### Reference counting

Implementation:
- Maintain a reference count per SST file in Raft metadata
- When a volume/snapshot is created: increment refcounts for all referenced SSTs
- When a volume/snapshot is deleted: decrement refcounts
- GC runs periodically: delete SSTs where refcount == 0 AND no in-flight operations reference them
- GC is ALWAYS safe to delay. Never safe to run early.
- Raft metadata is the source of truth for refcounts, not S3 object listing

### Reachability graph

```
Volume A (manifest) → [sst-001, sst-002, sst-003]
Snapshot X (manifest) → [sst-001, sst-002]  (shared with Volume A)
Volume B (restored from X) → [sst-001, sst-002, sst-004]  (shared SSTs + new)

Reachable: sst-001 (refcount 3), sst-002 (refcount 3), sst-003 (refcount 1), sst-004 (refcount 1)

Delete Snapshot X:
  sst-001 refcount 3→2, sst-002 refcount 3→2
  No SSTs become unreachable.

Delete Volume A:
  sst-001 refcount 2→1, sst-002 refcount 2→1, sst-003 refcount 1→0
  sst-003 is now unreachable → mark for GC

GC runs: delete sst-003 from S3 (after grace period)
```

### GC object matrix

| Object type | Referenced by | Retention rule | GC authority |
|-------------|--------------|----------------|-------------|
| Manifest | Volume metadata in Raft, snapshot records | Keep until no volume or snapshot references this generation+version | Raft leader |
| WAL segment | Active generation (for crash replay), snapshot wal_position | Keep all segments from the oldest snapshot wal_position to the current WAL head. Segments before the oldest reference point are deletable. | Raft leader |
| SST file | Manifest sst_files list (any active manifest — volume or snapshot) | Refcount in Raft. Delete when refcount == 0 AND grace period elapsed. | Raft leader |
| Orphaned generation objects | No active manifest references them | Delete after grace period (1 hour). Identified by generation < current active generation AND not in any manifest. | GC worker |

### WAL retention invariant

A WAL segment S must be retained if ANY of these are true:
- S is needed for crash recovery of the current active volume (S >= last compaction checkpoint)
- S is referenced by a snapshot's wal_position (S contains or follows that position)
- S belongs to a generation that is still active

A WAL segment is safe to delete only when ALL conditions are false.

### Snapshot WAL dependency

When a snapshot is created at wal_position P:
- All WAL segments containing writes up to position P must be retained
- These segments are added to the snapshot's dependency list in Raft
- When the snapshot is deleted, the dependency is removed
- GC recalculates the minimum retained WAL position across all snapshots

## 8. Attachment model (single-writer, hot-plug)

### Single-writer invariant

A volume can be attached to exactly one VM at a time. This is enforced by the Raft state machine:

- `AttachVolume { volume_id, vm_id }` checks that `volume.attached_to` is `None`
- If `attached_to` is `Some(other_vm_id)`, the command is rejected with `VolumeAlreadyAttached`
- There is no multi-attach mode in v1 (no shared filesystems, no clustered databases)

This is a correctness invariant, not a limitation. Block devices are single-writer by design. Multi-attach requires a cluster-aware filesystem (GFS2, OCFS2) on top — complexity that is out of scope.

### Attach flow

```
Control plane (Raft leader)           Forge (target hypervisor)
─────────────────────────────         ────────────────────────────

1. Validate: volume is Available,
   VM exists and is on this HV
2. Commit AttachVolume to Raft
   (volume.desired → AttachedTo(vm, hv),
    volume.placement_generation++)
                                      3. Reconciler sees: volume should
                                         be Attached on this HV
                                      4. ZeroFS: create NBD device
                                         pointing to s3://{bucket}/volumes/{id}/gen-{generation}/
                                      5. NBD device appears as /dev/nbdN
                                      6. Cloud Hypervisor: PUT /vm.add-disk
                                         { path: "/dev/nbdN" }
                                      7. Guest sees new /dev/vdX
                                      8. Report via gossip: volume attached
```

### Detach flow

```
Control plane (Raft leader)           Forge (target hypervisor)
─────────────────────────────         ────────────────────────────

1. Validate: volume is Attached
2. Commit DetachVolume to Raft
   (volume.desired → Available)
                                      3. Cloud Hypervisor: PUT /vm.remove-device
                                         (guest loses /dev/vdX)
                                      4. ZeroFS: flush cache to S3
                                         (all dirty data written)
                                      5. ZeroFS: disconnect NBD device
                                      6. Report: detach complete
7. Observed state converges to
   Available (via reconciliation)
```

### Detach contract (precise)

When `detach` returns success, the following are guaranteed:
- All writes that the guest fsynced via the block device are persisted to S3 WAL
- ZeroFS dirty cache has been flushed to S3
- The NBD device is disconnected
- The Cloud Hypervisor virtio-block device is removed

The following are NOT guaranteed:
- Dirty pages in the guest's page cache that were never fsynced are LOST
- Filesystem metadata that the guest did not flush is LOST
- If the guest had a mounted filesystem and did not unmount/sync before detach, the filesystem may be inconsistent (same as pulling a USB drive)

### Operator/tenant responsibility

Before detaching a volume from a running VM:
1. Inside the guest: `sync && umount /mnt/data` (or `fsfreeze --freeze`)
2. Then: `syfrah volume detach <volume>`

Syfrah does NOT automatically sync the guest filesystem. Detach is a host-level operation — it flushes the ZeroFS/NBD layer, not the guest's VFS layer.

### Force detach

`syfrah volume detach --force` skips the flush. Use only when the VM is crashed or unresponsive. Data since last fsync will be lost.

### Root volumes vs. data volumes

| Aspect | Root volume | Data volume |
|---|---|---|
| Creation | Auto-created when VM is created | Created independently via CLI/API |
| Lifecycle | Tied to VM — deleted when VM is deleted | Independent — persists after VM deletion |
| Detach | Only on VM stop/delete | Any time (hot-unplug) |
| Default size | From VM spec (`--disk-size`, default 20GB) | From `volume create --size` |
| Volume type | `VolumeType::Root` | `VolumeType::Data` |

### Hot-plug semantics

Data volumes can be attached and detached while the VM is running. Cloud Hypervisor supports `PUT /vm.add-disk` and `PUT /vm.remove-device` for hot-plug. The guest kernel detects the new block device via virtio PCI hotplug.

Root volumes are attached at VM boot (part of the initial Cloud Hypervisor config) and detached only when the VM stops.

## 9. S3 configuration

```rust
struct StorageConfig {
    /// S3-compatible endpoint URL.
    /// Example: "https://s3.par.io.cloud.ovh.net"
    s3_endpoint: String,

    /// S3 bucket name. All volumes in this region share this bucket.
    s3_bucket: String,

    /// S3 access key.
    s3_access_key: String,

    /// S3 secret key.
    s3_secret_key: String,

    /// Path to the local SSD used for the warm cache.
    /// Example: "/dev/nvme1n1" or "/mnt/cache"
    cache_disk_path: String,

    /// Maximum SSD cache size in gigabytes.
    cache_disk_size_gb: u32,

    /// Maximum memory cache size in gigabytes.
    cache_memory_size_gb: u32,

    /// Encryption passphrase. Used to derive the XChaCha20-Poly1305 key
    /// via Argon2id. All volumes in this region use the same key.
    encryption_passphrase: String,

    /// Region this config belongs to.
    region: String,
}
```

### One config per region

All hypervisors in the same region share the same S3 bucket and encryption passphrase. This means:

- A volume created on hypervisor A in `eu-west` can be attached to hypervisor B in `eu-west` — same S3 bucket, same encryption key.
- A volume cannot be moved across regions without an explicit cross-region copy (different S3 bucket, potentially different encryption key).

### S3 bucket strategies

| Strategy | When to use | Trade-off |
|---|---|---|
| One bucket per region | Multiple hypervisors in the same region share a bucket | Simple, cost-effective, enables cross-HV volume mobility within region |
| One bucket per hypervisor | Maximum isolation | Volumes cannot move between hypervisors without S3-to-S3 copy |
| One bucket for everything | Simplest setup, small deployments | All eggs in one basket, cross-region latency for remote HVs |

The recommended default is **one bucket per region**. It provides the best balance of simplicity, cost, and volume mobility.

### Configuration storage

`StorageConfig` records are stored in Raft (minus the encryption passphrase, which is stored locally on each hypervisor in a file with 0600 permissions — never replicated via Raft). The S3 credentials and endpoint are replicated so that all nodes know which bucket to use for each region. The encryption passphrase is an operator secret that never leaves the node.

## 10. Cache architecture (memory + SSD, LRU, write-back)

### Two-tier cache

```
    ┌─────────────────────────────────────────────┐
    │  Tier 1: Memory (hot)                        │
    │  - Recently written data (memtable)          │
    │  - Frequently read chunks                    │
    │  - Latency: microseconds                     │
    │  - Size: operator-configured (e.g., 8 GB)    │
    ├─────────────────────────────────────────────┤
    │  Tier 2: SSD (warm)                          │
    │  - Larger working set                        │
    │  - Evicted from memory but still hot         │
    │  - Latency: ~100-200 microseconds            │
    │  - Size: operator-configured (e.g., 200 GB)  │
    ├─────────────────────────────────────────────┤
    │  Tier 3: S3 (cold)                           │
    │  - All data (complete dataset)               │
    │  - Fetched on cache miss                     │
    │  - Latency: 10-100 milliseconds              │
    │  - Size: unlimited (pay per GB stored)        │
    └─────────────────────────────────────────────┘
```

### Write path

1. **VM writes to `/dev/vdX`** — virtio-block → NBD device
2. **Memory buffer** — ZeroFS accepts the write into its memtable. Returns immediately. Latency: microseconds.
3. **WAL on S3** — On `fsync`, the memtable is flushed to the Write-Ahead Log on S3. This ensures durability even if the node crashes. Latency: 5-50ms (S3 PUT latency).
4. **SST compaction** — Background process. The WAL and memtable are compacted into immutable SST files and uploaded to S3. The local cache retains a copy.

### Read path

1. **VM reads from `/dev/vdX`** — virtio-block → NBD device
2. **Memory cache hit?** → Return immediately. Latency: microseconds.
3. **SSD cache hit?** → Return from local SSD. Latency: ~100-200μs.
4. **Cache miss** → Fetch chunk from S3, store in SSD cache (and optionally memory), return to caller. Latency: 10-100ms.

### Eviction policy

LRU (Least Recently Used) eviction across both tiers:

- When memory cache is full, least recently used chunks are demoted to SSD cache
- When SSD cache is full, least recently used chunks are evicted entirely (they remain in S3)
- Eviction is per-chunk (256KB granularity)

### Cache sizing recommendations

| Node workload | Recommended SSD cache | Memory cache | Rationale |
|---|---|---|---|
| 5 small VMs (web servers) | 50 GB | 4 GB | Small working sets, mostly reads |
| 3 medium VMs (app + DB) | 200 GB | 8 GB | Database hot pages fit in cache |
| 1 large VM (heavy database) | 500 GB | 16 GB | Large working set, frequent random I/O |

Over-provisioning the cache is cheap (local SSD comes free with the dedicated server) and dramatically improves performance. Under-provisioning causes frequent S3 fetches (10-100ms each) which degrade VM I/O latency.

### Write-back vs. write-through

ZeroFS uses **write-back** caching:

- Writes are acknowledged to the VM as soon as they are in the memory buffer (microseconds)
- Durability is achieved asynchronously via WAL flush to S3 (on fsync) and SST compaction (background)
- This means: a node crash between a write and the next fsync can lose data that the VM believes is written

This is the same trade-off as a local disk with a write cache enabled. Applications that require durability guarantees must call `fsync()` — which triggers ZeroFS to flush the WAL to S3 before returning. See §3 (Durability Contract) for the formal guarantees.

## 11. Volume migration (zero-copy cross-node)

When a VM is rescheduled to a different hypervisor (operator-initiated or automatic on failure), its volumes follow with zero data copy. This is the key advantage of S3-backed storage.

### Migration flow

```
Node A (source)                        Node B (target)
───────────────                        ───────────────

1. Stop VM (Cloud Hypervisor
   shutdown or process killed)
2. ZeroFS flushes cache to S3
   (all dirty data written)
3. Disconnect NBD on Node A
4. Raft commits reschedule:
   volume.hypervisor_id = B,
   volume.placement_generation++
                                       5. Forge reconciler sees:
                                          volume should be on this HV
                                       6. ZeroFS connects NBD
                                          to gen-{N+1}/ prefix,
                                          reads from gen-N/ + gen-{N+1}/
                                       7. /dev/nbdN appears
                                       8. Cloud Hypervisor starts VM
                                          with /dev/nbdN as disk
                                       9. VM boots (~200ms)

Data copied over network: zero
Downtime: 5-30 seconds
Cache state on Node B: cold
```

### Cache warmup after migration

The cache on Node B starts empty. The first reads will hit S3 (10-100ms each). The working set of a typical VM (OS, active database pages, application code) is 1-10GB. At 100MB/s S3 throughput, warming the cache takes:

| Working set | Warmup time (approx.) | Impact |
|---|---|---|
| 1 GB | ~10 seconds | Barely noticeable |
| 5 GB | ~50 seconds | Short degradation |
| 10 GB | ~100 seconds | Noticeable for 1-2 minutes |
| 50 GB | ~500 seconds | Significant — 8+ minutes of degraded I/O |

During warmup, the VM is fully operational. Reads are slower (S3 latency instead of cache latency), but no data is lost and no errors occur. Write performance is unaffected (writes go to the local memory buffer immediately).

### Why this beats traditional migration

| Approach | Data copied | Downtime | Complexity |
|---|---|---|---|
| Syfrah (ZeroFS + S3) | Zero | 5-30 seconds | Low — just reconnect NBD |
| Ceph RBD | Zero (shared storage) | Seconds | Medium — Ceph cluster required |
| Local disk + rsync | Full disk | Minutes to hours | High — full data transfer |
| Live migration (QEMU) | Memory + dirty pages | Milliseconds | High — convergence issues |

## 12. Volume Fencing

### The problem

When a VM is rescheduled, the old hypervisor's ZeroFS may still be writing to S3. The new hypervisor's ZeroFS must not read corrupted/interleaved data.

### Manifest-based fencing via S3 prefix rotation

Each volume attachment uses a unique **S3 write prefix** derived from the placement generation:

```
s3://bucket/volumes/{volume_id}/gen-{generation}/
```

When a volume is attached with generation N, ZeroFS writes to `gen-N/`. When the volume is rescheduled to generation N+1:

1. New writer uses prefix `gen-{N+1}/`
2. New writer reads from BOTH `gen-N/` and `gen-{N+1}/` (merge view)
3. Old writer's prefix `gen-N/` is NEVER written to by the new writer
4. Old writer's writes to `gen-N/` after the reschedule are IGNORED by the new writer (they read from their own manifest)

### Why this is logical fencing, not hard fencing

The old writer CAN still write to S3 (we can't revoke S3 credentials instantly). But:
- The old writer writes to `gen-N/` prefix
- The new writer reads from its OWN manifest which starts at `gen-{N+1}/`
- The old writer's late writes are orphaned — they never appear in any active manifest
- The GC eventually cleans them up

This is **manifest-based fencing**: the commit point is the manifest, not the individual S3 objects. Only the active generation's manifest is authoritative.

> **Important clarification:** This is NOT hard fencing in the traditional sense (physical prevention of writes). The old writer CAN still write to S3. Safety relies on the manifest being the sole authority for data visibility. Late writes by a stale writer are orphaned — never referenced by any active manifest.
>
> True hard fencing (short-lived S3 credentials via STS that expire on detach) is a Phase 5 hardening. Until then, the system relies on manifest-authoritative logical fencing.

### Alternative: short-lived S3 credentials (future)

For true hard fencing, each attachment could use short-lived S3 credentials (STS tokens) that expire on detach. The old writer physically cannot write after credential expiry. This is Phase 5+ hardening.

### Fencing timeline

```
Time    Event                                  Node A          Node B
────    ─────                                  ──────          ──────
t=0     Node A is healthy                      ZeroFS active   -
                                               gen=41
                                               prefix: gen-41/
t=1     Node A becomes unreachable             (unreachable)   -
t=2     Gossip detects failure (~15s)           (unreachable)   -
t=3     Raft reschedules VM                    (unreachable)   -
        DetachVolume(gen=41→42)
        AttachVolume(gen=42, hv=B)
t=4     Node B starts ZeroFS                   (unreachable)   ZeroFS active
                                                               gen=42
                                                               prefix: gen-42/
                                                               reads: gen-41/ + gen-42/
t=5     Node A recovers                        Forge reads     ZeroFS active
                                               Raft: gen=42,   gen=42
                                               hv=B
                                               local gen=41
                                               → FENCED
                                               → stop ZeroFS
                                               → discard cache
                                               Late writes to
                                               gen-41/ are
                                               orphaned
```

## 12b. Manifest Commit Protocol

The manifest is the commit point for all volume state. It defines which S3 objects constitute the current state of a volume. Without a valid manifest, data objects in S3 are meaningless.

### Manifest structure

```rust
VolumeManifest {
    volume_id: VolumeId,
    generation: u64,           // placement generation — monotonically increasing
    manifest_version: u64,     // incremented on every publish — monotonically increasing
    sst_files: Vec<SstRef>,    // list of SST files comprising the volume data
    wal_position: u64,         // WAL offset for crash recovery replay
    created_at: u64,
    published_by: HypervisorId, // which node published this manifest
}
```

### Who can publish

Only the hypervisor currently assigned in the Raft placement record (matching generation) can publish a manifest. A manifest published by a stale generation is invalid and ignored.

### Publication path

1. ZeroFS completes a compaction (or snapshot request)
2. ZeroFS writes the new manifest object to S3: `s3://bucket/volumes/{id}/gen-{N}/manifest-{version}.json`
3. ZeroFS atomically updates the manifest pointer in its local state
4. The manifest pointer (volume_id, generation, manifest_version) is committed to Raft

### Manifest pointer in Raft

The Raft state machine stores the authoritative manifest pointer:

```rust
ManifestPointer {
    volume_id: VolumeId,
    generation: u64,
    manifest_version: u64,
    s3_key: String,
}
```

Only the Raft leader can commit a manifest pointer update. This prevents split-brain manifest publication.

### Concurrency rules

| Operation A | Operation B | Allowed concurrently? | Resolution |
|-------------|-------------|----------------------|------------|
| Compaction | Write (WAL) | Yes | Compaction reads immutable SSTs, writes don't touch SSTs |
| Compaction | Snapshot create | No | Snapshot pauses compaction, takes manifest at stable point |
| Snapshot create | Write (WAL) | Yes | Snapshot captures WAL position, writes continue after |
| Manifest publish | Manifest publish | No | Serialized through Raft — only one manifest pointer update at a time |
| GC | Compaction | Safe | GC never deletes SSTs referenced by any manifest (refcount > 0) |
| GC | Snapshot create | Safe | GC respects snapshot manifest references |

### Validation

A manifest is valid if and only if:
1. Its generation matches the current Raft placement generation for this volume
2. Its manifest_version is >= the last committed manifest_version in Raft
3. All SST files listed in the manifest exist in S3
4. The WAL position is reachable (WAL segments up to that position exist)

A stale writer publishing manifest version M for generation N, when generation N+1 is active, produces a manifest that will never be committed to Raft (the leader rejects generation mismatches). The manifest object exists in S3 but is orphaned.

## 13. ZeroFS Dependency Boundary

### What Syfrah guarantees (control plane)

- Volume CRUD lifecycle (Raft)
- Attachment ownership (single-writer invariant)
- Fencing (generation-based prefix isolation)
- Snapshot manifest management (refcounting, GC)
- API contract (CLI, REST, gRPC)

### What Syfrah assumes ZeroFS provides (data plane)

- WAL correctness (fsync → WAL → durable)
- Cache coherency (memory + SSD layers consistent)
- NBD protocol correctness (block device semantics)
- Compaction safety (SST files immutable after write)
- Crash recovery (WAL replay produces consistent state)
- Encryption correctness (XChaCha20-Poly1305)

### What must be validated before GA

- Power loss tests (kill -9 during write + fsync)
- S3 outage simulation (network partition to S3)
- Concurrent attach/detach under load
- Snapshot/restore/delete churn
- Long-running compaction stress
- Cache overflow behavior
- Multi-generation prefix isolation

### GA condition

**This ADR's acceptance as a production storage design is conditional on successful completion of the validation plan (section 27).** If ZeroFS fails to satisfy the assumed invariants under stress testing, the storage architecture must be reworked — potentially with a different storage engine or additional safety layers.

No GA release of Syfrah storage will be made before:
1. All correctness tests in the validation plan pass
2. Power loss tests confirm WAL replay correctness
3. Fencing tests confirm manifest isolation under concurrent writers
4. GC tests confirm no premature deletion under snapshot/restore/delete churn
5. Performance benchmarks validate expected latency ranges

This is a non-negotiable gate. The storage layer handles customer data — it must be proven correct, not assumed correct.

## 14. ZeroFS integration (NBD, SlateDB, chunks, encryption)

### ZeroFS binary management

ZeroFS is bundled with Syfrah releases, similar to Cloud Hypervisor (compute README, §Embedded binary). The release tarball includes:

```
syfrah-v1.0.0-x86_64-linux-musl.tar.gz
    syfrah
    cloud-hypervisor
    zerofs                          ← ZeroFS binary
    install.sh
```

Installed to `/usr/local/lib/syfrah/zerofs`. Compute looks for it in the same resolution order as Cloud Hypervisor.

### NBD device lifecycle

Each volume gets one NBD device on the hypervisor where it is attached.

```
Volume vol-abc123 (100GB)
    │
    ▼
ZeroFS instance
    │
    ├── NBD server: listening on /dev/nbd0
    ├── Cache: memory (2GB) + SSD (/mnt/cache/vol-abc123/)
    ├── SlateDB engine: s3://bucket/volumes/vol-abc123/gen-{N}/
    └── Encryption: XChaCha20-Poly1305 (region key)
    │
    ▼
Cloud Hypervisor
    │
    ├── virtio-block: path_on_host = "/dev/nbd0"
    └── rate_limiter: { bw: 200MB/s, ops: 10000 IOPS }
    │
    ▼
Guest VM sees /dev/vda (100GB block device)
```

### Encryption

All data written to S3 is encrypted before it leaves the hypervisor:

- **Algorithm**: XChaCha20-Poly1305 (authenticated encryption with associated data)
- **Key derivation**: Argon2id from the operator-configured passphrase
- **Scope**: every chunk written to S3 is encrypted. The S3 provider sees only opaque blobs.
- **Granularity**: per-region key in v1. Per-volume keys are a future enhancement.

The operator controls the encryption key. Even if the S3 provider is compromised, the data is unreadable without the passphrase.

### Chunk layout in S3

```
s3://bucket/volumes/{vol-id}/gen-{generation}/
    ├── manifest.json               # Current state: active SST files, WAL position
    ├── wal/
    │   ├── 000001.wal              # Write-ahead log segments
    │   ├── 000002.wal
    │   └── ...
    ├── sst/
    │   ├── L0-000001.sst           # Level 0 SST files (recent)
    │   ├── L1-000002.sst           # Level 1 SST files (compacted)
    │   └── ...
    └── snapshots/
        └── snap-{id}/
            └── manifest.json       # SST file list for this snapshot
```

Each SST file contains multiple 256KB chunks, compressed with LZ4 and encrypted with XChaCha20-Poly1305. SST files are immutable once written — they are never modified, only replaced during compaction.

## 15. Cloud Hypervisor integration (virtio-block, rate limiting)

### Block device attachment

Cloud Hypervisor exposes block devices to VMs via `virtio-block`. The NBD device from ZeroFS is passed as the `path` parameter:

```json
{
    "path": "/dev/nbd0",
    "readonly": false,
    "rate_limiter_config": {
        "bandwidth": {
            "size": 209715200,
            "one_time_burst": 0,
            "refill_time": 1000
        },
        "ops": {
            "size": 10000,
            "one_time_burst": 0,
            "refill_time": 1000
        }
    }
}
```

### Rate limiting

Per-volume rate limiting is applied at the `virtio-block` layer (Cloud Hypervisor's built-in token bucket), BEFORE the NBD device. This means:

- Rate limits are enforced regardless of whether data comes from cache or S3
- The VM cannot exceed its allocated bandwidth/IOPS even if all data is cached
- Rate limits are set at attach time and can be updated via Cloud Hypervisor's API

Default rate limits (configurable per VM tier):

| Tier | Bandwidth | IOPS |
|---|---|---|
| Standard | 200 MB/s | 10,000 |
| Performance | 500 MB/s | 25,000 |
| High I/O | 1 GB/s | 50,000 |

### Hot-plug and hot-unplug

- **Attach**: `PUT /vm.add-disk` with the NBD device path. Guest kernel detects new PCI device.
- **Detach**: `PUT /vm.remove-device` with the device ID. Guest kernel removes the block device. The guest should unmount first — removing a mounted device causes I/O errors in the guest (expected behavior, same as unplugging a physical disk).

## 16. Integration with Raft (metadata in Raft, data in S3)

Volume metadata goes through Raft for consistency. Volume data goes directly to S3 (not through Raft — the data volume would overwhelm the consensus protocol).

### What Raft manages

| Resource | Raft commands | Why in Raft |
|---|---|---|
| Volume CRUD | `CreateVolume`, `DeleteVolume`, `ResizeVolume` | Must be consistent — no duplicate IDs, no double-create |
| Attachment state | `AttachVolume`, `DetachVolume` | Single-writer invariant — Raft enforces exclusive attachment |
| Snapshot records | `CreateSnapshot`, `DeleteSnapshot`, `RestoreSnapshot` | Must be consistent — snapshot IDs unique, source volume valid |
| SST refcounts | Updated on snapshot/volume create/delete | GC safety — Raft is source of truth for reachability |
| Storage config | `SetStorageConfig` | Per-region S3 config, replicated to all nodes |
| Storage quotas | `SetStorageQuota`, `CheckQuota` | Per-org/project limits, enforced at commit time |
| Placement generation | Incremented on `AttachVolume`, `DetachVolume`, reschedule | Fencing (see §12) |

### What ZeroFS manages (locally, not through Raft)

| Concern | Managed by | Why local |
|---|---|---|
| NBD device lifecycle | ZeroFS on the hypervisor | Runtime artifact — only relevant on the node where the volume is attached |
| Cache contents | ZeroFS on the hypervisor | Ephemeral — rebuilt on demand from S3 |
| S3 read/write operations | ZeroFS on the hypervisor | Data plane — too much throughput for consensus |
| Chunk management (split, compress, encrypt) | ZeroFS on the hypervisor | Implementation detail of the storage engine |
| WAL flush and SST compaction | ZeroFS on the hypervisor | Background maintenance, local to each volume instance |

### Raft commands for storage

```rust
enum StorageCommand {
    CreateVolume {
        id: VolumeId,
        name: String,
        size_gb: u32,
        org_id: OrgId,
        project_id: ProjectId,
        env_id: EnvId,
        volume_type: VolumeType,
    },
    DeleteVolume {
        volume_id: VolumeId,
    },
    AttachVolume {
        volume_id: VolumeId,
        vm_id: VmId,
        hypervisor_id: HypervisorId,
    },
    DetachVolume {
        volume_id: VolumeId,
    },
    ResizeVolume {
        volume_id: VolumeId,
        new_size_gb: u32,
    },
    CreateSnapshot {
        id: SnapshotId,
        source_volume_id: VolumeId,
        sst_files: Vec<String>,
        wal_position: u64,
    },
    DeleteSnapshot {
        snapshot_id: SnapshotId,
    },
    RestoreSnapshot {
        snapshot_id: SnapshotId,
        new_volume_id: VolumeId,
        new_volume_name: String,
    },
    SetStorageConfig {
        region: String,
        config: StorageConfig,
    },
    SetStorageQuota {
        scope: QuotaScope,  // Org or Project
        max_volumes: u32,
        max_total_gb: u64,
        max_snapshots: u32,
    },
}
```

Each command is validated by the Raft state machine before committing:

- `CreateVolume`: check quota, check name uniqueness within environment
- `AttachVolume`: check volume is `Available`, check VM exists and is on the target HV, increment `placement_generation`
- `DetachVolume`: check volume is `Attached`, set desired state to `Available`
- `DeleteVolume`: check volume is `Available` (not attached), check no in-flight snapshot/restore operations reference it, set desired state to `Deleted`. Physical S3 deletion happens after a tombstone period — Raft marks the intent, Forge executes the cleanup.
- `ResizeVolume`: check volume is `Available`, check new size >= current size (no shrink in v1)
- `CreateSnapshot`: increment refcounts for all referenced SST files
- `DeleteSnapshot`: decrement refcounts for all referenced SST files, check no in-progress restore references this snapshot

## 17. CLI

```bash
# Volume lifecycle
syfrah volume create <name> --size <gb> --project <project> --org <org> [--env <env>]
syfrah volume list [--project <project>] [--org <org>] [--env <env>]
syfrah volume get <name> [--project <project>]
syfrah volume delete <name> [--project <project>]
syfrah volume resize <name> --size <new-gb> [--project <project>]

# Attach/detach
syfrah volume attach <volume> --vm <vm>
syfrah volume detach <volume>

# Snapshots
syfrah volume snapshot create <name> --volume <volume>
syfrah volume snapshot list [--volume <volume>]
syfrah volume snapshot get <name>
syfrah volume snapshot restore <snapshot> --name <new-volume-name>
syfrah volume snapshot delete <name>

# Operator: storage configuration
syfrah storage configure \
    --region <region> \
    --s3-endpoint <url> \
    --s3-bucket <bucket> \
    --s3-access-key <key> \
    --s3-secret-key <key> \
    --cache-disk <path> \
    --cache-disk-size <gb> \
    --cache-memory-size <gb>

syfrah storage status                   # Show S3 connectivity, cache utilization
syfrah storage health                   # S3 latency probe, cache hit rate
```

### Example session

```bash
# Operator configures storage for the eu-west region
syfrah storage configure \
    --region eu-west \
    --s3-endpoint https://s3.par.io.cloud.ovh.net \
    --s3-bucket syfrah-storage-eu-west \
    --s3-access-key XXXX \
    --s3-secret-key XXXX \
    --cache-disk /dev/nvme1n1 \
    --cache-disk-size 200 \
    --cache-memory-size 8

# Tenant creates a VM with a root volume (auto-created)
syfrah vm create web-1 --image ubuntu-24.10 --vcpu 2 --memory 4096 \
    --disk-size 50 --project myapp --org acme

# Tenant creates a data volume
syfrah volume create pgdata --size 100 --project myapp --org acme

# Attach to the VM
syfrah volume attach pgdata --vm web-1

# Take a snapshot before an upgrade
syfrah volume snapshot create pgdata-pre-upgrade --volume pgdata

# Something goes wrong — restore from snapshot
syfrah volume detach pgdata
syfrah volume snapshot restore pgdata-pre-upgrade --name pgdata-restored
syfrah volume attach pgdata-restored --vm web-1
```

## 18. Forge integration

Forge on each hypervisor manages local ZeroFS through the `VolumeMgr` subsystem (defined in ADR-003).

### VolumeMgr responsibilities

| Responsibility | How |
|---|---|
| Start ZeroFS for a volume | Exec `zerofs` with S3 config, cache config, encryption passphrase, placement generation |
| Stop ZeroFS for a volume | Graceful shutdown: flush cache → disconnect NBD → terminate process |
| Create NBD device | ZeroFS creates `/dev/nbdN` on startup |
| Destroy NBD device | ZeroFS removes the device on shutdown |
| Attach to Cloud Hypervisor | Call compute layer's `attach_disk()` with the NBD device path |
| Detach from Cloud Hypervisor | Call compute layer's `detach_device()` with the device ID |
| Monitor cache utilization | Read ZeroFS metrics (cache hit rate, dirty bytes, S3 latency) |
| Report storage capacity | Publish via gossip: cache used/total, volumes attached, S3 health |
| Fencing | Compare local placement_generation with Raft — stop ZeroFS if stale |

### Reconciliation loop

On every reconciliation cycle, Forge's storage reconciler:

1. **Read desired state**: list volumes assigned to this hypervisor from the Raft materialized view (redb)
2. **Read actual state**: list active ZeroFS instances and NBD devices on this hypervisor
3. **Compute diff**:
   - Volume in desired but not actual → start ZeroFS, create NBD, attach to VM
   - Volume in actual but not desired → stop ZeroFS, destroy NBD (fencing case)
   - Volume in both but generation mismatch → fence (stop ZeroFS, discard cache)
   - Volume in both and matching → no-op
4. **Apply changes**: execute the diff, one volume at a time, in dependency order

### Resource creation order (for VM with volumes)

```
1. Network (TAP device, bridge, VXLAN, FDB)
2. Storage (ZeroFS start, NBD device, attach to CH)
3. Compute (Cloud Hypervisor start, VM boot)
```

### Resource deletion order (reverse)

```
1. Compute (VM shutdown, CH process stop)
2. Storage (detach from CH, flush cache, stop ZeroFS)
3. Network (remove TAP, nftables, FDB)
```

## 19. Performance Expectations

These are **design targets, not SLA commitments**. Actual performance depends on workload, cache hit rate, S3 endpoint latency, and local hardware. Validation benchmarks are required before GA.

### Latency

| Operation | Expected range | Bottleneck | Benchmark required |
|-----------|---------------|------------|-------------------|
| Hot read (memory cache) | 1-10 us | Memory bandwidth | Yes |
| Warm read (SSD cache) | 50-200 us | NVMe/SSD latency | Yes |
| Cold read (S3 fetch) | 10-100 ms | S3 GET latency + network | Yes |
| Write (buffered, no fsync) | 1-10 us | Memory bandwidth | Yes |
| Write (fsync to WAL on S3) | 5-50 ms | S3 PUT latency | Yes |
| Snapshot create (metadata) | < 1s | Raft commit | Yes |
| Snapshot restore | 1-3s | Manifest read + ZeroFS init | Yes |
| Volume create | 2-5s | S3 init + NBD setup | Yes |
| Volume attach (hot-plug) | 1-3s | NBD connect + CH add-disk | Yes |
| Volume detach | 2-10s | Cache flush + NBD disconnect | Yes |

### Throughput

| Scenario | Expected range | Bottleneck | Benchmark required |
|----------|---------------|------------|-------------------|
| Sequential read (cached) | 2-7 GB/s | Local NVMe bandwidth | Yes |
| Sequential read (cold) | 100-500 MB/s | S3 bandwidth (parallel GETs) | Yes |
| Sequential write (buffered) | Memory speed | Memory bandwidth | Yes |
| Sequential write (fsync) | 50-200 MB/s | S3 PUT throughput | Yes |
| Random 4K read (cached) | 100K-500K IOPS | NVMe random read | Yes |
| Random 4K read (cold) | 100-1000 IOPS | S3 GET per chunk | Yes |

All numbers are subject to validation. They are not SLA figures and must not be marketed as guarantees.

### Cache hit rate expectations

For most VM workloads, >95% of I/O hits the local cache. The working set of a typical VM (OS, active database pages, application code) fits in the SSD cache. S3 latency only matters for cold reads (first access after migration, or access to rarely-used data).

Workloads with very large working sets (data warehouses, large media processing) will have lower cache hit rates and experience more S3 latency. These workloads should use hypervisors with larger SSD cache allocations.

## 20. Supported Workloads

### Well-suited (validated targets for v1)

| Workload | Why it works |
|----------|-------------|
| Web servers, APIs | Mostly read-heavy, working set fits in cache, infrequent fsync |
| Dev/test environments | Durability loss on crash is acceptable |
| CI/CD runners | Ephemeral, no fsync expectations |
| Root disks | OS data, infrequent writes, cache-friendly |
| Application data (moderate write) | Write-back semantics acceptable if app handles its own consistency |

### Not recommended (v1 limitations)

| Workload | Why it's risky |
|----------|---------------|
| Transactional databases (Postgres, MySQL) under heavy load | Frequent fsync → high S3 WAL latency (5-50ms per fsync) |
| Write-ahead-log-heavy systems (etcd, Kafka, Raft stores) | fsync-per-write kills throughput |
| High-frequency trading / ultra-low-latency | S3 tail latency unpredictable |
| Large random read workloads (cold data) | Cache misses → S3 latency (10-100ms) |

### Honest positioning

"Syfrah volumes are NOT equivalent to local NVMe or synchronous replicated block storage. They are designed for workloads where operator simplicity outweighs raw storage performance. For latency-critical databases, use dedicated database nodes with local storage or a managed database product."

## 21. Capacity management (local cache + S3 quotas)

### Local cache capacity

Each hypervisor has a finite SSD cache. Forge tracks and reports:

- `cache_total_gb`: total SSD allocated for cache
- `cache_used_gb`: currently used by all volumes' cached chunks
- `cache_hit_rate`: rolling 5-minute average (reported via gossip)
- `dirty_bytes`: total unflushed bytes across all volumes (reported via gossip)

The scheduler does NOT use cache capacity for placement decisions in v1. Volumes are S3-backed and can be attached to any hypervisor in the region. However, cache pressure and dirty bytes should be surfaced to the operator, and soft storage signals (cache pressure, dirty byte ratio) should be considered as scheduler hints in a future release (see §28, Future Work).

### S3 storage quotas

Quotas are enforced at the Raft level (before commit):

| Scope | Quota fields | Default |
|---|---|---|
| Organization | `max_total_gb`, `max_volumes`, `max_snapshots` | Unlimited (operator sets) |
| Project | `max_total_gb`, `max_volumes`, `max_snapshots` | Inherits from org |

The state machine sums all volume sizes and counts within the scope before committing a `CreateVolume` or `CreateSnapshot`. If the quota would be exceeded, the command is rejected.

## 22. Operator setup

### Initial setup (per region)

```bash
# 1. Create an S3 bucket with the provider
#    (e.g., via OVH console, Hetzner CLI, Scaleway console)

# 2. Configure Syfrah to use this bucket
syfrah storage configure \
    --region eu-west \
    --s3-endpoint https://s3.par.io.cloud.ovh.net \
    --s3-bucket syfrah-storage-eu-west \
    --s3-access-key AK_XXXXXXXXXXXX \
    --s3-secret-key SK_XXXXXXXXXXXX \
    --cache-disk /dev/nvme1n1 \
    --cache-disk-size 200 \
    --cache-memory-size 8

# 3. Verify connectivity
syfrah storage health
# Storage Health
#   S3 endpoint:    https://s3.par.io.cloud.ovh.net (reachable)
#   S3 bucket:      syfrah-storage-eu-west (accessible)
#   S3 PUT latency: 12ms
#   S3 GET latency: 8ms
#   Cache disk:     /dev/nvme1n1 (200 GB available)
#   Cache memory:   8 GB allocated
```

### Per-hypervisor overrides

Cache sizing can be overridden per hypervisor (some servers have more SSD than others):

```bash
syfrah storage configure \
    --region eu-west \
    --cache-disk /dev/nvme1n1 \
    --cache-disk-size 500 \
    --cache-memory-size 16
```

S3 endpoint and bucket are per-region (shared by all hypervisors). Cache config is per-hypervisor.

## 23. Deletion guards

### Volume deletion guards

A volume cannot be deleted if:

1. **It is attached** — must detach first. No "force delete while attached."
2. **It has snapshots** — must delete all snapshots first (or use `--cascade` to delete snapshots too).
3. **It is a root volume for a running VM** — must stop and delete the VM first.
4. **Deletion protection is enabled** — must explicitly remove protection first.
5. **A snapshot create or restore is in-flight** — must wait for the operation to complete. Deletion while a snapshot references this volume in an active operation could violate GC refcount invariants.

```bash
# Enable deletion protection
syfrah volume update pgdata --deletion-protection

# Attempt delete → rejected
syfrah volume delete pgdata
# Error: volume "pgdata" has deletion protection enabled.
# Run: syfrah volume update pgdata --no-deletion-protection

# Cascade delete (volume + all snapshots)
syfrah volume delete pgdata --cascade
```

### Snapshot deletion guards

A snapshot cannot be deleted if:

1. **It is the source for an in-progress restore** — wait for restore to complete
2. **Deletion protection is inherited from the volume** — remove volume deletion protection first

### Volume deletion protocol (tombstone before physical delete)

When a volume is deleted:
1. Raft marks the volume's desired state as `Deleted` (tombstone). The volume record remains in Raft with this tombstone for a grace period.
2. SST refcounts for the volume's manifest are decremented.
3. Forge on the last known hypervisor (or any hypervisor in the region) deletes S3 objects under the volume's prefix — but only after GC confirms no other volume/snapshot references those SSTs.
4. `Deleted` volume records are retained for 30 days (audit trail), then garbage-collected from Raft.

Physical S3 object deletion NEVER happens before the Raft tombstone is written and refcounts are updated. This ensures that a crash during deletion does not leave orphaned references.

## 24. Security (encryption at rest, key management)

### Encryption at rest

All volume data is encrypted before leaving the hypervisor. The S3 provider never sees plaintext data.

| Property | Value |
|---|---|
| Algorithm | XChaCha20-Poly1305 |
| Key derivation | Argon2id from operator passphrase |
| Key scope | Per-region (v1) |
| Encrypted | Every 256KB chunk written to S3 |
| Not encrypted | Volume metadata in Raft (names, sizes, states) |

### Key management

- **v1**: one passphrase per region, stored locally on each hypervisor (`/etc/syfrah/storage-key`, mode 0600). Not replicated via Raft. Operator must distribute the same passphrase to all hypervisors in a region.
- **Future**: per-volume keys, key rotation, integration with external KMS (HashiCorp Vault, AWS KMS).

### Threat model

| Threat | Mitigation |
|---|---|
| S3 provider reads data | All data encrypted with operator-controlled key |
| S3 credentials leaked | Operator rotates S3 keys; data still encrypted |
| Hypervisor compromised (root access) | Attacker can read decrypted cache; no protection against root compromise (standard threat model) |
| Network interception (S3 traffic) | HTTPS to S3 endpoint + data encrypted at rest |
| Rogue hypervisor joins mesh | Fabric peering requires operator approval; rogue node cannot join without explicit PIN acceptance |

## 25. Failure scenarios — contractual invariants

### S3 outage

| Duration | Impact | Invariant |
|----------|--------|-----------|
| < 30s | fsync blocks, guest may see latency spike | No data loss. Fsynced writes queued in memory. All previously fsynced data safe in S3. |
| 30s - 5min | fsync returns EIO. Guest sees write errors. | Fsynced-before-outage data is safe. In-flight fsyncs may fail — guest receives EIO and must retry. |
| > 5min | Volume transitions to Degraded. New writes rejected. | Cache dirty data preserved locally. Recovery on S3 return. No data loss if hypervisor stays up. |
| > 30min | Volume transitions to Error. Cache may overflow. | Best-effort preservation. Operator intervention required. |

These thresholds are default operational policy values, configurable per-deployment. They are not inherent system properties — they depend on buffer sizes, retry budgets, and memory pressure. The actual behavior under S3 outage depends on the ZeroFS cache capacity and the guest's write rate.

**Invariant: data that was fsynced and acknowledged BEFORE the outage is NEVER lost, regardless of outage duration.** It already reached S3 before the outage began.

### Cache disk full

1. LRU eviction frees space by removing cold chunks
2. If eviction cannot free enough space (all chunks are dirty/in-use): new cache writes spill directly to S3 (bypassing SSD cache)
3. Performance degrades (more S3 reads) but no data loss
4. Forge reports cache pressure via gossip metrics
5. Operator should increase cache allocation or add more SSD

### Node crash (unclean shutdown)

**Invariant**: fsynced data is recoverable. Non-fsynced data is lost.

1. ZeroFS processes die with the node
2. Data in memory buffer that was not fsync'd to S3 WAL is lost (same as local disk power loss)
3. Data in S3 (WAL + SST files) is durable and intact
4. On recovery: ZeroFS replays WAL from S3, reconstructs state up to the last successful fsync
5. If the VM was rescheduled during the outage: fencing prevents the old node from touching the volume (§12)

### NBD device failure

1. Cloud Hypervisor reports I/O errors to the guest
2. Guest VM sees disk errors (EIO)
3. Forge detects the failure and attempts to restart the ZeroFS instance
4. If restart succeeds: NBD reconnects, guest retries I/O
5. If restart fails: volume enters `Error` state, operator alerted

### S3 data corruption

1. ZeroFS detects corruption via XChaCha20-Poly1305 authentication tags — corrupted chunks fail decryption
2. The corrupted chunk is discarded from cache
3. ZeroFS attempts to re-read from S3 (may get a valid copy from a different S3 replica)
4. If corruption persists: I/O error returned to guest, volume enters `Error` state
5. Recovery: restore from the most recent snapshot

## 26. Limitations

These are deliberate choices for v1, not missing features:

| Limitation | Impact | Rationale |
|---|---|---|
| **Single-writer only** | No shared filesystems, no clustered databases | Block devices are inherently single-writer. Multi-attach requires cluster-aware FS. |
| **Cache cold after migration** | 1-10 minutes of degraded read performance | Zero-copy migration trade-off. Data is correct immediately; performance recovers gradually. |
| **S3 outage → I/O stalls** | Extended S3 outage blocks write-heavy VMs | Accepted trade-off for not running a storage cluster. Provider S3 has better uptime than self-managed Ceph. |
| **No live resize** | Must detach → resize → reattach | Live resize requires complex filesystem cooperation. Not worth the complexity in v1. |
| **Per-region encryption key (v1)** | All volumes in a region share one key | Simplicity. Per-volume keys add key management complexity. |
| **No multi-attach** | Cannot share a volume between VMs | Requires cluster-aware filesystem on top. Out of scope. |
| **No cross-region volume move** | Volume stays in its creation region | Cross-region requires S3-to-S3 copy + re-encryption. Future enhancement. |
| **No volume shrink** | Can only grow, not shrink | Shrinking requires data relocation + filesystem cooperation. Not worth the risk. |
| **fsync latency = S3 PUT latency** | 5-50ms per fsync | Inherent to S3-backed WAL. Applications that fsync frequently will feel this. |
| **Crash-consistent snapshots only** | No application-consistent snapshots without guest cooperation | App-consistent requires guest agent or fsfreeze. Tenant responsibility. |

## 27. Validation Plan

The following tests MUST pass before GA. No exceptions.

### Correctness tests

| Test | Method | Pass criteria |
|------|--------|--------------|
| Power loss during write | kill -9 ZeroFS during active writes | WAL replay recovers all fsynced data, no corruption |
| Power loss during fsync | kill -9 during S3 PUT (WAL flush) | Either the fsync completed (data durable) or it did not (data lost, no corruption) |
| S3 outage simulation | Network partition to S3 endpoint | fsync blocks or returns EIO. No data corruption. Recovery on reconnect. |
| Concurrent attach/detach | Rapid attach/detach cycles under load | No state machine violations. No orphaned NBD devices. |
| Fencing correctness | Old writer attempts S3 writes after reschedule | Old writes are orphaned in gen-N/. New writer never sees them. |
| Multi-generation isolation | Multiple reschedules in sequence | Each generation's data is isolated. Merge reads are correct. |
| Snapshot/restore/delete churn | Create, restore, delete snapshots rapidly | Refcounts are correct. No SSTs deleted while referenced. |
| GC safety | Delete volumes and snapshots, run GC | Only unreferenced SSTs are deleted. Referenced SSTs survive. |

### Performance benchmarks

| Benchmark | Tool | What to measure |
|-----------|------|----------------|
| Hot read latency | fio (randread, 4K) | p50, p99, p999 latency with warm cache |
| Cold read latency | fio after cache flush | p50, p99 latency hitting S3 |
| Buffered write latency | fio (randwrite, 4K, no fsync) | p50, p99 latency |
| fsync latency | fio (randwrite, 4K, fsync=1) | p50, p99 latency |
| Sequential throughput | fio (read/write, 1M, queue depth 32) | MB/s cached vs cold |
| Cache hit rate under load | Custom workload (90/10 read/write) | Hit rate over time |

### Stress tests

| Test | Duration | What to verify |
|------|----------|---------------|
| Long-running compaction | 24h continuous write | No SST corruption, no memory leak, no cache bloat |
| Cache overflow | Write more data than cache capacity | LRU eviction works, no OOM, performance degrades gracefully |
| S3 latency spike | Inject 500ms latency on S3 | fsync latency increases but no timeouts, no corruption |
| Rapid migration | Reschedule VM every 60s for 1h | Fencing works every time, no data loss, no stale writes |

## 28. Future work

Items explicitly deferred from v1, tracked for future phases:

- **Scheduler storage signals**: soft hints based on cache pressure and dirty byte ratio for smarter VM placement. The scheduler should prefer hypervisors with lower cache pressure when all other factors are equal.
- **Short-lived S3 credentials (STS)**: per-attachment credentials that expire on detach for physically hard fencing.
- **Per-volume encryption keys**: separate key per volume, key rotation support, external KMS integration.
- **Application-consistent snapshots**: guest agent integration for fsfreeze/thaw automation.
- **Cross-region volume copy**: S3-to-S3 copy with re-encryption for disaster recovery.
- **Live resize**: online volume expansion without detach (requires guest filesystem cooperation).
- **Cache pre-warming**: predictive cache loading after migration based on historical access patterns.

## 29. Implementation phases

### Phase 1 — Volume CRUD + Raft state (~8 issues)

- Volume and Snapshot types in `syfrah-core`
- `VolumeDesiredState` and `VolumeObservedState` types
- Raft commands: `CreateVolume`, `DeleteVolume`, `ResizeVolume`
- Raft state machine: desired/observed state reconciliation, quota checks
- Volume CLI: `syfrah volume create/list/get/delete`
- S3 configuration: `StorageConfig` type, `SetStorageConfig` command
- Volume state machine enforcement (invalid transition → error)
- redb tables for volumes, snapshots, storage config, SST refcounts
- Unit tests: state machine transitions, quota enforcement, refcount invariants

### Phase 2 — ZeroFS integration (~8 issues)

- ZeroFS binary management (download, version pinning, install)
- `VolumeMgr` in Forge: start/stop ZeroFS processes
- NBD device lifecycle: create, connect, disconnect, destroy
- Cache configuration: pass SSD path, memory limit, disk limit to ZeroFS
- Generation-based S3 prefix (`gen-{N}/`) for all ZeroFS instances
- Volume create flow: Raft commit → Forge reconciler → ZeroFS init → NBD ready
- Volume delete flow: tombstone in Raft → refcount update → Forge stops ZeroFS → deletes S3 objects → Raft marks Deleted
- Storage health check: `syfrah storage health` (S3 latency probe)
- Integration tests: volume create/delete with real ZeroFS + mock S3

### Phase 3 — Compute integration (~6 issues)

- Attach volume to running VM: ZeroFS NBD → Cloud Hypervisor `add-disk`
- Detach volume from running VM: Cloud Hypervisor `remove-device` → ZeroFS flush → NBD disconnect
- Root volume auto-create on `vm create` with `--disk-size`
- Root volume auto-delete on `vm delete`
- `syfrah volume attach/detach` CLI
- Integration tests: VM with attached volume, hot-plug/unplug

### Phase 4 — Snapshots + migration (~6 issues)

- Snapshot create: ZeroFS pause compaction → record manifest → Raft commit with SST refcount increments
- Snapshot restore: create new volume from snapshot SST files, new generation prefix
- Snapshot delete: Raft commit with SST refcount decrements → GC marks unreachable SSTs
- `syfrah volume snapshot create/list/restore/delete` CLI
- Volume migration on VM reschedule: flush → disconnect → reconnect on new HV with new generation prefix
- Logical fencing with write-prefix isolation (generation-based S3 prefix)

### Phase 5 — Production hardening (~4 issues)

- Cache monitoring: hit rate, dirty bytes, eviction rate → gossip metrics
- S3 health checks: periodic latency probe, alert on degradation
- Quota enforcement: per-org and per-project limits
- Graceful degradation on S3 outage: buffer management, backpressure to VMs
- GC implementation: two-phase delete, grace period, orphan cleanup
- Validation plan execution (see §27)

**Total: ~32 issues**

## 30. Commercial value

### Why this matters for operators

- **No storage cluster to operate.** The operator does not install, configure, or maintain Ceph, GlusterFS, or any distributed storage system. They rent an S3 bucket and Syfrah does the rest.
- **Cost-effective.** S3 storage costs €0.005-0.01/GB/month. Ceph requires 2-3x raw disk for replication, plus dedicated nodes, plus operational overhead.
- **Zero-copy migration.** Moving a VM to another hypervisor copies zero bytes of volume data. Downtime is 5-30 seconds instead of minutes-to-hours with local disk migration.
- **Snapshots are free.** No data copy, no additional storage (until the volume diverges from the snapshot). Operators can offer snapshots as a standard feature without worrying about storage overhead.

### Why this matters for tenants

- **Persistent storage that survives machine failure.** Fsynced data is durable in S3 even if the hypervisor hardware fails. See §3 for the durability contract.
- **Standard block device interface.** `/dev/vda`, `/dev/vdb` — format with ext4, XFS, or any filesystem. No proprietary API.
- **Fast snapshots.** Create a snapshot in seconds, restore in seconds. Useful for backups before deployments, database point-in-time recovery, cloning environments.
- **Predictable performance for cache-friendly workloads.** With proper cache sizing, >95% of I/O is served from local NVMe at sub-millisecond latency. S3 latency is invisible for typical workloads. See §20 for workload fit guidance.

## 31. Rejected alternatives

### Ceph

Ceph is the industry standard for distributed block storage. It provides RBD (RADOS Block Device) with replication, snapshots, and cloning. However:

- **Minimum 3 nodes for redundancy.** Syfrah must work with 1 node.
- **Operational complexity.** MON, OSD, MDS daemons. Cluster health management. CRUSH map tuning. PG balancing. Scrubbing. Deep-scrubbing. All on rented hardware with no local SRE team.
- **Redundant replication.** S3 already replicates data 3x. Ceph would replicate again 2-3x. Paying for 6-9x raw storage.
- **C++ codebase.** Different language from the rest of Syfrah. Integration complexity.

Ceph is the right choice for operators running on owned hardware with a storage team. It is the wrong choice for Syfrah's target user: a small team renting dedicated servers from a provider.

### GlusterFS

GlusterFS provides distributed file storage (not block storage). It would require an additional layer (file → block) to serve VMs. Same operational complexity concerns as Ceph. Rejected for the same reasons.

### Local disk only (no shared storage)

Using only the hypervisor's local NVMe for VM storage. Simple and fast, but:

- **No migration.** VM is pinned to its hypervisor. If the hypervisor dies, data is lost.
- **No snapshots.** No efficient snapshot mechanism at the block level.
- **No overcommit.** Total storage limited to physical disk on each node.

Local-only is what Syfrah would do without the storage layer. ZeroFS + S3 adds durability and mobility without the complexity of a storage cluster.

### Custom replicated block storage

Building a bespoke distributed block store. Rejected because:

- **Enormous engineering effort.** Consensus, replication, failure detection, rebalancing, scrubbing — years of work.
- **Redundant with S3.** The provider already handles durability.
- **Operational burden.** Another cluster for the operator to manage.

### NFS + local disk

Using NFS to share local disks between nodes. Rejected because:

- **Single point of failure.** NFS server crash = all VMs lose storage.
- **No encryption.** NFS traffic is unencrypted (without Kerberos, which adds complexity).
- **Performance.** NFS adds network latency to every I/O operation, with no local cache to absorb it.

## 32. References

- [ZeroFS](https://github.com/Barre/ZeroFS) — S3-backed block storage engine (Rust)
- [SlateDB](https://github.com/slatedb/slatedb) — LSM-tree engine used by ZeroFS
- [NBD protocol](https://github.com/NetworkBlockDevice/nbd) — Network Block Device specification
- [Cloud Hypervisor disk API](https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/api.md) — virtio-block hot-plug
- [`layers/storage/README.md`](../layers/storage/README.md) — Detailed storage design document
- [`layers/compute/README.md`](../layers/compute/README.md) — Compute layer, virtio-block integration
- [`handbook/adr-003-forge.md`](adr-003-forge.md) — Forge resource orchestrator, VolumeMgr
- [`handbook/adr-004-hypervisor-model.md`](adr-004-hypervisor-model.md) — Hypervisor resource model, disk capacity
- [`handbook/adr-005-control-plane.md`](adr-005-control-plane.md) — Raft state, placement_generation, fencing
- [`handbook/ARCHITECTURE.md`](ARCHITECTURE.md) — Global architecture, storage in the stack
