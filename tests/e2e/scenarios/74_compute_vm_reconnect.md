# Test: Daemon restart recovery — VM survives daemon restart

## Objective

- After killing the syfrah daemon and restarting, existing VMs are recovered
- The CH process survives the daemon restart
- syfrah compute vm list shows the VM after recovery
- syfrah compute vm get returns correct info

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI and daemon reconnect logic must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-reconn --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Create VM

Wait 3 seconds.

```bash
syfrah compute vm create --name test-vm-rc --vcpu 2 --memory 512 --image alpine-3.20
```

```bash
cat /run/syfrah/vms/test-vm-rc/pid
```

- Verify: VM `test-vm-rc` is in `Running` phase

### 3. Restarting syfrah daemon

Wait 3 seconds.

```bash
syfrah fabric start
```


### 4. Verify VM recovered

```bash
syfrah compute vm list --json
```

- Verify: VM `test-vm-rc` is in `Running` phase

## Expected results

- Daemon killed (PID <value>)
- CH process <value> survived daemon kill
- VM test-vm-rc recovered after daemon restart
- CH PID unchanged after daemon restart (<value>)

## Failure criteria

- Could not find daemon PID
- CH process died with daemon
- VM test-vm-rc not found after daemon restart
- CH PID changed: <value> -> <value>
