# Test: Forge compensating cleanup on failure

## Objective

- When create/update fails mid-way, reverse completed steps best-effort
- RollbackTracker records each completed step
- Rollback executes in reverse order: FDB -> NAT -> nftables -> TAP -> VXLAN -> bridge -> IP
- Failures during rollback are logged but do not prevent subsequent cleanup steps
- Residuals caught by reconciliation loop

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized

## Steps

### 1. Initialize

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Create a bridge successfully

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-cleanup-test"}'
```

### 3. Create a NIC that references a non-existent bridge

```bash
# This tests the cleanup path: TAP creation may succeed but bridge attach fails.
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/interfaces \
  -H 'Content-Type: application/json' \
  -d '{"vm_id":"vm-fail","vpc_id":"vpc-nonexistent","ip":"10.1.0.99","mac":"02:00:0a:01:00:99"}')
echo "$RESULT"
```

**Expected:** Error response (bridge attach failure). The TAP device should be cleaned up (not left as orphan).

### 4. Verify no orphaned TAP device

```bash
ip link show | grep syft- | grep "vm-fail" || echo "No orphaned TAP — cleanup worked"
```

**Expected:** No orphaned TAP device for vm-fail.

### 5. Reconciler catches any residuals

```bash
# Wait for reconciler pass.
sleep 6

# Any residual interfaces will be detected by drift detection.
ip link show | grep -c 'syft-.*vm-fail'
```

**Expected:** 0 (reconciler would clean up any residuals).

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
