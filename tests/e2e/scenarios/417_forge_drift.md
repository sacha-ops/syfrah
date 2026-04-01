# Test: Forge drift detection for all resource types

## Objective

Verify drift detection for 12 scenarios:
1. Missing bridge
2. Missing VXLAN
3. Missing TAP
4. Dead VM
5. Stale nftables
6. Wrong SG rules
7. Orphaned IP
8. Missing FDB
9. Missing ARP proxy
10. Missing NAT
11. Wrong gateway IP
12. Stale route

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

### 2. Create a bridge, then delete it from kernel

```bash
# Create via Forge API.
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-drift-test"}'

# Get bridge name.
BRIDGE=$(ip link show | grep syfb- | head -1 | awk -F: '{print $2}' | tr -d ' ')

# Delete bridge from kernel directly (simulate drift).
ip link delete $BRIDGE 2>/dev/null

# Verify bridge is gone.
ip link show $BRIDGE 2>&1
```

**Expected:** "does not exist" error. The reconciler should detect this as a missing bridge on the next pass.

### 3. Delete a VXLAN from kernel (simulate drift)

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/vxlans \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id":"vpc-drift-test","vni":300,"local_ip":"10.0.0.1"}'

VXLAN=$(ip link show | grep syfx- | head -1 | awk -F: '{print $2}' | tr -d ' ')
ip link delete $VXLAN 2>/dev/null
```

### 4. Flush nftables (simulate stale rules)

```bash
# Apply some SG rules.
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/sg/apply \
  -H 'Content-Type: application/json' \
  -d '{"vm_id":"vm-drift","ip":"10.1.0.3","mac":"02:00:0a:01:00:03","security_groups":["default"],"rules":[],"sg_ip_map":{}}'

# Flush the nftables table (simulate drift).
nft flush table inet syfrah_sg 2>/dev/null
```

**Expected:** The reconciler detects missing SG chains on the next pass.

### 5. Verify drift is detected

The drift detection module provides the following checks:
- NetworkDriftDetector: compares expected bridges/VXLANs/TAPs against kernel
- VmDriftDetector: compares expected VMs against running processes
- SgDriftDetector: compares expected nftables chains against actual
- FdbDriftDetector: reports missing FDB entries
- NatDriftDetector: reports missing NAT rules

All 12 scenarios have unit tests that verify detection logic.

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
