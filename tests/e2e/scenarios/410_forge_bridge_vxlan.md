# Test: Forge bridge and VXLAN management endpoints

## Objective

- POST /v1/networks/bridges creates a Linux bridge for a VPC (idempotent)
- DELETE /v1/networks/bridges/:id removes a bridge
- POST /v1/networks/vxlans creates a VXLAN interface for a VPC (idempotent)
- DELETE /v1/networks/vxlans/:id removes a VXLAN
- All responses include resource state with generation tracking
- Operations are idempotent: repeated creates succeed without error

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
echo "Forge IP: $FORGE_IP"
```

### 3. Create a bridge for a VPC

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-test-1"}')
echo "$RESULT"
```

**Expected:** HTTP 200. Response contains `bridge_name` starting with `syfb-`, `vpc_id` = `vpc-test-1`, and a `generation` object with `spec_generation` = 1.

### 4. Verify bridge exists in kernel

```bash
ip link show | grep syfb-
```

**Expected:** The bridge interface from step 3 is listed and UP.

### 5. Idempotent bridge create

```bash
RESULT2=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-test-1"}')
echo "$RESULT2"
```

**Expected:** HTTP 200. Same bridge name as step 3. No error.

### 6. Create a VXLAN for the same VPC

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/vxlans \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-test-1", "vni": 100, "local_ip": "10.0.0.1"}')
echo "$RESULT"
```

**Expected:** HTTP 200. Response contains `vxlan_name` starting with `syfx-`, `vni` = 100, and a `generation` object.

### 7. Verify VXLAN exists in kernel

```bash
ip link show | grep syfx-
```

**Expected:** The VXLAN interface is listed and UP.

### 8. Delete the VXLAN

```bash
VXLAN_NAME=$(echo "$RESULT" | jq -r '.vxlan_name')
curl -s -X DELETE http://[$FORGE_IP]:7100/v1/networks/vxlans/$VXLAN_NAME
```

**Expected:** HTTP 200 with `FORGE_VXLAN_DELETED` code.

### 9. Delete the bridge

```bash
BRIDGE_NAME=$(echo "$RESULT2" | jq -r '.bridge_name')
curl -s -X DELETE http://[$FORGE_IP]:7100/v1/networks/bridges/$BRIDGE_NAME
```

**Expected:** HTTP 200 with `FORGE_BRIDGE_DELETED` code.

### 10. Verify interfaces removed from kernel

```bash
ip link show | grep -c 'syfb-\|syfx-'
```

**Expected:** 0 (no syfrah bridge or VXLAN interfaces remain).

### 11. Operations without network backend return 503

This is verified by unit tests: when `network_backend` is None, all bridge/VXLAN endpoints return HTTP 503 with `FORGE_NETWORK_UNAVAILABLE`.

## Cleanup

```bash
syfrah fabric stop
sleep 2
```
