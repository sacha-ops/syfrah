# Test: Overlay bridge management — create, configure, and delete bridges

## Objective

- A Linux bridge named `syfbr-{vpc_id}` can be created and brought up
- Gateway IPs can be added to and removed from the bridge
- An interface can be attached to the bridge
- Deleting the bridge removes it cleanly
- All operations are idempotent (repeating them does not produce errors)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- Root access (bridge management requires CAP_NET_ADMIN)
- The `ip` command available (iproute2 package)
- No existing `syfbr-test-*` bridges (clean state)

## Steps

### 1. Create a bridge

```bash
# Via syfrah overlay or directly using the LinuxBackend
# The bridge name follows the convention: syfbr-{vpc_id}
ip link show syfbr-test-100 2>/dev/null && echo "FAIL: bridge already exists" && exit 1
```

Trigger bridge creation for VPC ID `test-100`. Verify:

```bash
ip link show syfbr-test-100
```

**Expected**: bridge exists and state is UP.

### 2. Idempotent create

Trigger bridge creation for VPC ID `test-100` again.

**Expected**: no error, bridge still exists and is UP.

### 3. Add a gateway IP (subnet 1)

Add `10.99.1.1/24` to `syfbr-test-100`.

```bash
ip addr show dev syfbr-test-100
```

**Expected**: output contains `inet 10.99.1.1/24`.

### 4. Add a gateway IP (subnet 2)

Add `10.99.2.1/24` to `syfbr-test-100`.

```bash
ip addr show dev syfbr-test-100
```

**Expected**: output contains both `inet 10.99.1.1/24` and `inet 10.99.2.1/24`.

### 5. Idempotent IP add

Add `10.99.1.1/24` again.

**Expected**: no error (EEXIST is silently ignored).

### 6. Remove a gateway IP

Remove `10.99.1.1` from `syfbr-test-100`.

```bash
ip addr show dev syfbr-test-100
```

**Expected**: `10.99.1.1/24` is gone, `10.99.2.1/24` still present.

### 7. Idempotent IP remove

Remove `10.99.1.1` again.

**Expected**: no error (already removed).

### 8. Attach an interface to the bridge

Create a dummy interface and attach it:

```bash
ip link add syf-dummy0 type dummy
ip link set syf-dummy0 master syfbr-test-100
ip -o link show syf-dummy0
```

**Expected**: output contains `master syfbr-test-100`.

### 9. Delete the bridge

Trigger bridge deletion for VPC ID `test-100`.

```bash
ip link show syfbr-test-100 2>/dev/null
```

**Expected**: command fails (device not found). Attached interfaces are released.

### 10. Idempotent delete

Trigger bridge deletion for VPC ID `test-100` again.

**Expected**: no error (already deleted).

## Cleanup

```bash
ip link del syf-dummy0 2>/dev/null || true
ip link del syfbr-test-100 2>/dev/null || true
```

## Expected results

- All 10 steps pass without errors
- Bridge operations are idempotent throughout
- Multiple gateway IPs coexist on the same bridge
- Bridge deletion is clean (no leftover interfaces)

## Failure criteria

- Any step returns a non-zero exit code unexpectedly
- Creating an existing bridge produces an error
- Deleting a non-existent bridge produces an error
- Adding a duplicate IP produces an error
- Removing a non-existent IP produces an error
- Bridge is not in UP state after creation
