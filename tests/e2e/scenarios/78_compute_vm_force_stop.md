# Test: Force stop a VM

## Objective

- A running VM can be force-stopped
- Force stop completes quickly (< 5 seconds)
- The VM reaches Stopped phase

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI (syfrah compute vm create/stop --force) must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-fstop --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create VM

Wait 3 seconds.

```bash
syfrah compute vm create --name test-vm-fs --vcpu 1 --memory 256 --image alpine-3.20
```

- Verify: VM `test-vm-fs` is in `Running` phase

### 3. Force stopping VM

```bash
syfrah compute vm stop test-vm-fs
```


### 4. Verify Stopped phase

Wait 2 seconds.

- Verify: VM `test-vm-fs` is in `Stopped` phase

## Expected results

- Force stop command succeeded
- Force stop completed in <value>s (< 5s)
- CH process terminated after force stop

## Failure criteria

- Force stop command failed
- Force stop took <value>s (expected < 5s)
- CH process <value> still alive after force stop
