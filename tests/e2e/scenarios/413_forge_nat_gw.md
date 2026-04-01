# Test: Forge NAT gateway endpoints

## Objective

- POST /v1/networks/nat-gw creates a NAT GW with masquerade (Pending -> Active)
- DELETE /v1/networks/nat-gw/:id removes masquerade (Active -> Deleting -> removed)
- GET /v1/networks/nat-gw lists all NAT gateways
- State transitions are tracked: Pending -> Active -> Deleting

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized

## Steps

### 1. Initialize the mesh

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Create a bridge (NAT needs a bridge)

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-nat-test"}'
```

### 3. Create a NAT GW

```bash
BRIDGE=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-nat-test"}' | jq -r '.bridge_name')

RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/nat-gw \
  -H 'Content-Type: application/json' \
  -d "{\"bridge\":\"$BRIDGE\",\"subnet_cidr\":\"10.1.0.0/24\"}")
echo "$RESULT"
```

**Expected:** HTTP 201. Response contains `state: "Active"`, `id` starting with `nat-`.

### 4. List NAT gateways

```bash
curl -s http://[$FORGE_IP]:7100/v1/networks/nat-gw
```

**Expected:** JSON array with 1 entry in Active state.

### 5. Delete the NAT GW

```bash
NAT_ID=$(echo "$RESULT" | jq -r '.id')
curl -s -X DELETE http://[$FORGE_IP]:7100/v1/networks/nat-gw/$NAT_ID
```

**Expected:** HTTP 200 with `FORGE_NAT_DELETED`.

### 6. List should be empty

```bash
curl -s http://[$FORGE_IP]:7100/v1/networks/nat-gw
```

**Expected:** Empty JSON array.

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
