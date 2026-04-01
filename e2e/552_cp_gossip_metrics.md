# E2E: Gossip Metrics (#1071)

## Scope

Verify that gossip-specific metrics are exposed on the `/metrics` endpoint
(port 7100) alongside the existing forge and Raft metrics.

## Prerequisites

- Two-node cluster with `controlplane init` + `controlplane join`

## Test Steps

### 1. Query gossip metrics

```bash
FABRIC_IP=$(ssh root@65.109.130.108 "ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print \$2}' | cut -d/ -f1 | head -1")
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep gossip"
```

### 2. Verify expected gossip metrics

```bash
# Expected output (values may vary):
# gossip_members_total{state="alive"} 2
# gossip_members_total{state="suspect"} 0
# gossip_members_total{state="down"} 0
# gossip_messages_sent_total <N>
# gossip_messages_received_total <N>
```

### 3. Verify message counters increase over time

```bash
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep gossip_messages"
sleep 5
ssh root@65.109.130.108 "curl -s http://[$FABRIC_IP]:7100/metrics | grep gossip_messages"
# Counters should have increased (gossip probes run every second)
```

### 4. Verify metrics on second node

```bash
FABRIC_IP2=$(ssh root@37.27.12.205 "ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print \$2}' | cut -d/ -f1 | head -1")
ssh root@37.27.12.205 "curl -s http://[$FABRIC_IP2]:7100/metrics | grep gossip"
```

## Expected Metrics

| Metric                        | Type    | Description                        |
|-------------------------------|---------|------------------------------------|
| gossip_members_total{state=X} | gauge   | Members by state (alive/suspect/down) |
| gossip_messages_sent_total    | counter | Total gossip messages sent         |
| gossip_messages_received_total| counter | Total gossip messages received     |

## Pass Criteria

- All gossip metrics present in `/metrics` output
- Member counts match the actual cluster state
- Message counters increase over time
- Metrics available on both nodes
