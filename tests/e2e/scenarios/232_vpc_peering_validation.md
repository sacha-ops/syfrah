# 232 — VPC Peering Validation

Validates that peering creation enforces the five validation rules:
no self-peering, no duplicates, CIDR overlap warning, both VPCs must exist,
and key normalization (A/B == B/A).

## Prerequisites

- `syfrah` binary built and accessible
- No pre-existing orgs/VPCs/peerings in the store

## Setup

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah vpc create hub-vpc --project backend --org acme --cidr 10.1.0.0/16
syfrah vpc create spoke-a --project backend --org acme --cidr 10.2.0.0/16
syfrah vpc create spoke-b --project backend --org acme --cidr 10.3.0.0/16
```

## Scenario 1: Self-peering rejected

```bash
syfrah vpc peer --from hub-vpc --to hub-vpc
# Expected: error "cannot peer a VPC with itself"
# Exit code: non-zero
```

## Scenario 2: Successful peering

```bash
syfrah vpc peer --from hub-vpc --to spoke-a
# Expected: peering created, status Active
# Exit code: 0

syfrah vpc peerings --vpc hub-vpc
# Expected: one peering listed (hub-vpc <-> spoke-a)
```

## Scenario 3: Duplicate peering rejected

```bash
syfrah vpc peer --from hub-vpc --to spoke-a
# Expected: error "already peered"
# Exit code: non-zero
```

## Scenario 4: Reverse-direction duplicate also rejected

```bash
syfrah vpc peer --from spoke-a --to hub-vpc
# Expected: error "already peered"
# Exit code: non-zero
```

## Scenario 5: Nonexistent VPC rejected

```bash
syfrah vpc peer --from hub-vpc --to ghost-vpc
# Expected: error "vpc not found: ghost-vpc"
# Exit code: non-zero
```

## Scenario 6: CIDR overlap warning (peering still succeeds)

Create two VPCs with overlapping CIDRs (in different orgs to bypass intra-org overlap check):

```bash
syfrah org create other-org
syfrah project create svc --org other-org
syfrah vpc create overlap-a --project svc --org other-org --cidr 10.50.0.0/16
syfrah vpc create overlap-b --project backend --org acme --cidr 10.50.0.0/16
syfrah vpc peer --from overlap-a --to overlap-b
# Expected: peering created successfully (exit 0)
# Expected: warning in logs about overlapping CIDRs
```

## Teardown

```bash
syfrah vpc unpeer --from hub-vpc --to spoke-a
syfrah vpc unpeer --from overlap-a --to overlap-b
syfrah vpc delete spoke-b
syfrah vpc delete spoke-a
syfrah vpc delete hub-vpc
syfrah vpc delete overlap-a
syfrah vpc delete overlap-b
syfrah project delete svc --org other-org
syfrah project delete backend --org acme
syfrah org delete other-org
syfrah org delete acme
```
