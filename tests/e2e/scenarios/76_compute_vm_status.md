# Test: Compute status endpoint with mixed VM states

## Objective

- syfrah compute status reports correct total VM count
- syfrah compute status reports correct running VM count
- Counts update correctly when VMs are stopped

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI (syfrah compute status, vm create/stop) must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-status --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create 2 VMs

Wait 5 seconds.

```bash
syfrah compute vm create --name vm-run --vcpu 1 --memory 256 --image alpine-3.20
```

```bash
syfrah compute vm create --name vm-stop --vcpu 1 --memory 256 --image alpine-3.20
```

- Verify: VM `vm-run` is in `Running` phase
- Verify: VM `vm-stop` is in `Running` phase

### 3. Stop one VM

```bash
syfrah compute vm stop vm-stop
```

- Verify: Wait until VM `vm-stop` reaches `Stopped` phase (timeout 15s)

### 4. Checking compute status

```bash
syfrah compute status --json
```


## Expected results

- total_vms = 2
- running_vms = 1

## Failure criteria

- total_vms = <value> (expected 2)
- running_vms = <value> (expected 1)
