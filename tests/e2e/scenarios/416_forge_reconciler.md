# Test: Forge reconciliation engine

## Objective

- Reconciler runs a 5-second periodic loop
- Reads desired state from local state
- Observes actual state (kernel interfaces, processes, nftables)
- Computes diff between desired and actual
- Applies changes in dependency order (bridge -> VXLAN -> NIC -> SG -> FDB -> NAT -> route -> VM)
- Event-driven: API mutations trigger immediate reconciliation

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized

## Steps

### 1. Initialize and get Forge IP

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Verify reconciler is running

The reconciler starts automatically with the Forge server. Check logs for reconciliation pass messages:

```bash
journalctl -u syfrah --since "1 minute ago" 2>/dev/null | grep -i reconcil || echo "Check daemon logs for reconciliation"
```

**Expected:** Log entries showing periodic reconciliation passes.

### 3. Create resources and verify reconciler tracks them

```bash
# Create a bridge (triggers reconciler).
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-reconcile-test"}'

# Wait for reconciler pass (5s interval).
sleep 6

# The bridge should still exist (reconciler maintains it).
ip link show | grep syfb-
```

**Expected:** Bridge interface is present and maintained by the reconciler.

### 4. Dependency order verification

```bash
# Create resources in dependency order:
# 1. Bridge
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-dep-test"}'

# 2. VXLAN (depends on bridge)
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/vxlans \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id":"vpc-dep-test","vni":200,"local_ip":"10.0.0.1"}'

# 3. NIC (depends on bridge)
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/interfaces \
  -H 'Content-Type: application/json' \
  -d '{"vm_id":"vm-dep","vpc_id":"vpc-dep-test","ip":"10.1.0.3","mac":"02:00:0a:01:00:03"}'

# All resources should be present after reconciler pass.
sleep 6
ip link show | grep -c 'syfb-\|syfx-\|syft-'
```

**Expected:** At least 3 interfaces (bridge, VXLAN, TAP).

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
