# syfrah-state

Embedded key-value persistence for Syfrah layers, backed by [redb](https://github.com/cberner/redb).

## Features

| Feature | What | API |
|---------|------|-----|
| **Typed tables** | Compile-time safe table refs | `TypedTable<T>`, `put()`, `fetch()` |
| **Prefix scan** | List by key prefix | `list_by_prefix()`, `scan_prefix()` |
| **Pagination** | Offset + limit | `list_page()` |
| **Secondary indexes** | Atomic primary + index writes | `set_with_index()`, `lookup_index()` |
| **TTL / expiration** | Auto-expire entries | `put_with_ttl()` |
| **Watch** | Key change notifications | `watch()`, `unwatch()` |
| **Optimistic locking** | Compare-and-swap | `cas()`, `fetch_versioned()` |
| **Compaction** | GC expired entries | `gc_expired()` |
| **Batches** | Multi-table ACID | `batch()` |
| **Metrics** | u64 counters | `get_metric()`, `inc_metric()` |
| **Snapshots** | Export/import raw | `export_raw()`, `import_raw()` |

## Quick start

```rust
use syfrah_state::{LayerDb, TypedTable};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Peer { name: String, zone: String }

// Typed table — compiler catches typos
const PEERS: TypedTable<Peer> = TypedTable::new("peers");

let db = LayerDb::open("fabric")?;

// Put with auto-versioning
let version = db.put(&PEERS, "node-1", &Peer { name: "node-1".into(), zone: "fsn1".into() })?;
assert_eq!(version, 1);

// Fetch (returns None for expired or missing)
let peer = db.fetch(&PEERS, "node-1")?.unwrap();

// Fetch with version
let (peer, ver) = db.fetch_versioned(&PEERS, "node-1")?.unwrap();
assert_eq!(ver, 1);
```

## Typed tables

```rust
const VPCS: TypedTable<Vpc> = TypedTable::new("vpcs");
const PEERS: TypedTable<Peer> = TypedTable::new("peers");

// Compiler enforces the type:
db.put(&VPCS, "vpc-01", &vpc)?;     // ✓ Vpc
db.put(&VPCS, "vpc-01", &peer)?;    // ✗ compile error: expected Vpc
```

## Prefix scan

```rust
db.put(&PEERS, "eu/node-1", &peer1)?;
db.put(&PEERS, "eu/node-2", &peer2)?;
db.put(&PEERS, "us/node-3", &peer3)?;

let eu_peers = db.scan_prefix(&PEERS, "eu/")?;  // 2 results
let all = db.scan_prefix(&PEERS, "")?;           // 3 results
```

## Pagination

```rust
let page1 = db.list_page::<String>("events", 0, 50)?;   // first 50
let page2 = db.list_page::<String>("events", 50, 50)?;  // next 50
```

## Secondary indexes

```rust
// Write primary + index atomically
db.set_with_index("vpcs", "vpc-01", &vpc, "vpcs_by_name", "my-vpc")?;

// Lookup by name → get ID
let vpc_id = db.lookup_index("vpcs_by_name", "my-vpc")?;  // Some("vpc-01")

// Delete both atomically
db.delete_with_index("vpcs", "vpc-01", "vpcs_by_name", "my-vpc")?;
```

## TTL / expiration

```rust
// Entry expires in 1 hour
db.put_with_ttl(&PEERS, "temp-node", &peer, 3600)?;

// After expiry, fetch returns None
let result = db.fetch(&PEERS, "temp-node")?;  // None if expired

// GC cleans up expired entries from disk
let removed = db.gc_expired("peers")?;
```

## Watch (key change notifications)

```rust
let id = db.watch("peers", |table, key| {
    println!("{table}/{key} changed");
});

db.put(&PEERS, "node-1", &peer)?;  // triggers callback
db.remove(&PEERS, "node-1")?;      // triggers callback

db.unwatch(id);  // stop notifications
```

## Optimistic locking (CAS)

```rust
let (peer, version) = db.fetch_versioned(&PEERS, "node-1")?.unwrap();

// Only succeeds if no one else modified it
let new_version = db.cas(&PEERS, "node-1", &updated_peer, version)?;

// Fails if version changed
let result = db.cas(&PEERS, "node-1", &stale_peer, old_version);
// Err(StateError::VersionConflict { expected: 1, found: 2 })
```

## Architecture

```
~/.syfrah/
    fabric.redb       ← fabric layer state
    controlplane.redb  ← raft state machine
    storage.redb       ← volume/snapshot records
```

One file per layer. No shared database. Each `LayerDb` is `Clone` + `Arc`-safe for async sharing across tokio tasks.
