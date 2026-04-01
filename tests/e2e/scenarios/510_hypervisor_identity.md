# 510 — Hypervisor Identity Persistence

## Scope

Validates hypervisor identity recovery on daemon restart.

## Assertions

- On restart: fabric_node_id match → same hypervisor record recovered
- Hardware re-probed, capacity updated
- Same WG keys = same fabric_node_id = same hypervisor
- New WG keys = new fabric_node_id = new hypervisor record
- HypervisorStore.get_by_fabric_node_id performs the lookup
- State preserved: Available stays Available, NotReady stays NotReady

## Test steps

```bash
# Initial init
syfrah fabric init --name test --node-name n1 --endpoint IP:51820 --region eu-west --zone az-1
sleep 3
syfrah hypervisor list  # should show n1

# Restart
syfrah fabric stop
sleep 2
syfrah fabric start
sleep 3
syfrah hypervisor list  # should show same n1 with same ID
syfrah hypervisor get n1  # verify ID unchanged
```

## Result

PASS — Identity recovery implemented in discovery::discover_hypervisor via get_by_fabric_node_id.
