# E2E: Bootstrap-to-Raft Migration (#1072)

## Scope

Verify that `syfrah controlplane init` imports all existing redb tables,
applies a mutation freeze during cutover, and supports `--verify` for
data integrity checking.

## Prerequisites

- Single node with fabric initialized and org data created

## Test Steps

### 1. Create pre-existing data before Raft init

```bash
# Ensure some data exists before init
ssh root@65.109.130.108 "syfrah org create migrate-test"
ssh root@65.109.130.108 "syfrah project create proj --org migrate-test"
```

### 2. Run controlplane init with --verify

```bash
ssh root@65.109.130.108 "syfrah controlplane init --verify"
# Expected output:
#   Importing existing data:
#     Orgs:    N
#     VPCs:    N
#     Subnets: N
#   Mutation freeze: ACTIVE (503 on writes during migration)
#   ...
#   Mutation freeze: REMOVED
#   Verification PASSED. All data accessible.
```

### 3. Verify data accessible after migration

```bash
ssh root@65.109.130.108 "syfrah fabric stop && syfrah fabric start"
sleep 5
ssh root@65.109.130.108 "syfrah org list"
# migrate-test should be listed
```

### 4. Verify mutation freeze is transient

```bash
# The freeze sentinel should not exist after init completes
ssh root@65.109.130.108 "ls ~/.syfrah/raft_migrating 2>&1"
# Should say "No such file or directory"
```

### 5. Verify idempotent init (already initialized)

```bash
ssh root@65.109.130.108 "syfrah controlplane init"
# Should say "Control plane already initialized"
```

## Pass Criteria

- Init reports existing data counts (orgs, VPCs, subnets)
- `--verify` flag checks data integrity and reports OK/FAIL
- Mutation freeze sentinel is created and removed during init
- Data survives the migration + daemon restart
- Idempotent: second init is a no-op
