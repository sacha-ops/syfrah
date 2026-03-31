# Test: Forge instance CRUD endpoints

## Objective

- POST /v1/instances creates a VM via VmManager
- GET /v1/instances lists all VMs
- GET /v1/instances/:id gets a single VM
- DELETE /v1/instances/:id deletes a VM
- POST /v1/instances/:id/start|stop|reboot manage lifecycle
- Errors return FORGE_ prefix codes
- Task records are created for create/delete operations

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized
- Cloud Hypervisor binary available

## Steps

### 1. Get the Forge endpoint

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
FORGE="http://[$FORGE_IP]:7100"
```

### 2. Create an instance

```bash
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"crud-test","image":"alpine-3.20","vcpus":1,"memory_mb":512}' | jq .
```

### 3. List instances

```bash
curl -s $FORGE/v1/instances | jq .
```

### 4. Get instance by ID

```bash
curl -s $FORGE/v1/instances/crud-test | jq .
```

### 5. Stop the instance

```bash
curl -s -X POST $FORGE/v1/instances/crud-test/stop | jq .
```

### 6. Start the instance

```bash
curl -s -X POST $FORGE/v1/instances/crud-test/start | jq .
```

### 7. Delete the instance

```bash
curl -s -X DELETE $FORGE/v1/instances/crud-test | jq .
```

### 8. Verify deletion

```bash
curl -s $FORGE/v1/instances/crud-test | jq .
# Should return 404 with FORGE_INSTANCE_NOT_FOUND
```

### 9. Check tasks were created

```bash
curl -s "$FORGE/v1/tasks?resource_id=crud-test" | jq .
# Should show create_instance and delete_instance tasks
```

## Expected Results

- POST /v1/instances returns 201 with VmStatus JSON
- GET /v1/instances returns array of VmStatus objects
- GET /v1/instances/:id returns single VmStatus or 404
- DELETE /v1/instances/:id returns 200 with confirmation
- POST /v1/instances/:id/start|stop|reboot return appropriate responses
- All errors use FORGE_ prefix codes
- Task records track each mutation operation
