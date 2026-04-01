# Test: Node drain/undrain protocol

## Objective

Verify the drain/undrain protocol:
- POST /v1/hypervisor/drain: mark draining, stop admitting new VMs
- POST /v1/hypervisor/activate: return to Available
- Drain with force: immediate
- Existing VMs survive drain (continue running)

## Steps

### 1. Initialize

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 2. Drain the node via HTTP API

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/hypervisor/drain \
  -H 'Content-Type: application/json' \
  -d '{"force": false}' | python3 -m json.tool
```

**Expected:** `{"draining": true, "force": false, ...}`

### 3. Verify new creates are rejected while draining

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"name": "should-fail", "image": "alpine-3.20"}' 2>&1
```

**Expected:** 409 Conflict with `FORGE_NODE_DRAINING`.

### 4. Activate (undrain)

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/hypervisor/activate | python3 -m json.tool
```

**Expected:** `{"status": "available", "draining": false}`

### 5. Verify creates work after activate

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"name": "post-activate", "image": "alpine-3.20"}' 2>&1
```

**Expected:** Success (or appropriate error unrelated to draining).

## Pass criteria

- Drain stops new VM creation with FORGE_NODE_DRAINING
- Activate resumes normal operation
- Drain status visible via GET /v1/hypervisor/drain
- CLI `syfrah hypervisor drain/activate` works through the daemon handler
