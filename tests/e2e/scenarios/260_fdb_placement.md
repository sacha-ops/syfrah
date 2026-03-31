# Test: FDB VM placement persistence

## Objective

- A VM placement record can be created and retrieved
- A VM placement record can be deleted
- Placements can be listed by VPC (only returns entries for the queried VPC)
- Placements can be listed by hosting node (only returns entries for the queried node)
- Deleting a non-existent placement returns an error

## Prerequisites

- The `syfrah-org` crate is built
- No daemon required (pure store tests against redb)

## Steps

### 1. Add a placement

Create a `VmPlacement` record with action `Add` for vpc `vpc-1`, vm `vm-web-1`, MAC `02:00:0a:01:01:03`, IP `10.1.1.3`, subnet `subnet-frontend`, hosting node `fd00::1`.

Store it via `PlacementStore::add_placement()`.

**Expected**: `get_placement("vpc-1", "vm-web-1")` returns the stored record with all fields matching.

### 2. Add additional placements

Add two more placements:
- `vpc-1 / vm-web-2` on node `fd00::2`, subnet `subnet-frontend`
- `vpc-2 / vm-db-1` on node `fd00::1`, subnet `subnet-database`

**Expected**: all three records are individually retrievable.

### 3. List by VPC

Call `list_by_vpc("vpc-1")`.

**Expected**: returns exactly 2 placements (`vm-web-1` and `vm-web-2`), both with `vpc_id == "vpc-1"`.

Call `list_by_vpc("vpc-2")`.

**Expected**: returns exactly 1 placement (`vm-db-1`).

### 4. List by node

Call `list_by_node("fd00::1")`.

**Expected**: returns exactly 2 placements (`vm-web-1` from vpc-1 and `vm-db-1` from vpc-2).

Call `list_by_node("fd00::2")`.

**Expected**: returns exactly 1 placement (`vm-web-2`).

### 5. Remove a placement

Call `remove_placement("vpc-1", "vm-web-1")`.

**Expected**: succeeds. `get_placement("vpc-1", "vm-web-1")` returns `None`. `list_by_vpc("vpc-1")` returns 1 entry.

### 6. Remove non-existent placement

Call `remove_placement("vpc-1", "vm-web-1")` again.

**Expected**: returns `NotFound` error.

## Validation

- All assertions pass in `cargo test -p syfrah-org`
- `cargo clippy -p syfrah-org` reports no warnings
- `cargo fmt -- --check` reports no formatting issues
