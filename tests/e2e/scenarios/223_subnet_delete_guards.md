# Test: Subnet deletion guards

## Objective

- Verify that a subnet with active VMs cannot be deleted
- Verify that an empty subnet (no VMs) can be deleted
- Verify that the error message is clear and actionable

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing orgs, projects, environments, VPCs, or subnets)

## Steps

### 1. Set up the hierarchy

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
```

Expected: all three commands succeed.

### 2. Create a subnet

```bash
syfrah subnet create frontend --env production --project backend --org acme
```

Expected output confirms subnet creation with an auto-allocated CIDR (e.g. `10.0.1.0/24`) and gateway `.1`.

### 3. Delete an empty subnet

```bash
syfrah subnet delete frontend
```

Expected: deletion succeeds since no VMs reference this subnet.

### 4. Re-create the subnet and attach a VM

```bash
syfrah subnet create frontend --env production --project backend --org acme
syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend --project backend --org acme --vcpus 1 --memory 512
```

Expected: subnet and VM are created successfully.

### 5. Attempt to delete subnet with active VMs

```bash
syfrah subnet delete frontend
```

Expected: error indicating the subnet cannot be deleted because it has active VMs. The error message must include:
- The subnet name (`frontend`)
- The number of active VMs (e.g. `1`)
- A clear phrase like "active VM(s)"

Exit code non-zero.

### 6. Delete the VM, then delete the subnet

```bash
syfrah compute vm delete web-1 --project backend --org acme
syfrah subnet delete frontend
```

Expected: after deleting the VM, the subnet deletion succeeds.

## Pass criteria

- Empty subnets can be deleted without error
- Subnets with active VMs are rejected with a clear error: "cannot delete subnet 'X': has N active VM(s)"
- After removing all VMs, the subnet can be deleted
- Non-existent subnet deletion returns a "not found" error

## Failure criteria

- Subnet with VMs is silently deleted (data loss)
- Error message does not mention the subnet name or VM count
- Deletion of a non-existent subnet succeeds silently
