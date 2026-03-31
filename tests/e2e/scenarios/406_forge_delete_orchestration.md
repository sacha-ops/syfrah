# Test: Forge delete orchestration in reverse dependency order

## Objective

- DELETE /v1/instances/:id performs full reverse-dependency cleanup
- Order: stop VM -> FDB remove -> IPAM release -> NIC delete -> TAP delete -> nftables remove -> bridge cleanup
- Best-effort: errors are logged but don't fail the delete
- Capacity is released even on partial failure
- Task record tracks the delete operation

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized
- Org hierarchy created and a VM instance running

## Steps

### 1. Set up and create a VM

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep "inet6 fd" | awk '{print $2}' | cut -d/ -f1 | head -1)
FORGE="http://[$FORGE_IP]:7100"

# Create org hierarchy
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
syfrah subnet create frontend --env production --project backend --org acme
syfrah nat-gw create main-gw --vpc acme-backend-default --subnet frontend

# Create a VM to delete
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"del-test","image":"alpine-3.20","vcpus":1,"memory_mb":512,"subnet":"frontend"}'
```

### 2. Verify network resources exist before delete

```bash
ip link show syft-del-test 2>/dev/null && echo "TAP exists"
ip link show syfb-acme-backend-default 2>/dev/null && echo "Bridge exists"
```

### 3. Delete the instance

```bash
curl -s -X DELETE $FORGE/v1/instances/del-test | jq .
```

Expected: 200 with FORGE_INSTANCE_DELETED

### 4. Verify network resources cleaned up

```bash
# TAP should be gone
ip link show syft-del-test 2>/dev/null || echo "TAP cleaned up"

# Bridge may or may not be cleaned up depending on other VMs
```

### 5. Verify task tracking

```bash
curl -s "$FORGE/v1/tasks?resource_id=del-test" | jq .
# Should show delete_instance task in Completed state
```

### 6. Verify instance is gone

```bash
curl -s $FORGE/v1/instances/del-test | jq .
# Should return 404 with FORGE_INSTANCE_NOT_FOUND
```

### 7. Delete nonexistent instance

```bash
curl -s -X DELETE $FORGE/v1/instances/ghost-vm | jq .
# Should return error (VM not found)
```

## Expected Results

- Delete performs reverse-dependency cleanup (stop, FDB, IPAM, NIC, TAP, nftables, bridge)
- Network resources (TAP, FDB entries) are cleaned up
- Capacity is released
- Task record shows Completed or Failed with error details
- Best-effort: partial cleanup failures are logged but delete returns 200 if VM is removed
- Deleting nonexistent VM returns an error
