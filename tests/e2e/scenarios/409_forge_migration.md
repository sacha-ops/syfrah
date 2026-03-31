# Test: Forge migration — daemon starts HTTP server alongside control socket

## Objective

- daemon.rs starts the Forge HTTP server alongside the control socket
- Both coexist: CLI uses control socket, API consumers use HTTP
- CLI commands (`syfrah fabric`, `syfrah compute vm`, `syfrah org`) still work unchanged
- Forge HTTP API endpoints are accessible on syfrah0:7100
- No regression in existing functionality

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized

## Steps

### 1. Verify CLI still works via control socket

```bash
syfrah fabric status
syfrah state show
```

Expected: Both commands return valid output via the control socket.

### 2. Verify Forge HTTP API works

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep "inet6 fd" | awk '{print $2}' | cut -d/ -f1 | head -1)
FORGE="http://[$FORGE_IP]:7100"

curl -s $FORGE/v1/node/health | jq .
```

Expected: `{"status":"healthy","uptime":...}`

### 3. Test both paths with compute operations

```bash
# Via CLI (control socket path)
syfrah org create migration-test
syfrah project create app --org migration-test
syfrah env create staging --project app --org migration-test
syfrah subnet create web --env staging --project app --org migration-test

# Via HTTP API (Forge path)
curl -s $FORGE/v1/instances | jq .

# Via CLI again
syfrah compute vm list
```

### 4. Create via API, query via CLI

```bash
# Create via Forge API
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"migr-vm","image":"alpine-3.20","vcpus":1,"memory_mb":512,"subnet":"web"}'

# Query via CLI
syfrah compute vm list
syfrah compute vm get migr-vm
```

### 5. Query via API, delete via API

```bash
curl -s $FORGE/v1/instances/migr-vm | jq .
curl -s $FORGE/v1/tasks | jq .
curl -s -X DELETE $FORGE/v1/instances/migr-vm | jq .
```

### 6. Verify CLI still works after API operations

```bash
syfrah compute vm list
syfrah fabric status
```

## Expected Results

- Control socket and HTTP server both run simultaneously
- CLI commands work unchanged via control socket
- HTTP API endpoints return correct responses
- VMs created via API are visible via CLI and vice versa
- No interference between the two paths
- Daemon is the single process entry point for both
