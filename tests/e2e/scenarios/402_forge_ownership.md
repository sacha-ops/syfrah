# Test: Forge ownership registry in redb

## Objective

- Ownership registry stores resource_id → {type, kernel_name, created_at}
- register(), deregister(), lookup(), list_all(), rebuild() work correctly
- 3-tier orphan policy: known→manage, suspected→quarantine, unknown→ignore

## Prerequisites

- Rust toolchain installed
- Repository checked out

## Steps

### 1. Run ownership unit tests

```bash
cargo test -p syfrah-forge -- ownership
```

### 2. Verify the registry functions

The unit tests cover:
- `register_and_lookup` — register a resource, verify lookup returns it
- `deregister` — register then deregister, verify it's gone
- `list_all` — register multiple resources, verify count
- `classify_orphan_policy` — test 3-tier classification:
  - Known: resource with matching kernel_name in registry
  - Suspected: br-*, tap-*, vx-* prefixed names not in registry
  - Unknown: names that don't match Syfrah patterns
- `rebuild_replaces_all` — rebuild clears old entries and inserts new ones

### 3. Verify on running daemon

```bash
# After daemon start with VMs running:
syfrah state list forge
# Should show ownership_registry table with entries for each managed resource
```

## Expected Results

- All ownership unit tests pass
- Registry persists across daemon restarts (redb durability)
- Orphan classification correctly identifies:
  - Known resources (in registry) → manage
  - Suspected orphans (syfrah-prefixed but not in registry) → quarantine
  - Unknown resources (not syfrah-related) → ignore
