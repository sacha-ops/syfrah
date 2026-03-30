# Test: Multiple VMs on a single node

## Objective

- 3 VMs with different specs can be created
- All 3 appear in vm list
- Stopping one does not affect others
- Deleting one does not affect others
- Counts are correct at each step

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
syfrah fabric init --name test-mesh --node-name compute-multi --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Creating 3 VMs

Wait 5 seconds.

```bash
syfrah compute vm create --name vm-alpha --vcpu 1 --memory 256 --image alpine-3.20
```

```bash
syfrah compute vm create --name vm-beta --vcpu 2 --memory 512 --image alpine-3.20
```

```bash
syfrah compute vm create --name vm-gamma --vcpu 4 --memory 1024 --image alpine-3.20
```


### 3. Verify all 3 in list

- Verify: assert_vm_count "e2e-compute-multi" 3
- Verify: VM `vm-alpha` is in `Running` phase
- Verify: VM `vm-beta` is in `Running` phase
- Verify: VM `vm-gamma` is in `Running` phase

### 4. Stopping vm-beta

```bash
syfrah compute vm stop vm-beta
```

- Verify: Wait until VM `vm-beta` reaches `Stopped` phase (timeout 15s)
- Verify: VM `vm-alpha` is in `Running` phase
- Verify: VM `vm-beta` is in `Stopped` phase
- Verify: VM `vm-gamma` is in `Running` phase

### 5. Deleting vm-beta

Wait 2 seconds.

```bash
syfrah compute vm delete vm-beta
```

- Verify: assert_vm_count "e2e-compute-multi" 2

### 6. Remaining VMs unaffected

- Verify: VM `vm-alpha` is in `Running` phase
- Verify: VM `vm-gamma` is in `Running` phase

## Expected results

- 3 VMs with different specs can be created
- All 3 appear in vm list
- Stopping one does not affect others
- Deleting one does not affect others
- Counts are correct at each step

## Failure criteria

- Any syfrah command returns a non-zero exit code unexpectedly
- Expected output patterns are missing
- Timeouts exceeded waiting for convergence
