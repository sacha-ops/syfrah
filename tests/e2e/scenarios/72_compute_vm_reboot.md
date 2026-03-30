# Test: VM reboot

## Objective

- A running VM can be rebooted
- The VM returns to Running phase after reboot

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI (syfrah compute vm create/list) and reboot support
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-reboot --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create VM

Wait 3 seconds.

```bash
syfrah compute vm create --name test-vm-rb --vcpu 1 --memory 256 --image alpine-3.20
```

- Verify: VM `test-vm-rb` is in `Running` phase

### 3. Rebooting VM

```bash
syfrah compute vm reboot "test-vm-rb"
```


### 4. Verify VM returns to Running

Wait 3 seconds.

- Verify: VM `test-vm-rb` is in `Running` phase

## Expected results

- VM reboot command succeeded
- CH process still alive after reboot

## Failure criteria

- VM reboot command failed
- CH process not alive after reboot
