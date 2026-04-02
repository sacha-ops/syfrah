# 601 — VPC Peering Connectivity

Verify that VMs in peered VPCs CAN communicate, and that unpeering restores isolation.

## Prerequisites

- Test 600 setup completed (vpc-a, vpc-b, iso-a, iso-b all exist and running)
- iso-a and iso-b confirmed isolated (test 600 passed)

## Steps

### Step 1 — Peer the two VPCs

```bash
syfrah vpc peer --from vpc-a --to vpc-b
```

### Step 2 — Verify connectivity after peering

```bash
# From iso-a, ping iso-b's IP
# ping <iso-b-ip> -c 4 -W 2
# Expected: 0% packet loss — peering bridges the two VNIs
```

### Step 3 — Verify latency

```bash
# Same-zone peering (both fsn1): latency should be <5ms
# Cross-zone peering (fsn1 ↔ hel1): latency should be <30ms
# ping <iso-b-ip> -c 10 | tail -1
# Expected: avg < 5ms for same-zone
```

### Step 4 — Unpeer the VPCs

```bash
syfrah vpc unpeer --from vpc-a --to vpc-b
```

### Step 5 — Verify isolation restored

```bash
# From iso-a, ping iso-b's IP again
# ping <iso-b-ip> -c 4 -W 2
# Expected: 100% packet loss — isolation restored
```

## Assertions

| Check                              | Expected          |
|------------------------------------|--------------------|
| Ping after peering                 | 0% loss            |
| Latency (same zone)               | < 5ms avg          |
| Ping after unpeering               | 100% loss (timeout)|

## Pass criteria

- Peering MUST enable cross-VPC communication
- Latency MUST be reasonable for the zone topology
- Unpeering MUST fully restore isolation (no residual routes or FDB entries)
