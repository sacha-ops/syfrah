# Test: NIC CRUD operations

## Objective

Verify that NetworkInterface (NIC) records can be created, listed, attached to security groups, and deleted through the store layer.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with a VPC, subnet, and at least one VM

## Steps

### 1. Create a VM with networking — NIC auto-created

```bash
syfrah vm create test-nic-vm --vpc default --subnet default --image ubuntu-22.04
```

- Verify: VM is created successfully
- Verify: A NIC record exists for the VM (visible in state DB)
- Verify: NIC has a valid private IP from the subnet CIDR
- Verify: NIC has a MAC address derived from the IP (format `02:00:xx:xx:xx:xx`)
- Verify: NIC state is `Active`
- Verify: NIC has the default security group attached

### 2. List NICs by VM

- Verify: Listing NICs for `test-nic-vm` returns exactly 1 NIC
- Verify: The NIC's `vm_id` matches the created VM
- Verify: The NIC's `subnet_id` and `vpc_id` match the requested subnet/VPC

### 3. List NICs by subnet

- Verify: Listing NICs for the default subnet includes the NIC created in step 1
- Verify: Listing NICs for a non-existent subnet returns an empty list

### 4. Attach a second security group to the NIC

- Verify: Attaching a new SG to the NIC succeeds
- Verify: The NIC's `security_groups` list now contains both the default SG and the new SG
- Verify: Attaching the same SG again is a no-op (idempotent)

### 5. Detach a security group from the NIC

- Verify: Detaching the second SG succeeds
- Verify: The NIC's `security_groups` list contains only the default SG
- Verify: Detaching a SG that is not attached returns an error

### 6. Delete the VM — NIC cleaned up

```bash
syfrah vm delete test-nic-vm
```

- Verify: VM deletion succeeds
- Verify: The NIC transitions to `Deleted` state
- Verify: The NIC's IP is released back to IPAM
