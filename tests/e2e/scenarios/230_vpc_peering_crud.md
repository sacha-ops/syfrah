# Test: VPC Peering CRUD operations

## Objective

- Create, list, get, and delete VPC peerings via the CLI
- Validate that self-peering and duplicate peering are rejected
- Verify key normalization (order of VPCs does not matter)
- Verify that a VPC with active peerings cannot be deleted

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing orgs, VPCs, or peerings)

## Steps

### 1. Set up org, project, and two VPCs

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah vpc create hub --project backend --org acme --cidr 10.1.0.0/16
syfrah vpc create spoke-a --project backend --org acme --cidr 10.2.0.0/16
syfrah vpc create spoke-b --project backend --org acme --cidr 10.3.0.0/16
```

### 2. Create a peering between hub and spoke-a

```bash
syfrah vpc peer --from hub --to spoke-a
```

Expected: peering created successfully, status Active.

### 3. Create a peering between hub and spoke-b

```bash
syfrah vpc peer --from hub --to spoke-b
```

Expected: peering created successfully.

### 4. List all peerings

```bash
syfrah vpc peerings
```

Expected: two peerings listed (hub/spoke-a and hub/spoke-b).

### 5. List peerings for a specific VPC

```bash
syfrah vpc peerings --vpc hub
```

Expected: two peerings (hub is in both).

```bash
syfrah vpc peerings --vpc spoke-a
```

Expected: one peering (hub/spoke-a only).

### 6. Reject self-peering

```bash
syfrah vpc peer --from hub --to hub
```

Expected: error — cannot peer a VPC with itself.

### 7. Reject duplicate peering

```bash
syfrah vpc peer --from hub --to spoke-a
```

Expected: error — peering already exists.

### 8. Reject duplicate peering (reversed order)

```bash
syfrah vpc peer --from spoke-a --to hub
```

Expected: error — peering already exists (key normalization).

### 9. VPC deletion blocked by active peering

```bash
syfrah vpc delete hub
```

Expected: error — VPC has active peerings, cannot delete.

### 10. Delete a peering

```bash
syfrah vpc unpeer --from hub --to spoke-a
```

Expected: peering deleted successfully.

### 11. Verify peering is gone

```bash
syfrah vpc peerings --vpc hub
```

Expected: one peering remaining (hub/spoke-b).

### 12. Delete remaining peering and VPC

```bash
syfrah vpc unpeer --from hub --to spoke-b
syfrah vpc delete hub
```

Expected: both succeed — hub has no more peerings.

## Cleanup

```bash
syfrah vpc delete spoke-a
syfrah vpc delete spoke-b
syfrah project delete backend --org acme
syfrah org delete acme
```
