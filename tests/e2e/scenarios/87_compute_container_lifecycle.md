# Test: Full container lifecycle — create, list, get, stop, start, delete

## Objective

- Multiple containers can be created
- All containers appear in vm list
- vm get returns correct spec fields
- Containers can be stopped and restarted
- Containers can be deleted
- Final list is empty after full cleanup

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- Container runtime (`crun` / `runsc`) installed
- Compute module enabled in the daemon

## Steps

### 1. Initialize the daemon

```bash
syfrah fabric init --name test-mesh --node-name lifecycle-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be fully ready (~2 seconds).

### 2. Pulling alpine-3.20 for lifecycle tests

```bash
sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json'
```


### 3. Step 1: Create container life-a

```bash
syfrah compute vm create --name life-a --image alpine-3.20 --vcpus 1 --memory 256
```


### 4. Step 1b: Create container life-b

Wait 3 seconds.

```bash
syfrah compute vm create --name life-b --image alpine-3.20 --vcpus 2 --memory 512
```


### 5. Step 2: Verify both containers in vm list

```bash
syfrah compute vm list --json
```


### 6. Step 3: Verify vm get fields

```bash
syfrah compute vm get life-a --json
```

```bash
syfrah compute vm get life-b --json
```


### 7. Step 4: Verify both containers Running

- Verify: Wait until VM `life-a` reaches `Running` phase (timeout 30s)
- Verify: VM `life-a` is in `Running` phase
- Verify: VM `life-b` is in `Running` phase

### 8. Step 5: Stop life-a

```bash
syfrah compute vm stop life-a
```

- Verify: VM `life-b` is in `Running` phase

### 9. Step 6: Restart life-a

```bash
syfrah compute vm start life-a
```

```bash
syfrah compute vm get life-a --json
```


### 10. Step 7: Delete all containers

```bash
syfrah compute vm delete life-a
```

```bash
syfrah compute vm delete life-b
```

```bash
syfrah compute vm list --json
```


## Expected results

- Image ready
- Container life-a creation accepted
- Container life-b creation accepted
- vm list shows 2 containers
- life-a in list
- life-b in list
- life-b has 2 vCPUs
- life-b has 512 MB memory
- life-a stopped
- life-a restarted to Running
- All containers deleted — list is empty

## Failure criteria

- Failed to pull alpine-3.20 — cannot proceed
- Container life-a creation failed
- Container life-b creation failed
- vm list shows <value> containers (expected 2)
- life-a missing from list
- life-b missing from list
- life-b vCPUs: <value> (expected 2)
- life-b memory: <value> (expected 512)
- life-a did not stop
- life-a restart failed (phase: unknown)
- After delete, vm list still has <value> entries
