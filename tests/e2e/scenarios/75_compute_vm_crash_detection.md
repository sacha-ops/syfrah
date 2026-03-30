# Test: CH process crash detection

## Objective

- When the CH process is killed directly, the monitor detects it
- The VM transitions to Failed phase
- The failed VM appears correctly in vm list
- The failed VM can be deleted and cleaned up

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI and process monitor must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-crash --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create VM

Wait 3 seconds.

```bash
syfrah compute vm create --name test-vm-crash --vcpu 1 --memory 256 --image alpine-3.20
```

- Verify: VM `test-vm-crash` is in `Running` phase

### 3. Waiting for crash detection (up to 15s)

- Verify: Wait until VM `test-vm-crash` reaches `Failed` phase (timeout 15s)
- Verify: VM `test-vm-crash` is in `Failed` phase

### 4. Verify failed VM in list

```bash
syfrah compute vm list --json
```


### 5. Deleting failed VM

Wait 2 seconds.

```bash
syfrah compute vm delete test-vm-crash
```

- Verify: assert_vm_count "e2e-compute-crash" 0

## Expected results

- CH process killed
- Failed VM still visible in list
- Runtime directory cleaned up after deleting failed VM

## Failure criteria

- Could not find CH PID
- Failed VM not in list
- Runtime directory still exists after deleting failed VM
