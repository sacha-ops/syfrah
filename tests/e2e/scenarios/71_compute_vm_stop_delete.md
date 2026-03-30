# Test: VM stop and delete lifecycle

## Objective

- A running VM can be stopped
- A stopped VM reaches Stopped phase
- A VM can be deleted
- Deleted VM no longer appears in list
- Runtime directory is cleaned up after delete

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI (syfrah compute vm create/stop/delete/list) must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-stopdel --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create and verify VM is running

Wait 3 seconds.

```bash
syfrah compute vm create --name test-vm-sd --vcpu 1 --memory 256 --image alpine-3.20
```

- Verify: VM `test-vm-sd` is in `Running` phase

### 3. Stopping VM

```bash
syfrah compute vm stop test-vm-sd
```

- Verify: Wait until VM `test-vm-sd` reaches `Stopped` phase (timeout 15s)
- Verify: VM `test-vm-sd` is in `Stopped` phase

### 4. Deleting VM

Wait 2 seconds.

```bash
syfrah compute vm delete test-vm-sd
```


### 5. Verify VM gone from list

```bash
syfrah compute vm list --json
```


## Expected results

- VM stop command succeeded
- VM delete command succeeded
- VM test-vm-sd removed from list
- Runtime directory cleaned up

## Failure criteria

- VM stop command failed
- VM delete command failed
- VM test-vm-sd still in list after delete
- Runtime directory still exists after delete
