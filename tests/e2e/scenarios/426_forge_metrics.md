# Test: Prometheus metrics endpoint

## Objective

Verify GET /metrics returns Prometheus text exposition format with all required metrics.

## Steps

### 1. Query metrics endpoint

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print $2}' | cut -d/ -f1 | head -1)
curl -s http://[$FORGE_IP]:7100/metrics | head -30
```

**Expected:** Prometheus text format with:
- `forge_instances_total{state="running"}`
- `forge_instances_total{state="stopped"}`
- `forge_reconciliation_duration_seconds`
- `forge_node_cpu_used_ratio`
- `forge_node_memory_used_ratio`
- Content-Type: `text/plain; version=0.0.4; charset=utf-8`

## Pass criteria

- /metrics returns valid Prometheus exposition format
- All 5 metric families present
- Values are numeric and parseable by Prometheus
