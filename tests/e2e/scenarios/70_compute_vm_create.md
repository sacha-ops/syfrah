# Test: VM creation lifecycle

## Objective

- A VM can be created with syfrah compute vm create
- The VM appears in syfrah compute vm list
- The VM phase is Running
- syfrah compute vm get returns correct fields (vcpus, memory)
- Runtime directory exists with valid metadata
- PID file exists and the process is alive

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Compute CLI (syfrah compute vm create/list/get) must be implemented
- ComputeHandler must be integrated into the daemon
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name compute-create --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Creating test VM

```bash
syfrah compute vm create --name test-vm-1 --vcpu 2 --memory 512 --image alpine-3.20
```


### 3. Verify VM in list

Wait 3 seconds.

```bash
syfrah compute vm list --json
```


### 4. Verify VM phase

- Verify: VM `test-vm-1` is in `Running` phase

### 5. Verify VM details

```bash
syfrah compute vm get test-vm-1 --json
```


## Expected results

- VM creation command succeeded
- VM test-vm-1 appears in vm list
- VM has 2 vCPUs
- VM has 512 MB memory
- Runtime directory exists
- meta.json is valid JSON
- CH process <value> is alive

## Failure criteria

- VM creation command failed
- VM test-vm-1 not in vm list
- VM vCPUs: <value> (expected 2)
- VM memory: <value> (expected 512)
- Runtime directory /run/syfrah/vms/test-vm-1 missing
- meta.json invalid or missing
- CH process <value> is not alive
- PID file missing or empty
