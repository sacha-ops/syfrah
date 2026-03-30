# Test: Container runtime — create a real container (no KVM needed)

## Objective

- syfrah compute status shows "container" runtime
- An image can be pulled from the catalog
- A VM (actually a gVisor container) can be created
- The container reaches Running phase
- The container process (PID) is alive
- The container can be stopped and deleted

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Container runtime (`crun` / `runsc`) installed
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name container-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Step 1: Check compute status for container runtime

```bash
syfrah compute status
```

```bash
syfrah compute status --json
```


### 3. Step 2: Pull alpine-3.20 image

```bash
sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json'
```


### 4. Step 3: Create VM (container-backed via gVisor/crun)

```bash
syfrah compute vm create --name ctr-test-1 --image alpine-3.20 --vcpus 1 --memory 256
```


### 5. Step 4: Verify container reaches Running phase

```bash
syfrah compute vm get ctr-test-1 --json
```


### 6. Step 6: Stop the container

```bash
syfrah compute vm stop ctr-test-1
```


### 7. Step 7: Delete the container

```bash
syfrah compute vm delete ctr-test-1
```

```bash
syfrah compute vm list --json
```


## Expected results

- compute status shows container runtime
- compute status JSON shows container runtime
- compute status returned (runtime detection may vary)
- Image alpine-3.20 pulled successfully
- Container VM creation command accepted
- Container ctr-test-1 reached Running
- Container process <value> is alive
- Container ctr-test-1 stopped
- Container ctr-test-1 deleted

## Failure criteria

- compute status shows degraded/unavailable instead of container runtime
- Failed to pull alpine-3.20 after 3 attempts
- Container VM creation failed
- Container ctr-test-1 did not reach Running (current: unknown)
- Container process <value> is not alive
- PID file missing or empty for ctr-test-1
- Container ctr-test-1 did not stop
- Container ctr-test-1 still in list after delete
