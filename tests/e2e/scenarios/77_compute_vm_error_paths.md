# Test: Compute error handling

## Objective

- Creating a VM with invalid spec (vcpus=0) returns an error
- Stopping a non-existent VM returns an error or not-found
- Deleting a non-existent VM returns an error or succeeds (idempotent)
- Creating a duplicate-named VM returns an error
- No leaked processes or runtime dirs after errors

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI (syfrah compute vm create/stop/delete) must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-errors --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Creating VM with vcpus=0 (should fail)

```bash
syfrah compute vm create --name bad-vm --vcpu 0 --memory 256 --image alpine-3.20) || EXIT_CODE=$?
```


### 3. Stopping non-existent VM (should fail)

```bash
syfrah compute vm stop ghost-vm
```


### 4. Deleting non-existent VM

```bash
syfrah compute vm delete ghost-vm
```


### 5. Creating VM, then creating duplicate

Wait 3 seconds.

```bash
syfrah compute vm create --name dup-vm --vcpu 1 --memory 256 --image alpine-3.20
```

```bash
syfrah compute vm create --name dup-vm --vcpu 1 --memory 256 --image alpine-3.20) || EXIT_CODE=$?
```


## Expected results

- VM creation with vcpus=0 failed as expected
- Stopping non-existent VM failed as expected
- Stopping non-existent VM returned not-found message
- Deleting non-existent VM returned success (idempotent)
- Deleting non-existent VM returned not-found
- Duplicate VM creation failed as expected
- No leaked runtime directory for bad-vm
- No leaked runtime directory for ghost-vm

## Failure criteria

- VM creation with vcpus=0 should have failed
- Stopping non-existent VM unexpectedly succeeded
- Deleting non-existent VM unexpected result (<value>)
- Duplicate VM creation should have failed
- Runtime directory leaked for bad-vm
- Runtime directory leaked for ghost-vm
