# Test: Forge capacity tracking — full breakdown

## Objective

Verify that GET /v1/hypervisor/capacity returns a complete capacity breakdown including physical, reserved, overcommit ratios, allocatable, used, available resources, and disk usage.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized and hypervisor registered

## Steps

### 1. Initialize mesh and hypervisor

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
syfrah hypervisor register --region eu-west --zone az-1
syfrah hypervisor enable n1
FORGE_IP=$(ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Query capacity endpoint

```bash
curl -s http://[$FORGE_IP]:7100/v1/hypervisor/capacity | python3 -m json.tool
```

**Expected:** JSON response with all fields:
- `physical_vcpus` > 0 (from /proc or sysconf)
- `physical_memory_mb` > 0 (from /proc/meminfo)
- `reserved_vcpus` = 1 (host reserved)
- `reserved_memory_mb` = 1024 (host reserved 1 GB)
- `overcommit_cpu` = 2.0
- `overcommit_memory` = 1.0
- `allocatable_vcpus` = (physical - reserved) * overcommit_cpu
- `allocatable_memory_mb` = (physical - reserved) * overcommit_memory
- `used_vcpus` >= 0
- `used_memory_mb` >= 0
- `available_vcpus` = allocatable - used
- `available_memory_mb` = allocatable - used
- `disk_total_gb` > 0
- `disk_used_gb` >= 0
- `disk_available_gb` >= 0

### 3. Verify used counts update after VM creation

```bash
# Note the current used values
BEFORE=$(curl -s http://[$FORGE_IP]:7100/v1/hypervisor/capacity)

# Create a VM (requires org/project/env/subnet setup)
syfrah org create acme
syfrah project create backend --org acme
syfrah env create prod --project backend --org acme
syfrah subnet create web --env prod --project backend --org acme
syfrah nat-gw create main-gw --vpc acme-backend-default --subnet web
syfrah compute vm create --name cap-test --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub
sleep 10

# Check capacity again
AFTER=$(curl -s http://[$FORGE_IP]:7100/v1/hypervisor/capacity)
echo "Before: $BEFORE"
echo "After:  $AFTER"
```

**Expected:** `used_vcpus` increased by 1, `used_memory_mb` increased by 512 (or equivalent), `available_vcpus` decreased correspondingly.

### 4. Verify used counts decrease after VM deletion

```bash
syfrah compute vm delete cap-test --yes
sleep 5
DELETED=$(curl -s http://[$FORGE_IP]:7100/v1/hypervisor/capacity)
echo "After delete: $DELETED"
```

**Expected:** `used_vcpus` and `used_memory_mb` returned to pre-creation values.

### 5. Cleanup

```bash
syfrah fabric stop 2>/dev/null
```

## Pass criteria

- Capacity endpoint returns all fields (physical, reserved, overcommit, allocatable, used, available, disk)
- Physical values match the actual hardware
- Overcommit ratio: CPU 2:1, memory 1:1
- Reserved: 1 vCPU, 1024 MB
- Used counts update on VM create/delete
- Available = allocatable - used
