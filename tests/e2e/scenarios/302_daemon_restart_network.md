# 302 — Daemon restart network recovery

## Purpose
Verify that after a daemon restart (or node reboot), all overlay network
state is correctly recovered: bridges, VXLANs, TAPs, nftables rules, and
FDB entries are reconciled with the persisted redb state.

## Prerequisites
- A running Syfrah daemon with at least one org, project, environment,
  VPC, subnet, and VM provisioned.
- The VM has an IP assigned and nftables rules applied.

## Steps

### 1. Baseline — capture state before restart
```bash
# Record existing interfaces
ip link show | grep -E 'syfbr-|syfvx-|syftap-'

# Record nftables rules
nft list table inet syfrah
nft list table ip syfrah_nat

# Verify VM connectivity
syfrah compute vm list --project backend --org acme
```

### 2. Restart the daemon
```bash
# Stop the daemon (simulates restart, not reboot)
syfrah fabric stop

# Start the daemon again
syfrah fabric start
```

### 3. Verify bridges and VXLANs survived
```bash
# Bridges should still exist (kernel keeps them across daemon restart)
ip link show | grep 'syfbr-'
# Expected: same bridges as step 1

# VXLANs should still exist
ip link show | grep 'syfvx-'
# Expected: same VXLANs as step 1
```

### 4. Verify nftables rules re-applied
```bash
# nftables rules survive daemon restart (but NOT reboot)
nft list table inet syfrah
# Expected: anti-spoofing, ingress deny, SSH allow, ICMP allow rules

nft list table ip syfrah_nat
# Expected: masquerade rules for each subnet
```

### 5. Verify FDB entries rebuilt
```bash
# FDB entries should be rebuilt from vm_placements table
bridge fdb show | grep -i '02:00'
# Expected: static FDB entries for all remote VMs
```

### 6. Simulate reboot (flush nftables)
```bash
# Flush nftables to simulate reboot
nft flush ruleset

# Restart daemon
syfrah fabric stop
syfrah fabric start

# Verify rules are re-applied
nft list table inet syfrah
# Expected: all VM rules re-applied
```

### 7. Orphaned interface cleanup
```bash
# Create a fake orphaned bridge
ip link add syfbr-orphan type bridge

# Restart daemon
syfrah fabric stop
syfrah fabric start

# Verify orphaned bridge was deleted
ip link show syfbr-orphan 2>&1
# Expected: "Device syfbr-orphan does not exist"
```

### 8. Missing interface recovery
```bash
# Delete a bridge that should exist
ip link del syfbr-<vpc_id>

# Restart daemon
syfrah fabric stop
syfrah fabric start

# Verify bridge was re-created
ip link show syfbr-<vpc_id>
# Expected: bridge exists with correct gateway IPs
```

## Expected results
- `RecoveryReport.bridges_recovered`: 0 on daemon restart (kernel keeps bridges), >0 after manual deletion
- `RecoveryReport.vxlans_recovered`: 0 on daemon restart, >0 after manual deletion
- `RecoveryReport.nft_reapplied`: equals number of local VMs
- `RecoveryReport.fdb_rebuilt`: equals number of remote VMs in shared VPCs
- `RecoveryReport.orphans_cleaned`: equals number of orphaned interfaces found
- VM connectivity is restored after recovery completes
