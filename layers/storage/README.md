# Storage

## The problem

Dedicated servers have local disks — fast but tied to a single machine. If a server dies, the data on its disks is gone. If a VM needs to move to another node, its volumes can't follow.

Cloud providers solve this with distributed storage systems (AWS EBS, GCP Persistent Disks). These systems are complex, require dedicated infrastructure, and typically involve Ceph, GlusterFS, or similar — all heavy to operate.

Syfrah takes a different approach.

## The idea: S3 as the durability layer

Every provider where users rent dedicated servers also offers cheap S3-compatible object storage:

| Provider | S3 Service | Cost |
|---|---|---|
| OVH | Object Storage (S3) | ~€0.01/GB/month |
| Scaleway | Object Storage | ~€0.01/GB/month |
| Hetzner | Object Storage | ~€0.005/GB/month |
| Any | MinIO (self-hosted) | Free (on own hardware) |

Instead of building a distributed block storage system, Syfrah uses the provider's S3 as the **durable backend** for all block storage. Local NVMe/SSD on each node serves as a **high-speed cache**.

```
    ┌──────────────────────────────────────────────────────────┐
    │                        Node                               │
    │                                                           │
    │  ┌─────────┐  ┌─────────┐  ┌─────────┐                   │
    │  │  VM-1   │  │  VM-2   │  │  VM-3   │                   │
    │  │ /dev/vda│  │ /dev/vda│  │ /dev/vda│                   │
    │  └────┬────┘  └────┬────┘  └────┬────┘                   │
    │       │            │            │                         │
    │       └────────────┼────────────┘                         │
    │                    │                                      │
    │              ┌─────┴──────┐                               │
    │              │   ZeroFS   │                               │
    │              │            │                               │
    │              │  NBD block │  ← exposes /dev/nbd* devices  │
    │              │  devices   │     to VMs via Cloud Hypervisor│
    │              │            │                               │
    │              ├────────────┤                               │
    │              │   Cache    │                               │
    │              │  local SSD │  ← hot data, microsecond      │
    │              │  + memory  │     latency                   │
    │              ├────────────┤                               │
    │              │  SlateDB   │  ← LSM-tree engine            │
    │              │  (engine)  │     chunks, compaction,       │
    │              │            │     encryption                │
    │              └─────┬──────┘                               │
    │                    │                                      │
    └────────────────────┼──────────────────────────────────────┘
                         │
                         ▼
                  ┌──────────────┐
                  │  S3 Bucket   │
                  │  (provider)  │
                  │              │
                  │  Durable     │
                  │  storage     │
                  └──────────────┘
```

## ZeroFS

Syfrah uses [ZeroFS](https://github.com/Barre/ZeroFS) as the storage engine. ZeroFS is an open-source Rust project that turns S3-compatible object storage into usable block devices and filesystems.

### How it works

1. ZeroFS runs on each hypervisor node
2. It connects to an S3-compatible bucket configured by the operator
3. Data is split into **256KB chunks**, compressed (LZ4), and encrypted (XChaCha20-Poly1305)
4. Chunks are managed by **SlateDB**, an LSM-tree engine that handles memtables, compaction, and flush to S3
5. A local **cache** (SSD + memory) absorbs reads and writes — hot data never hits S3
6. ZeroFS exposes **NBD (Network Block Device)** endpoints — each volume is a `/dev/nbd*` device

### Why ZeroFS

| Requirement | ZeroFS | Ceph | GlusterFS |
|---|---|---|---|
| S3 as backend | Native | No (needs its own OSDs) | No (needs bricks) |
| Operational complexity | Single binary, no cluster | Complex cluster | Complex cluster |
| Minimum nodes | 1 | 3+ | 3+ |
| Language | Rust | C++ | C |
| Encryption at rest | Always on (XChaCha20-Poly1305) | Optional | Optional |
| Cache layer | Built-in (SSD + memory) | Separate OSD cache | Separate |
| Block device support | NBD | RBD | No (file only) |

The key advantage: **no storage cluster to operate**. The S3 provider handles durability and replication. ZeroFS handles the block device abstraction and caching.

## Volume lifecycle

### Creating a volume

When a tenant creates a volume (via the forge API), ZeroFS creates a sparse file in its NBD directory:

```
    Control Plane                        Forge (Node A)
    ─────────────                        ────────────────

    POST /storage/volumes                ZeroFS:
    {                                      truncate -s 100G .nbd/vol-abc123
      "id": "vol-abc123",                 nbd-client connect vol-abc123
      "size_gb": 100
    }                                    → /dev/nbd0 ready

    POST /compute/vms                    Cloud Hypervisor:
    {                                      attach /dev/nbd0 as /dev/vda
      "id": "vm-xyz",
      "volumes": ["vol-abc123"]
    }                                    → VM boots with 100GB block device
```

Inside the VM, the tenant sees a normal block device (`/dev/vda`) and formats it with ext4, XFS, or whatever they want. They don't know about ZeroFS, S3, or caching.

### How it connects to Cloud Hypervisor

Cloud Hypervisor uses `virtio-block` to expose block devices to VMs. It accepts any block device or file on the host. ZeroFS exposes NBD devices (`/dev/nbd*`). The two connect directly — no custom integration, standard Linux block device semantics:

```
    Cloud Hypervisor             ZeroFS                       S3
    (virtio-block)               (NBD + cache + LSM)          (durable backend)

    block device config          /dev/nbd0                    s3://bucket/vol-abc
    {                                │                            │
      path_on_host: "/dev/nbd0"      ├── memory cache             │
    }                                ├── SSD cache (/dev/nvme1)   │
         │                           └── SlateDB engine ──────────┘
         ▼
    Guest sees /dev/vdb
    (normal block device)
```

Per-drive rate limiting (bandwidth + IOPS via token bucket) is applied at the `virtio-block` layer, **before** the NBD device. This caps VM I/O regardless of whether data comes from cache or S3. See [compute.md](compute.md) for details on rate limiting.

### How data flows

```
    Write path:

    VM writes to /dev/vdb
         │
         ▼
    Cloud Hypervisor virtio-block → rate limit check
         │
         ▼
    ZeroFS NBD receives write
         │
         ├─► Memory buffer (immediate, microseconds)
         │
         ├─► WAL on S3 (on fsync, ~10-50ms)
         │
         └─► Flush to S3 as SST chunks
             (background, batched)

    Read path:

    VM reads from /dev/vdb
         │
         ▼
    Cloud Hypervisor virtio-block → rate limit check
         │
         ▼
    ZeroFS NBD checks:
         ├─► Memory cache → hit? return (microseconds)
         ├─► SSD cache    → hit? return (~100μs)
         └─► S3           → miss? fetch, cache, return (~10-100ms)
```

Hot data (recently written or frequently read) stays in local cache and never touches S3. Cold data is fetched from S3 on demand and cached locally for subsequent reads.

For most VM workloads, **>95% of I/O hits the cache**. The working set of a typical VM (OS, active database pages, application data) fits in the local SSD cache.

### Snapshots

Since all data ultimately lives in S3 as immutable SST files, snapshots are lightweight:

1. ZeroFS flushes pending writes to S3
2. Record the current set of SST files as a snapshot
3. Done — no data copy needed

Restoring a snapshot means pointing a new volume at the recorded SST files.

### Moving volumes between nodes

Because the durable data is in S3 (not on local disk), a volume can be detached from one node and attached to another — see [compute.md](compute.md) for the full migration flow.

```
    Node A                                 Node B
    ──────                                 ──────

    1. Stop VM (cloud-hypervisor process stopped)
    2. ZeroFS flushes cache to S3
    3. Disconnect NBD on Node A
                                           4. ZeroFS connects NBD
                                              to same S3 data
                                           5. Start VM (cloud-hypervisor
                                              boots in ~200ms)

    Data copied: zero
    Downtime: ~5-30 seconds
    Cache: cold on Node B, warms up gradually
```

The volume data is the same — only the cache is different. First reads on Node B will hit S3 (~10-100ms), then cache locally for subsequent access. Within minutes, the active working set is cached and performance returns to near-native.

This design makes **compute and storage independent**: a Cloud Hypervisor VM can be stopped and restarted on any node, and ZeroFS reconnects to the same data in S3. No shared filesystem, no distributed block device, no data replication between nodes.

## Operator setup

The operator configures S3 access when setting up each node. The platform needs a single S3 bucket (or one per region) as the storage backend.

```bash
# During node setup, the operator provides S3 credentials
syfrah node configure-storage \
  --s3-endpoint https://s3.par.io.cloud.ovh.net \
  --s3-bucket syfrah-storage-eu-west \
  --s3-access-key XXXX \
  --s3-secret-key XXXX \
  --cache-disk-path /dev/nvme1n1 \
  --cache-disk-size 200 \
  --cache-memory-size 8
```

The cache uses the node's local SSD for fast access. The S3 bucket stores the actual data durably.

### S3 bucket strategies

| Strategy | When to use |
|---|---|
| **One bucket per region** | Multiple nodes in the same region share a bucket. Simple, cost-effective. |
| **One bucket per node** | Maximum isolation. Each node's data is independent. |
| **One bucket for everything** | Simplest setup. Works well for small deployments. |

The choice doesn't affect tenants — they see the same block devices regardless of the bucket layout.

## Encryption

All data stored in S3 is encrypted by ZeroFS before it leaves the node:

- **Algorithm**: XChaCha20-Poly1305 (authenticated encryption)
- **Key derivation**: Argon2id from a password/key configured by the operator
- **Scope**: Every chunk written to S3 is encrypted. The S3 provider never sees plaintext data.

This is encryption at rest that the operator controls — independent of any server-side encryption the S3 provider might offer.

## Performance characteristics

| Scenario | Latency | Bottleneck |
|---|---|---|
| Hot read (in memory cache) | Microseconds | Memory speed |
| Warm read (in SSD cache) | ~100 microseconds | SSD speed |
| Cold read (from S3) | 10-100 milliseconds | S3 latency + network |
| Write (buffered) | Microseconds | Memory speed |
| Write (fsynced to WAL) | ~10 milliseconds | S3 PUT latency |

For most workloads, the cache absorbs the majority of I/O. The working set of a typical VM (OS, active data, hot database pages) fits in the SSD cache. S3 latency only matters for cold reads.

### Sizing the cache

The local SSD cache should be sized to cover the **working set** of all VMs on the node:

| Node workload | Recommended SSD cache | Memory cache |
|---|---|---|
| 5 small VMs (web servers) | 50 GB | 4 GB |
| 3 medium VMs (app + DB) | 200 GB | 8 GB |
| 1 large VM (heavy database) | 500 GB | 16 GB |

Over-provisioning the cache is cheap (local SSD is free with the dedicated server) and dramatically improves performance.

## Limitations

- **Single-writer**: A volume can only be attached to one VM at a time (standard block device semantics)
- **Cache warmup**: After moving a volume to a new node, reads are slower until the cache warms up
- **S3 dependency**: If the S3 endpoint is unreachable, writes buffer locally until capacity runs out. Extended S3 outages can cause I/O stalls.
- **No live resize**: Volume size cannot be changed while attached. Detach, resize, reattach.

## Why this matters for Syfrah

This storage design aligns with Syfrah's core philosophy: **leverage what providers already offer instead of building heavy infrastructure**.

The operator doesn't need to set up a Ceph cluster or manage replicated storage. They rent dedicated servers (compute) and an S3 bucket (storage) from their existing provider. Syfrah ties them together with ZeroFS.

```
    What the operator provides:        What Syfrah adds:
    ──────────────────────────         ─────────────────

    Dedicated servers                  Fabric (WireGuard mesh)
    (OVH, Hetzner, Scaleway)          Forge (node control)
                                       ZeroFS (block storage)
    S3 bucket                          Products (VMs, LBs, DBs)
    (same provider or any)             Organization model
```

## Relationship to other concepts

```
    ┌──────────────────────────────────────────────────────┐
    │                                                      │
    │   Cloud Products  ◄── products.md      │
    │   VMs use volumes, databases use volumes              │
    │                                                      │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │   Storage         ◄── this document                  │
    │   ZeroFS + S3: block devices backed by object storage│
    │                                                      │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │   Forge           ◄── forge.md         │
    │   Creates/attaches volumes via ZeroFS NBD            │
    │                                                      │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │   Fabric          ◄── fabric.md        │
    │   Connects nodes (storage is per-node + S3)          │
    │                                                      │
    └──────────────────────────────────────────────────────┘
```
