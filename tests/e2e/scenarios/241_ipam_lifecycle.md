# Test: IPAM IP allocation lifecycle

## Objective

Verify the full IP allocation lifecycle: reserve, assign, release, and orphan detection/reclamation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with org/VPC/subnet infrastructure created
- At least one subnet exists (e.g., `frontend` in VPC `default`)

## Steps

### 1. Reserve an IP from a subnet

Trigger a VM creation that allocates an IP from the subnet bitmap.

```bash
syfrah compute vm create --name lifecycle-test-1 --image alpine-3.20 \
  --subnet frontend --project backend --org acme \
  --vcpus 1 --memory 512 --ssh-key ~/.ssh/id.pub
```

- Verify: VM is created with an IP in the subnet range (e.g., `10.0.1.3`)
- Verify: `IpAllocation` record exists with `state: Assigned`
- Verify: MAC address matches the deterministic derivation (`02:00:{ip_hex}`)

### 2. Create a second VM and verify sequential allocation

```bash
syfrah compute vm create --name lifecycle-test-2 --image alpine-3.20 \
  --subnet frontend --project backend --org acme \
  --vcpus 1 --memory 512 --ssh-key ~/.ssh/id.pub
```

- Verify: Second VM gets the next sequential IP (e.g., `10.0.1.4`)
- Verify: Both allocations are listed for the subnet

### 3. Delete first VM and verify IP release

```bash
syfrah compute vm delete lifecycle-test-1 --project backend --org acme
```

- Verify: IP `10.0.1.3` is released from the bitmap
- Verify: `IpAllocation` record for `10.0.1.3` is removed
- Verify: Second VM (`10.0.1.4`) allocation is unaffected

### 4. Create a third VM and verify IP reuse

```bash
syfrah compute vm create --name lifecycle-test-3 --image alpine-3.20 \
  --subnet frontend --project backend --org acme \
  --vcpus 1 --memory 512 --ssh-key ~/.ssh/id.pub
```

- Verify: Third VM gets `10.0.1.3` (the released IP, since bitmap scans from lowest)

### 5. Orphan detection (simulated crash scenario)

This step requires the ability to interrupt VM creation between IPAM allocation and VM boot, or to inspect the IPAM state directly via `syfrah state inspect org`.

- Verify: An IP reserved but never assigned within 5 minutes is flagged as orphaned by the reconciliation loop
- Verify: The orphaned IP is reclaimed and made available for new allocations

### 6. Cleanup

```bash
syfrah compute vm delete lifecycle-test-2 --project backend --org acme
syfrah compute vm delete lifecycle-test-3 --project backend --org acme
```

- Verify: All IPs are released
- Verify: Subnet bitmap shows all IPs available (252 free)

## Expected results

- IPs are allocated sequentially starting from `.3`
- Released IPs are reusable
- MAC addresses are deterministically derived from IPs
- Orphaned allocations (Reserved but never Assigned within 5 minutes) are detected
- Orphaned IPs are reclaimed and returned to the available pool

## Failure criteria

- IP allocation returns an address outside the subnet range
- Released IP is not reusable (bitmap bit stuck)
- Orphan detection misses a stale Reserved allocation
- Orphan detection falsely flags a recently Reserved allocation (< 5 minutes old)
- Two VMs receive the same IP address
- MAC address does not match the expected deterministic derivation
