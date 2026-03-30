# Test: Overlay veth peering between VPC bridges

## Objective

Validate that `create_veth_peer` and `delete_veth_peer` correctly manage veth
pairs between two VPC bridges on the same node, including interface creation,
bridge attachment, route setup, and cleanup.

## Prerequisites

- Rust toolchain installed
- Repository cloned and workspace builds (`cargo build`)
- `syfrah-overlay` crate with `NetworkBackend` trait and `MockBackend`

## Steps

### 1. Build the overlay crate

```bash
cargo build -p syfrah-overlay
```

**Expected**: compiles without errors.

### 2. Run veth peer unit tests

```bash
cargo test -p syfrah-overlay -- veth_peer
```

**Expected**: all veth peer tests pass:
- `create_veth_peer_creates_and_attaches_both_ends`
- `create_veth_peer_is_idempotent`
- `add_routes_for_peer_cidrs`
- `delete_peer_cleans_up`
- `delete_peer_is_idempotent`
- `cleanup_routes_removed_on_delete`

### 3. Verify create sequence

The `create_veth_peer` function must perform operations in this order:
1. Check if `syfpeer-{peering_id}-a` already exists (idempotency guard)
2. Create veth pair (`syfpeer-{peering_id}-a`, `syfpeer-{peering_id}-b`)
3. Attach `-a` to `bridge_a`, `-b` to `bridge_b`
4. Bring both interfaces up
5. Add route for `vpc_b_cidr` via `-a` device
6. Add route for `vpc_a_cidr` via `-b` device
7. Apply peering forwarding rules between bridges

### 4. Verify delete sequence

The `delete_veth_peer` function must:
1. Check if `syfpeer-{peering_id}-a` exists (idempotency guard)
2. Remove routes for both CIDRs
3. Remove peering forwarding rules
4. Delete the veth pair (kernel auto-removes both ends)

### 5. Verify idempotency — create

Call `create_veth_peer` when the link already exists. Only the existence check
should be recorded; no creation or attachment calls.

### 6. Verify idempotency — delete

Call `delete_veth_peer` when the link does not exist. Only the existence check
should be recorded; no deletion calls.

### 7. Clippy clean

```bash
cargo clippy -p syfrah-overlay -- -D warnings
```

**Expected**: no warnings.

## Pass criteria

- All 6 veth peer unit tests pass
- Create and delete operations are fully idempotent
- Routes are added for both VPC CIDRs in both directions
- Routes and peering rules are cleaned up on delete
- No clippy warnings
