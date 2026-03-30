# Test: Container runtime — runtime field in vm list and vm get

## Objective

- `vm list` output contains a RUNTIME column
- The RUNTIME column shows "container" for container-backed VMs
- `vm get --json` includes a runtime field with value "container"

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Container runtime (`crun` / `runsc`) installed
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name runtime-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Pulling alpine-3.20 for runtime field tests

```bash
sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json'
```


### 3. Step 1: Create container rt-vm

```bash
syfrah compute vm create --name rt-vm --image alpine-3.20 --vcpus 1 --memory 256
```


### 4. Step 3: Verify RUNTIME column in vm list

```bash
syfrah compute vm list
```


### 5. Step 5: Verify runtime field in vm list --json

```bash
syfrah compute vm list --json
```


### 6. Step 6: Verify Runtime field in vm get --json

```bash
syfrah compute vm get rt-vm --json
```


### 7. Cleanup

```bash
syfrah compute vm delete rt-vm
```


## Expected results

- Image ready
- Container rt-vm creation accepted
- rt-vm reached Running
- vm list output contains RUNTIME column header
- vm list shows 'container' runtime for rt-vm
- vm list --json shows runtime 'container' for rt-vm
- vm list --json shows runtime 'container' (alternate field name)
- vm get --json shows Runtime 'container' for rt-vm
- vm get --json shows Runtime 'container' (alternate field name)

## Failure criteria

- Failed to pull alpine-3.20 — cannot proceed
- Container rt-vm creation failed
- rt-vm did not reach Running
- vm list output missing RUNTIME column header
- vm list does not show 'container' runtime
- vm list --json missing runtime field for rt-vm
- vm get --json missing Runtime field for rt-vm
