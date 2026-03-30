# Test: VPC hub & spoke topology

## Objective

Verify that VPC peering supports a hub & spoke topology where a hub VPC peers with multiple spoke VPCs, spokes cannot reach each other (no transitive peering), and individual peerings can be removed without affecting others.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- An org and project exist (e.g. `acme` / `backend`)

## Steps

### 1. Create hub and spoke VPCs

```bash
syfrah vpc create hub-vpc --project backend --org acme --cidr 10.0.0.0/16
syfrah vpc create spoke-a --project backend --org acme --cidr 10.1.0.0/16
syfrah vpc create spoke-b --project backend --org acme --cidr 10.2.0.0/16
```

Expected: All three VPCs created with unique VNIs. `syfrah vpc list` shows 3 VPCs.

### 2. Peer hub with both spokes

```bash
syfrah vpc peer --from hub-vpc --to spoke-a
syfrah vpc peer --from hub-vpc --to spoke-b
```

Expected: Both peering commands succeed (exit 0).

### 3. Verify hub peerings

```bash
syfrah vpc peerings --vpc hub-vpc
```

Expected: Output shows 2 active peerings:
- hub-vpc <-> spoke-a
- hub-vpc <-> spoke-b

### 4. Verify spoke-a peerings

```bash
syfrah vpc peerings --vpc spoke-a
```

Expected: Output shows exactly 1 active peering (with hub-vpc only). No peering with spoke-b.

### 5. Verify spoke-b peerings

```bash
syfrah vpc peerings --vpc spoke-b
```

Expected: Output shows exactly 1 active peering (with hub-vpc only). No peering with spoke-a.

### 6. Verify no transitive peering between spokes

Confirm that spoke-a and spoke-b are NOT peered. There is no route between them through the hub.

```bash
syfrah vpc peerings --vpc spoke-a
syfrah vpc peerings --vpc spoke-b
```

Expected: Neither spoke lists the other spoke in its peerings. Peering is explicit, not transitive.

### 7. Delete one spoke peering

```bash
syfrah vpc unpeer --from hub-vpc --to spoke-a
```

Expected: Unpeer succeeds (exit 0).

### 8. Verify hub still peered with spoke-b

```bash
syfrah vpc peerings --vpc hub-vpc
```

Expected: Output shows exactly 1 active peering (with spoke-b only).

### 9. Verify spoke-a has no peerings

```bash
syfrah vpc peerings --vpc spoke-a
```

Expected: Output shows 0 peerings.

### 10. Verify spoke-b is unaffected

```bash
syfrah vpc peerings --vpc spoke-b
```

Expected: Output shows exactly 1 active peering (with hub-vpc).

### 11. Cleanup

```bash
syfrah vpc unpeer --from hub-vpc --to spoke-b
syfrah vpc delete hub-vpc
syfrah vpc delete spoke-a
syfrah vpc delete spoke-b
```

Expected: All resources cleaned up. `syfrah vpc list` shows no hub/spoke VPCs.

## Pass criteria

- Hub VPC has exactly 2 peerings after step 2
- Each spoke has exactly 1 peering (with hub only)
- Spokes are never peered with each other (no transitive peering)
- Removing one spoke peering does not affect the other
- All cleanup succeeds without errors

## Failure criteria

- Spokes appear peered with each other at any point
- Removing hub<->spoke-a peering also removes hub<->spoke-b
- Hub peering count does not match expected values
- Any command returns an unexpected error
