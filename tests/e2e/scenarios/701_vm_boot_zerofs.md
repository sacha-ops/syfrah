# Test: VM boot from ZeroFS-backed root volume

## Objective

- A VM (or container fallback) can be created with `--disk-size 20`
- A root volume is auto-created and visible in `syfrah volume list`
- The VM/container boots successfully and reaches Running phase
- The guest can write data to the root disk
- Data persists across stop/start cycles (ZeroFS → S3 round-trip)
- Deleting the VM auto-deletes the root volume

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Compute module enabled in the daemon
- Storage module (ZeroFS) configured or available
- Either KVM (`/dev/kvm`) for Cloud Hypervisor **or** container runtime (`crun`) as fallback

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name zerofs-boot --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Detect runtime (KVM vs container)

```bash
test -c /dev/kvm && echo "kvm" || echo "container"
```

If KVM is available, proceed with Cloud Hypervisor VM path. Otherwise, use container (crun) fallback.

### 3. Create VM with explicit disk size

```bash
syfrah compute vm create --name zerofs-vm-1 --vcpus 1 --memory 512 --image alpine-3.20 --disk-size 20
```

This should auto-create a ZeroFS-backed root volume.

### 4. Verify root volume exists

```bash
syfrah volume list --json
```

- Verify: at least one volume exists
- Verify: a volume is associated with `zerofs-vm-1`

### 5. Verify VM reaches Running phase

```bash
syfrah compute vm get zerofs-vm-1 --json
```

- Verify: phase is `Running`
- Verify: `root_volume_id` field is present and non-empty

### 6. Verify guest can write to root disk

For KVM path:
```bash
syfrah compute vm exec zerofs-vm-1 -- sh -c "echo 'zerofs-persist-test' > /tmp/persist.txt && cat /tmp/persist.txt"
```

For container path:
```bash
syfrah compute vm exec zerofs-vm-1 -- sh -c "echo 'zerofs-persist-test' > /tmp/persist.txt && cat /tmp/persist.txt"
```

- Verify: output contains `zerofs-persist-test`

### 7. Stop the VM/container

```bash
syfrah compute vm stop zerofs-vm-1
```

- Verify: VM phase transitions to `Stopped`

### 8. Restart and verify data persistence

```bash
syfrah compute vm start zerofs-vm-1
```

Wait for Running phase.

```bash
syfrah compute vm exec zerofs-vm-1 -- cat /tmp/persist.txt
```

- Verify: output contains `zerofs-persist-test` (data survived restart via S3)

### 9. Delete VM and verify volume cleanup

```bash
syfrah compute vm delete zerofs-vm-1 --yes
```

```bash
syfrah volume list --json
```

- Verify: `zerofs-vm-1` no longer in vm list
- Verify: root volume auto-deleted from volume list

## Expected results

- Runtime detection succeeds (KVM or container)
- VM creation with `--disk-size 20` accepted
- Root volume appears in `syfrah volume list`
- VM reaches Running phase
- `root_volume_id` present in VM JSON
- Guest writes to root disk succeed
- VM stops cleanly
- Data persists after restart (ZeroFS → S3 round-trip)
- VM deletes cleanly
- Root volume auto-deleted after VM deletion

## Failure criteria

- VM creation failed
- No root volume created (volume list empty after VM create)
- VM did not reach Running (current: unknown)
- `root_volume_id` missing from VM JSON
- Guest write failed or returned unexpected output
- VM did not stop within timeout
- Data did NOT persist after restart
- VM deletion failed
- Root volume still exists after VM deletion
