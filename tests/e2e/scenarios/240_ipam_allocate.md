# Test: IPAM bitmap allocator — allocate and release IPs

## Objective

- Verify that IPAM allocates IPs sequentially starting from .3
- Verify reserved IPs (.0, .1, .2, .255) are never allocated
- Verify released IPs are reclaimed on next allocation
- Verify bitmap state persists across database reopens
- Verify pool exhaustion returns a clear error

## Prerequisites

- `syfrah-org` crate built with IPAM module
- A temporary redb database for isolation

## Steps

### 1. Allocate first IP in a fresh /24 subnet

Create a subnet with CIDR `10.0.1.0/24`. Call `allocate()`.

**Expected**: returns `10.0.1.3` (first non-reserved IP).

### 2. Allocate sequential IPs

Call `allocate()` three more times.

**Expected**: returns `10.0.1.4`, `10.0.1.5`, `10.0.1.6` in order.

### 3. Verify reserved IPs are marked allocated

Call `is_allocated()` for `.0`, `.1`, `.2`, `.255`.

**Expected**: all return `true`.

### 4. Release an IP and reallocate

Release `10.0.1.3`. Call `allocate()`.

**Expected**: returns `10.0.1.3` (the released IP is reused).

### 5. Verify persistence

Close and reopen the database. Call `is_allocated()` for previously allocated IPs.

**Expected**: all allocations are preserved across restart.

### 6. Exhaust the pool

Allocate all remaining IPs in the /24 subnet (252 total usable). Call `allocate()` one more time.

**Expected**: returns `IpExhausted` error.

### 7. Verify available count

After releasing one IP, call `available_count()`.

**Expected**: count increases by 1 after release, decreases by 1 after allocate.
