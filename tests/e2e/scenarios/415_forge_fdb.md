# Test: Forge FDB management endpoints

## Objective

- GET /v1/networks/fdb lists all FDB entries
- POST /v1/networks/fdb with action "add" creates FDB + ARP proxy entries
- POST /v1/networks/fdb with action "remove" removes FDB + ARP proxy entries
- Invalid action returns 400

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized

## Steps

### 1. Initialize and get Forge IP

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Add FDB entry

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/fdb \
  -H 'Content-Type: application/json' \
  -d '{"action":"add","vpc_id":"vpc-fdb-test","mac":"02:00:0a:01:00:03","vtep":"10.0.0.2","vm_ip":"10.1.0.3"}')
echo "$RESULT"
```

**Expected:** HTTP 200 with `FORGE_FDB_ADDED`.

### 3. List FDB entries

```bash
curl -s http://[$FORGE_IP]:7100/v1/networks/fdb
```

**Expected:** JSON array with 1 entry.

### 4. Remove FDB entry

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/fdb \
  -H 'Content-Type: application/json' \
  -d '{"action":"remove","vpc_id":"vpc-fdb-test","mac":"02:00:0a:01:00:03","vtep":"10.0.0.2","vm_ip":"10.1.0.3"}'
```

**Expected:** HTTP 200 with `FORGE_FDB_REMOVED`.

### 5. List should be empty

```bash
curl -s http://[$FORGE_IP]:7100/v1/networks/fdb
```

**Expected:** Empty JSON array.

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
