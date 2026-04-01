# Test: Forge reservation system with expiry

## Objective

Verify the reservation system prevents double-booking under concurrent creates:
- Resources are reserved atomically during admission
- Reservations expire after 60 seconds if creation doesn't complete
- Successful creation converts reservation to allocation
- Failed creation releases reservation

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized and hypervisor registered

## Steps

### 1. Initialize

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
syfrah hypervisor register --region eu-west --zone az-1
syfrah hypervisor enable n1
FORGE_IP=$(ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Check initial reservations (should be empty)

```bash
curl -s http://[$FORGE_IP]:7100/v1/hypervisor/reservations | python3 -m json.tool
```

**Expected:** `{"reservations": [], "count": 0}`

### 3. Create a VM and observe reservation flow

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create prod --project backend --org acme
syfrah subnet create web --env prod --project backend --org acme
syfrah nat-gw create main-gw --vpc acme-backend-default --subnet web

# Create VM — reservation is made during creation
syfrah compute vm create --name res-test --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub
sleep 15

# After creation completes, reservation should be converted to allocation
curl -s http://[$FORGE_IP]:7100/v1/hypervisor/reservations | python3 -m json.tool
```

**Expected:** Reservations list is empty (reservation was committed to allocation).

### 4. Verify capacity reflects allocation

```bash
curl -s http://[$FORGE_IP]:7100/v1/hypervisor/capacity | python3 -m json.tool
```

**Expected:** `used_vcpus >= 1`, `used_memory_mb >= 512`.

### 5. Double-booking prevention

Try creating VMs that exceed capacity. The second should be rejected.

```bash
AVAIL=$(curl -s http://[$FORGE_IP]:7100/v1/hypervisor/capacity | python3 -c 'import sys,json; print(json.load(sys.stdin).get("available_vcpus",0))')
echo "Available vCPUs: $AVAIL"

# Request more than available
OVER=$((AVAIL + 10))
curl -s -X POST http://[$FORGE_IP]:7100/v1/instances \
  -H 'Content-Type: application/json' \
  -d "{\"name\": \"overbook\", \"image\": \"alpine-3.20\", \"vcpus\": $OVER, \"memory_mb\": 512}" 2>&1
```

**Expected:** 409 Conflict with `FORGE_INSUFFICIENT_CAPACITY`.

### 6. Cleanup

```bash
syfrah compute vm delete res-test --yes
syfrah fabric stop 2>/dev/null
```

## Pass criteria

- Reservations are created atomically during admission
- Reservations expire after 60s
- Successful creation converts reservation to allocation
- Over-capacity requests are rejected with 409
- GET /v1/hypervisor/reservations shows active reservations
