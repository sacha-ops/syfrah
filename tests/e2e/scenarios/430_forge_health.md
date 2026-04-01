# Test: 4-category health model

## Objective

Verify GET /v1/hypervisor/health returns the 4-category health model with independent assessments.

## Steps

### 1. Query health endpoint

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print $2}' | cut -d/ -f1 | head -1)
curl -s http://[$FORGE_IP]:7100/v1/hypervisor/health | python3 -m json.tool
```

**Expected:** JSON response with 4 categories:
```json
{
  "status": "healthy",
  "agent_health": {"category": "self", "status": "healthy", ...},
  "node_health": {"category": "node", "status": "healthy", ...},
  "workload_health": {"category": "workload", "status": "healthy", ...},
  "control_health": {"category": "control", "status": "healthy", "message": "bootstrap mode (no Raft)"},
  "uptime_secs": ...,
  "vm_count": ...
}
```

### 2. Verify overall status is worst of four

Each category is independent. Overall = worst of the four. In bootstrap mode, all four should be healthy.

## Pass criteria

- GET /v1/hypervisor/health returns 4 categories
- Each category has: category name, status, optional message
- Overall status = worst of the four categories
- control_health reports "bootstrap mode" when no control plane
- uptime_secs and vm_count are accurate
