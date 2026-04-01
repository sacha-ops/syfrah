# Test: Forge route table enforcement

## Objective

- POST /v1/networks/routes/enforce applies blackhole routes as nftables DROP
- Validates route targets (NAT GW must be Active, peering must have target_id)
- Inactive NAT GW targets produce descriptive errors
- Unknown target types are rejected with errors
- Partial success: some routes may succeed while others fail

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized
- `nft` (nftables) available

## Steps

### 1. Initialize and get Forge IP

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Enforce a blackhole route

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d '{"routes":[{"destination":"10.99.0.0/24","target_type":"blackhole"}]}')
echo "$RESULT"
```

**Expected:** HTTP 200, `applied: 1`, `errors: []`.

### 3. Verify nftables DROP rule

```bash
nft list table inet syfrah_routes 2>/dev/null
```

**Expected:** Contains a DROP rule for 10.99.0.0/24.

### 4. Create NAT GW for target validation

```bash
BRIDGE=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-route-test"}' | jq -r '.bridge_name')

NAT_RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/nat-gw \
  -H 'Content-Type: application/json' \
  -d "{\"bridge\":\"$BRIDGE\",\"subnet_cidr\":\"10.1.0.0/24\"}")
NAT_ID=$(echo "$NAT_RESULT" | jq -r '.id')
echo "NAT GW: $NAT_ID"
```

### 5. Enforce route with valid Active NAT GW target

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d "{\"routes\":[{\"destination\":\"10.2.0.0/24\",\"target_type\":\"nat-gw\",\"target_id\":\"$NAT_ID\"}]}")
echo "$RESULT"
```

**Expected:** HTTP 200, `applied: 1`.

### 6. Enforce route with non-existent NAT GW target

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d '{"routes":[{"destination":"10.3.0.0/24","target_type":"nat-gw","target_id":"nat-nonexistent"}]}')
echo "$RESULT"
```

**Expected:** `applied: 0`, `errors` array with "NAT GW not found".

### 7. Enforce peering route with target_id

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d '{"routes":[{"destination":"10.4.0.0/24","target_type":"peering","target_id":"peer-abc"}]}')
echo "$RESULT"
```

**Expected:** HTTP 200, `applied: 1`.

### 8. Enforce peering route without target_id

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d '{"routes":[{"destination":"10.5.0.0/24","target_type":"peering"}]}')
echo "$RESULT"
```

**Expected:** `applied: 0`, error mentions "requires target_id".

### 9. Enforce route with unknown target type

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d '{"routes":[{"destination":"10.6.0.0/24","target_type":"invalid"}]}')
echo "$RESULT"
```

**Expected:** `applied: 0`, error mentions "unknown target_type".

## Cleanup

```bash
nft delete table inet syfrah_routes 2>/dev/null
syfrah fabric stop; sleep 2
```
