# Test: VPC Peering CLI

## Objective

Verify the VPC peering CLI commands: `vpc peer`, `vpc unpeer`, and `vpc peerings`.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running and responsive
- No pre-existing VPCs or peerings

## Steps

### 1. Set up org and project

```bash
syfrah org create peering-test-org
syfrah project create backend --org peering-test-org
```

### 2. Create two VPCs

```bash
syfrah vpc create hub-vpc --project backend --org peering-test-org --cidr 10.1.0.0/16
syfrah vpc create spoke-a --project backend --org peering-test-org --cidr 10.2.0.0/16
```

Expected: both VPCs created successfully.

### 3. List peerings (empty)

```bash
syfrah vpc peerings
```

Expected output:
```
No peerings found.
```

### 4. Create a peering

```bash
syfrah vpc peer --from hub-vpc --to spoke-a
```

Expected output:
```
VPCs peered: hub-vpc <-> spoke-a
```

### 5. List peerings

```bash
syfrah vpc peerings
```

Expected output: table with columns VPC_A, VPC_B, STATUS, CREATED showing hub-vpc and spoke-a with status Active.

### 6. List peerings filtered by VPC

```bash
syfrah vpc peerings --vpc hub-vpc
```

Expected: same peering shown (hub-vpc is part of it).

```bash
syfrah vpc peerings --vpc spoke-a
```

Expected: same peering shown (spoke-a is part of it).

### 7. List peerings as JSON

```bash
syfrah vpc peerings --json
```

Expected: JSON array with one peering object containing vpc_a, vpc_b, status, created_at fields.

### 8. Error: self-peering

```bash
syfrah vpc peer --from hub-vpc --to hub-vpc
```

Expected error:
```
cannot peer a VPC with itself: 'hub-vpc'
```

### 9. Error: duplicate peering

```bash
syfrah vpc peer --from hub-vpc --to spoke-a
```

Expected error:
```
VPCs 'hub-vpc' and 'spoke-a' are already peered
```

### 10. Error: unknown VPC

```bash
syfrah vpc peer --from hub-vpc --to nonexistent
```

Expected error:
```
vpc not found: nonexistent
```

### 11. Remove peering

```bash
syfrah vpc unpeer --from hub-vpc --to spoke-a
```

Expected output:
```
VPCs unpeered: hub-vpc <-> spoke-a
```

### 12. Verify peering removed

```bash
syfrah vpc peerings
```

Expected output:
```
No peerings found.
```

### 13. Error: unpeer when not peered

```bash
syfrah vpc unpeer --from hub-vpc --to spoke-a
```

Expected error:
```
no active peering between 'hub-vpc' and 'spoke-a'
```

### 14. Cleanup

```bash
syfrah vpc delete hub-vpc --org peering-test-org --yes
syfrah vpc delete spoke-a --org peering-test-org --yes
syfrah project delete backend --org peering-test-org --yes
syfrah org delete peering-test-org --yes
```

## Pass criteria

- All commands produce expected output
- Error messages are actionable and mention the specific resource names
- Table output has VPC_A, VPC_B, STATUS, CREATED columns
- JSON output is valid and contains all peering fields
- Self-peering is rejected
- Duplicate peering is rejected
- Unpeering a non-existent peering shows a clear error
