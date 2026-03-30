# Test: Container runtime — container reconnect after daemon restart

## Objective

- A container VM can be created and reaches Running
- After `fabric stop` + `fabric start`, the daemon reconnects
- The container VM still appears in `vm list`
- The container VM is still in Running phase (or recovers to it)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Container runtime (`crun` / `runsc`) installed
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name reconn-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Pulling alpine-3.20 for reconnect tests

```bash
sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json'
```


### 3. Step 1: Create container reconn-vm

```bash
syfrah compute vm create --name reconn-vm --image alpine-3.20 --vcpus 1 --memory 256
```


### 4. Step 3: Stop daemon (fabric stop)

Wait 2 seconds.

```bash
syfrah fabric stop
```


### 5. Step 4: Restart daemon (fabric start)

```bash
syfrah fabric start
```


### 6. Step 5: Verify reconn-vm in vm list after daemon restart

Wait 3 seconds.

```bash
syfrah compute vm list --json
```


### 7. Step 6: Verify reconn-vm phase after reconnect

```bash
syfrah compute vm get reconn-vm --json
```


### 8. Cleanup

```bash
syfrah compute vm delete reconn-vm
```


## Expected results

- Image ready
- Container reconn-vm creation accepted
- reconn-vm reached Running
- Daemon stopped
- Daemon restarted
- reconn-vm still in vm list after daemon restart
- reconn-vm is Running after daemon restart
- reconn-vm is Stopped after daemon restart (container may not survive daemon cycle)

## Failure criteria

- Failed to pull alpine-3.20 — cannot proceed
- Container reconn-vm creation failed
- reconn-vm did not reach Running
- reconn-vm missing from vm list after daemon restart
- reconn-vm unexpected phase after restart: unknown
