# Test: Forge HTTP server on syfrah0:7100

## Objective

- Forge HTTP server starts alongside the daemon
- Binds to the fabric IPv6 address on port 7100
- GET /v1/node/health returns healthy status with uptime
- Server is only reachable from within the WireGuard mesh

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Initialize the mesh

```bash
rm -rf ~/.syfrah/*.redb
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/fabric.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
```

### 2. Get the fabric IPv6 address

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
echo "Forge IP: $FORGE_IP"
```

### 3. Test the health endpoint

```bash
HEALTH=$(curl -s http://[$FORGE_IP]:7100/v1/node/health)
echo "$HEALTH"
```

### 4. Validate the response

```bash
STATUS=$(echo "$HEALTH" | jq -r '.status')
UPTIME=$(echo "$HEALTH" | jq -r '.uptime')
```

## Expected Results

- The daemon starts successfully
- `syfrah0` interface exists with a ULA IPv6 address
- `curl http://[$FORGE_IP]:7100/v1/node/health` returns HTTP 200
- Response contains `{"status": "healthy", "uptime": N}` where N >= 0
- The server is NOT reachable on the public IP (port 7100 is only on syfrah0)

## Cleanup

```bash
syfrah fabric leave --force
```
