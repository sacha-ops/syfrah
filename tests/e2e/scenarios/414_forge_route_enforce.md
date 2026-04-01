# Test: Forge route table enforcement

## Objective

- POST /v1/networks/routes/enforce applies blackhole routes as nftables DROP
- Validates route targets (NAT GW must be active, peering must have target_id)
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

### 4. Enforce route with invalid NAT GW target

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/routes/enforce \
  -H 'Content-Type: application/json' \
  -d '{"routes":[{"destination":"10.2.0.0/24","target_type":"nat-gw","target_id":"nat-nonexistent"}]}')
echo "$RESULT"
```

**Expected:** `applied: 0`, `errors` array with "NAT GW not found".

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
