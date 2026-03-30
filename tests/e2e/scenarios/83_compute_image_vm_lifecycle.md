# Test: VM lifecycle with real catalog images

## Objective

- A VM can be created using the real alpine-3.20 image from the catalog
- The VM reaches Running phase
- The instance directory contains a cloned rootfs (non-zero size)
- The VM can be stopped and deleted cleanly

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- KVM support (`/dev/kvm`) or container runtime fallback
- Compute module enabled in the daemon
- Docker image built with real images from syfrah-images catalog
- Compute CLI must be implemented
- Fake cloud-hypervisor must be installed in the Docker image

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name image-vm --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Creating VM with real alpine-3.20 image

```bash
syfrah compute vm create --name real-vm --vcpu 1 --memory 256 --image alpine-3.20
```


### 3. Checking instance rootfs size

```bash
sh -c 'ls -d /opt/syfrah/instances/*/rootfs.raw
```


### 4. Stopping VM

```bash
syfrah compute vm stop real-vm
```

- Verify: Wait until VM `real-vm` reaches `Stopped` phase (timeout 15s)
- Verify: VM `real-vm` is in `Stopped` phase

### 5. Deleting VM

Wait 2 seconds.

```bash
syfrah compute vm delete real-vm
```

```bash
syfrah compute vm list --json
```


## Expected results

- VM creation with real image succeeded
- instance rootfs is a real file (<value>) MB)
- VM running (instance layout check skipped)
- VM real-vm cleaned up

## Failure criteria

- VM creation with real image failed
- instance rootfs is too small (<value> bytes)
- VM real-vm still in list after delete
