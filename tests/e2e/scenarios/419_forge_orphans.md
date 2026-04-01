# Test: Forge 3-tier orphan handling

## Objective

Verify the 3-tier orphan classification and handling:
- Tier 1 (Known owned): in ownership registry -> manage normally
- Tier 2 (Suspected): matches Syfrah naming (syfb-*, syft-*, syfx-*) but not in registry -> quarantine (log, don't delete)
- Tier 3 (Unknown): no match (eth0, docker0, lo) -> ignore completely

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with a mesh initialized

## Steps

### 1. Initialize

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Create a known resource via Forge API

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/bridges \
  -H 'Content-Type: application/json' \
  -d '{"vpc_id": "vpc-orphan-test"}'
```

The bridge is registered in the ownership registry. It is a Tier 1 (Known) resource.

### 3. Create an orphan bridge manually (Tier 2 — Suspected)

```bash
# Create a bridge that matches Syfrah naming but is NOT registered.
ip link add syfb-orphan00 type bridge
ip link set syfb-orphan00 up
```

### 4. Verify classification

The ownership registry classifies:
- The Forge-created bridge: Known (Tier 1) -> managed
- syfb-orphan00: Suspected (Tier 2) -> quarantined (logged, NOT deleted)
- eth0, lo, docker0: Unknown (Tier 3) -> ignored

```bash
# The orphan bridge should NOT be deleted by the reconciler.
sleep 6
ip link show syfb-orphan00
```

**Expected:** syfb-orphan00 still exists — suspected orphans are quarantined, not deleted.

### 5. Verify Tier 3 interfaces are ignored

```bash
# These should never be touched by Forge.
ip link show eth0
ip link show lo
```

**Expected:** Both exist and are unmodified.

### 6. Clean up orphan manually

```bash
ip link delete syfb-orphan00 2>/dev/null
```

## Cleanup

```bash
syfrah fabric stop; sleep 2
```
