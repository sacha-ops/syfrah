# Test: IPAM subnet exhaustion returns actionable error

## Objective

- Verify that allocating all 252 usable IPs in a /24 subnet produces a clear, actionable error on the next attempt
- Verify the error message names the subnet and suggests creating a new subnet
- Verify that releasing an IP allows allocation to resume

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with an initialized mesh
- An org, project, environment, and subnet exist (or will be created in steps below)

## Steps

### 1. Set up org hierarchy and subnet

```bash
syfrah org create exhaust-test
syfrah project create backend --org exhaust-test
syfrah env create prod --project backend --org exhaust-test
syfrah subnet create full-net --env prod --project backend --org exhaust-test
```

### 2. Exhaust the subnet

Create 252 VMs to consume every allocatable IP (.3 through .254):

```bash
for i in $(seq 1 252); do
  syfrah compute vm create \
    --name "vm-${i}" \
    --image alpine-3.20 \
    --subnet full-net \
    --project backend \
    --org exhaust-test \
    --vcpus 1 \
    --memory 512
done
```

Verify all 252 VMs are running and each received a unique IP.

### 3. Attempt one more allocation

```bash
syfrah compute vm create \
  --name vm-overflow \
  --image alpine-3.20 \
  --subnet full-net \
  --project backend \
  --org exhaust-test \
  --vcpus 1 \
  --memory 512
```

### 4. Release an IP and retry

```bash
syfrah compute vm delete vm-252 --project backend --org exhaust-test
syfrah compute vm create \
  --name vm-replacement \
  --image alpine-3.20 \
  --subnet full-net \
  --project backend \
  --org exhaust-test \
  --vcpus 1 \
  --memory 512
```

## Expected results

1. Steps 1–2 succeed: 252 VMs are created, each with a unique IP in 10.x.x.3–10.x.x.254.
2. Step 3 fails with an error message that:
   - Contains the subnet name `full-net`
   - Shows `0/252` (zero available out of 252 total)
   - Includes the text "Create a new subnet to add capacity"
   - Exit code is non-zero
3. Step 4 succeeds: after deleting a VM, the released IP is re-allocated to the new VM.

## Failure criteria

- The error message is a raw internal error (e.g., panic, redb error, "index out of bounds") instead of the actionable `IpExhausted` message.
- The error does not mention the subnet name.
- The error does not suggest creating a new subnet.
- The system silently fails (no error) or hangs when the subnet is full.
- Releasing an IP does not make it available for the next allocation.
