# 605 — Burst Create 10 VMs

Verify that creating 10 VMs in rapid succession results in unique IPs, distributed placement, and clean cleanup.

## Prerequisites

- Cluster bootstrapped with Raft leader elected
- Multiple hypervisors registered across zones
- Default VPC, subnet `web`, security group `web-sg` configured
- Subnet has sufficient IP space for 10 allocations

## Setup

```bash
# Create 10 VMs in rapid sequence (no --zone constraint, let scheduler distribute)
for i in $(seq 1 10); do
  syfrah compute vm create --name burst-$i --image alpine-3.20 --vcpus 1 --memory 512 \
    --env prod --subnet web --project backend --org acme \
    --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg
done
```

## Assertions

1. **All 10 VMs created successfully**.
   ```bash
   syfrah compute vm list --project backend --org acme | grep burst-
   # Expected: 10 entries, all in "Running" state
   ```

2. **All 10 have unique IPs** — no IPAM collision.
   ```bash
   syfrah compute vm list --project backend --org acme --format json | jq '[.[] | select(.name | startswith("burst-")) | .ip] | unique | length'
   # Expected: 10 (all unique)
   ```

3. **Distributed across hypervisors** — not all on one node.
   ```bash
   syfrah compute vm list --project backend --org acme --format json | jq '[.[] | select(.name | startswith("burst-")) | .hypervisor] | unique | length'
   # Expected: >= 2 (at least 2 different hypervisors used)
   ```

4. **All VMs are Running**.
   ```bash
   syfrah compute vm list --project backend --org acme --format json | jq '[.[] | select(.name | startswith("burst-")) | .status] | unique'
   # Expected: ["Running"]
   ```

## Cleanup

```bash
# Delete all 10 VMs
for i in $(seq 1 10); do
  syfrah compute vm delete burst-$i --project backend --org acme --force
done
```

## Post-cleanup assertions

5. **All IPs released** — no leaked allocations.
   ```bash
   syfrah ipam list --subnet web --env prod --project backend --org acme
   # Expected: burst-* IPs no longer allocated
   ```

6. **No leaked interfaces** — tap devices and veth pairs cleaned up.
   ```bash
   # On each hypervisor:
   # ip link show | grep burst
   # Expected: no interfaces matching "burst-*"
   ```

## Expected results

| Check                          | Expected                    |
|--------------------------------|-----------------------------|
| VMs created                    | 10/10 success               |
| Unique IPs                     | 10 unique IPs               |
| Hypervisor distribution        | >= 2 hypervisors used       |
| All Running                    | Yes                         |
| IPs released after delete      | All freed                   |
| Interfaces cleaned after delete| No leaked tap/veth devices  |

## Pass criteria

- All 10 VMs must be created without IPAM collisions
- Scheduler must distribute across at least 2 hypervisors
- All VMs reach Running state
- Deletion must cleanly release all IPs and network interfaces
