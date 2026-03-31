# Scenario 313: nftables per-VM chain architecture + vmap dispatch

**Covers:** Issue #869

## Objective

Verify that the per-VM chain nftables architecture correctly isolates VM traffic
using dedicated chains and vmap-based dispatch.

## Prerequisites

- Two VMs (`vm-a`, `vm-b`) in the same VPC/subnet
- Each VM has an assigned IP and MAC from IPAM
- Security groups with SSH (TCP 22) and ICMP ingress rules on `vm-a`
- No ingress rules on `vm-b` (default deny)
- Default egress (allow all) on both VMs

## Steps

### 1. Verify dispatch table structure

```bash
nft list table inet syfrah
```

- The `forward` chain has `policy drop`
- The chain contains `iif vmap @spoofcheck`
- The chain contains `ct state established,related accept`
- The chain contains `ct state invalid drop`
- The chain contains `oif vmap @ingress_chains`
- The chain contains `iif vmap @egress_chains`

### 2. Verify vmap entries

```bash
nft list map inet syfrah spoofcheck
nft list map inet syfrah ingress_chains
nft list map inet syfrah egress_chains
```

- Each map contains entries for both `vm-a` and `vm-b` interfaces
- `spoofcheck` maps interface to `goto spoof_{vm_id}`
- `ingress_chains` maps interface to `goto in_{vm_id}`
- `egress_chains` maps interface to `goto out_{vm_id}`

### 3. Verify anti-spoofing

From `vm-a`, send a packet with a spoofed source MAC or IP:

```bash
# Spoof source MAC (should be dropped by spoof chain)
nft list chain inet syfrah spoof_{vm_a_id}
```

- Chain contains `ether saddr != {expected_mac} drop`
- Chain contains `ip saddr != {expected_ip} drop`

### 4. Verify ingress rules on vm-a

```bash
nft list chain inet syfrah in_{vm_a_id}
```

- Contains `tcp dport 22 accept` (SSH rule)
- Contains `icmp type echo-request accept` (ICMP rule)
- Ends with `drop` (default deny)

### 5. Verify default deny on vm-b

```bash
nft list chain inet syfrah in_{vm_b_id}
```

- Contains only `drop` (no allow rules)
- SSH from external to `vm-b` is blocked

### 6. Verify egress default allow

```bash
nft list chain inet syfrah out_{vm_a_id}
nft list chain inet syfrah out_{vm_b_id}
```

- Both contain `accept`
- Outbound traffic from both VMs succeeds

### 7. Verify VM removal cleanup

Delete `vm-b` and verify:

```bash
nft list table inet syfrah
```

- `vm-b` interface is removed from all three vmaps
- `spoof_{vm_b_id}`, `in_{vm_b_id}`, `out_{vm_b_id}` chains are deleted
- `vm-a` chains and vmap entries are unaffected

### 8. Verify conntrack

From an external host, establish a TCP connection to `vm-a:22`. Verify return
traffic flows without an explicit outbound rule for that connection (conntrack
`established,related` in the forward chain handles it).

## Expected Results

- Per-VM chains provide isolated rule evaluation
- vmap dispatch is O(1) per packet (no linear iif matching)
- Anti-spoofing prevents MAC/IP forgery
- Default deny ingress blocks all unmatched inbound traffic
- Default allow egress permits outbound traffic
- VM removal cleanly removes all associated chains and map entries
- Conntrack allows return traffic for established connections
