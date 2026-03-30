# Test: VPC deletion guards prevent unsafe deletes

## Objective

- VPC with subnets cannot be deleted
- VPC with active peerings cannot be deleted
- Empty VPC deletes successfully
- Error messages are clear and include resource counts

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- An org and project exist (e.g. `acme` / `backend`)

## Steps

### 1. Create a VPC

```bash
syfrah vpc create test-vpc --project backend --org acme --cidr 10.1.0.0/16
```

Expected: VPC created with a unique VNI.

### 2. Delete empty VPC succeeds

```bash
syfrah vpc delete test-vpc
```

Expected: VPC deleted. `syfrah vpc list` no longer shows `test-vpc`.

### 3. Recreate VPC and add a subnet

```bash
syfrah vpc create test-vpc --project backend --org acme --cidr 10.1.0.0/16
syfrah env create staging --project backend --org acme
syfrah subnet create frontend --env staging --project backend --org acme --vpc test-vpc
```

### 4. Attempt to delete VPC with subnet

```bash
syfrah vpc delete test-vpc
```

Expected: Error. Output must contain:
- `cannot delete vpc 'test-vpc'`
- `has 1 active subnet(s)`

Exit code: non-zero.

### 5. Remove the subnet, then delete

```bash
syfrah subnet delete frontend
syfrah vpc delete test-vpc
```

Expected: VPC deleted successfully after subnet removal.

### 6. Recreate VPCs and add a peering

```bash
syfrah vpc create vpc-hub --project backend --org acme --cidr 10.1.0.0/16
syfrah vpc create vpc-spoke --project backend --org acme --cidr 10.2.0.0/16
syfrah vpc peer --from vpc-hub --to vpc-spoke
```

### 7. Attempt to delete VPC with active peering

```bash
syfrah vpc delete vpc-hub
```

Expected: Error. Output must contain:
- `cannot delete vpc 'vpc-hub'`
- `has 1 active peering(s)`

Exit code: non-zero.

### 8. Remove the peering, then delete

```bash
syfrah vpc unpeer --from vpc-hub --to vpc-spoke
syfrah vpc delete vpc-hub
syfrah vpc delete vpc-spoke
```

Expected: Both VPCs deleted successfully after peering removal.

## Pass criteria

- Steps 2, 5, 8: VPC deleted successfully (exit 0)
- Steps 4, 7: VPC delete rejected with clear error (exit non-zero)
- Error messages include the VPC name and resource count
- No data corruption: resources that were not deleted remain intact
