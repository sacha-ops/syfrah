# Test: Forge task engine with API endpoints

## Objective

- Task records are created for every mutation operation
- Tasks track state: Pending → Running → Completed|Failed
- API endpoints expose tasks for observability
- GET /v1/tasks/:id returns a single task
- GET /v1/tasks?resource_id=X filters tasks by resource

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized

## Steps

### 1. Get the fabric IPv6 address

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. List all tasks (should be empty initially)

```bash
curl -s http://[$FORGE_IP]:7100/v1/tasks | jq .
# Should return []
```

### 3. Create a VM (triggers task creation)

```bash
curl -X POST http://[$FORGE_IP]:7100/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"task-test-vm","image":"alpine-3.20","vcpus":1,"memory_mb":512,"subnet":"frontend","project":"backend","org":"acme"}'
```

### 4. List tasks again

```bash
curl -s http://[$FORGE_IP]:7100/v1/tasks | jq .
# Should return at least one task with operation "create_instance"
```

### 5. Get a specific task

```bash
TASK_ID=$(curl -s http://[$FORGE_IP]:7100/v1/tasks | jq -r '.[0].id')
curl -s http://[$FORGE_IP]:7100/v1/tasks/$TASK_ID | jq .
```

### 6. Filter tasks by resource

```bash
curl -s "http://[$FORGE_IP]:7100/v1/tasks?resource_id=task-test-vm" | jq .
```

## Expected Results

- GET /v1/tasks returns array of task objects
- Each task has: id, resource_id, operation, state, created_at
- GET /v1/tasks/:id returns a single task or 404
- GET /v1/tasks?resource_id=X filters correctly
- Task states are Pending, Running, Completed, or Failed

## Cleanup

```bash
curl -X DELETE http://[$FORGE_IP]:7100/v1/instances/task-test-vm
```
