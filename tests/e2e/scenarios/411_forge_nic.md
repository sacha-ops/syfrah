# Test: Forge NIC management endpoints

## Objective

- POST /v1/networks/interfaces creates a TAP device, attaches to bridge, registers NIC
- DELETE /v1/networks/interfaces/:id removes TAP and firewall rules
- GET /v1/networks/interfaces?vm_id=X lists NICs filtered by VM
- NIC creation applies anti-spoofing firewall rules automatically

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Initialize the mesh

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/fabric.redb ~/.syfrah/state.json ~/.syfrah/*.redb
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
```

### 2. Get the fabric IPv6 address

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 3. Create a bridge first (NIC needs a bridge to attach to)

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-nic-test"}'
```

### 4. Create a NIC

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/interfaces \
  -H 'Content-Type: application/json' \
  -d '{"vm_id":"vm-1","vpc_id":"vpc-nic-test","ip":"10.1.0.3","mac":"02:00:0a:01:00:03"}')
echo "$RESULT"
```

**Expected:** HTTP 201. Response contains `tap_name` starting with `syft-`, `vm_id`, `ip`, `mac`, and a `generation` object.

### 5. Verify TAP exists in kernel

```bash
ip link show | grep syft-
```

**Expected:** TAP interface from step 4 is listed.

### 6. Create a second NIC for a different VM

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/interfaces \
  -H 'Content-Type: application/json' \
  -d '{"vm_id":"vm-2","vpc_id":"vpc-nic-test","ip":"10.1.0.4","mac":"02:00:0a:01:00:04"}'
```

### 7. List all NICs

```bash
curl -s http://[$FORGE_IP]:7100/v1/networks/interfaces
```

**Expected:** JSON array with 2 NICs.

### 8. List NICs filtered by VM

```bash
curl -s "http://[$FORGE_IP]:7100/v1/networks/interfaces?vm_id=vm-1"
```

**Expected:** JSON array with 1 NIC (vm-1 only).

### 9. Delete a NIC

```bash
NIC_ID=$(echo "$RESULT" | jq -r '.id')
curl -s -X DELETE http://[$FORGE_IP]:7100/v1/networks/interfaces/$NIC_ID
```

**Expected:** HTTP 200 with `FORGE_NIC_DELETED` code.

### 10. Verify TAP removed from kernel

```bash
TAP_NAME=$(echo "$RESULT" | jq -r '.tap_name')
ip link show $TAP_NAME 2>&1
```

**Expected:** "does not exist" error — TAP was cleaned up.

## Cleanup

```bash
syfrah fabric stop
sleep 2
```
