# Test: Forge admission control + resource reservation

## Objective

- Admission control checks allocatable capacity before create
- Reject 409 Conflict when insufficient resources
- 60s reservation between admission and creation
- Capacity released on failure or timeout
- Overcommit ratios: CPU 2:1, memory 1:1

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized

## Steps

### 1. Get Forge endpoint

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep "inet6 fd" | awk '{print $2}' | cut -d/ -f1 | head -1)
FORGE="http://[$FORGE_IP]:7100"
```

### 2. Check node health (shows capacity info)

```bash
curl -s $FORGE/v1/node/health | jq .
```

### 3. Create a VM within capacity

```bash
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"admit-vm1","image":"alpine-3.20","vcpus":1,"memory_mb":512}' | jq .
```

Expected: 201 Created

### 4. Attempt to create a VM exceeding capacity

```bash
# Request absurdly large resources that will exceed any node
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"huge-vm","image":"alpine-3.20","vcpus":9999,"memory_mb":999999}' | jq .
```

Expected: 409 Conflict with FORGE_INSUFFICIENT_CAPACITY

### 5. Clean up

```bash
curl -s -X DELETE $FORGE/v1/instances/admit-vm1 | jq .
```

### 6. Verify capacity released after delete

```bash
# Should be able to create again
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"admit-vm2","image":"alpine-3.20","vcpus":1,"memory_mb":512}' | jq .
curl -s -X DELETE $FORGE/v1/instances/admit-vm2 | jq .
```

## Expected Results

- Normal creates succeed with 201
- Oversized creates are rejected with 409 FORGE_INSUFFICIENT_CAPACITY
- Capacity is released after delete, allowing new creates
- Overcommit: CPU 2:1 allows more vCPUs than physical cores
- Memory: 1:1 ratio with 1GB reserved for host
