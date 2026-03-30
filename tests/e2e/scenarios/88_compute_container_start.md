# Test: Container runtime — vm start for stopped containers

## Objective

- A running container can be stopped
- `vm start` brings a stopped container back to Running
- Idempotent start on an already-running VM succeeds (or is a no-op)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Container runtime (`crun` / `runsc`) installed
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name start-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Pulling alpine-3.20 for start tests

```bash
sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json'
```


### 3. Step 1: Create container start-vm

```bash
syfrah compute vm create --name start-vm --image alpine-3.20 --vcpus 1 --memory 256
```


### 4. Step 3: Stop start-vm

```bash
syfrah compute vm stop start-vm
```


### 5. Step 4: Start start-vm (from Stopped)

```bash
syfrah compute vm start start-vm
```

```bash
syfrah compute vm get start-vm --json
```


### 6. Step 5: Start start-vm again (already Running — idempotent)

```bash
syfrah compute vm start start-vm
```

```bash
syfrah compute vm get start-vm --json
```


### 7. Cleanup

```bash
syfrah compute vm delete start-vm
```


## Expected results

- Image ready
- Container start-vm creation accepted
- start-vm reached Running
- start-vm stopped
- start-vm returned to Running after vm start
- Idempotent start: VM still Running

## Failure criteria

- Failed to pull alpine-3.20 — cannot proceed
- Container start-vm creation failed
- start-vm did not reach Running
- start-vm did not stop
- start-vm did not return to Running (phase: unknown)
- Idempotent start changed phase to: unknown
