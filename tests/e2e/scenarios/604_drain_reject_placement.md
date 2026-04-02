# 604 — Drain Reject Placement

Verify that a drained hypervisor is excluded from VM placement, and that activating it re-enables placement.

## Prerequisites

- Cluster bootstrapped with Raft leader elected
- Multiple hypervisors registered, including at least 2 in zone `nbg1`
- Default VPC, subnet, and security group configured

## Steps

### Step 1 — Verify initial state

```bash
syfrah hypervisor list
# All hypervisors should be in "active" state
# Note which hypervisors are in zone nbg1
```

### Step 2 — Drain one nbg1 hypervisor

```bash
syfrah hypervisor drain hv-nbg1-01
syfrah hypervisor list
# hv-nbg1-01 should now show status "draining" or "drained"
```

### Step 3 — Create a VM targeting nbg1

```bash
syfrah compute vm create --name drain-test --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone nbg1
```

### Step 4 — Verify placement avoided drained node

```bash
syfrah compute vm get drain-test --project backend --org acme
# Expected: VM placed on hv-nbg1-02 (or another active nbg1 hypervisor), NOT hv-nbg1-01
```

### Step 5 — Edge case: drain all nbg1 hypervisors

```bash
syfrah hypervisor drain hv-nbg1-02
syfrah compute vm create --name drain-test-2 --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone nbg1
# Expected: FAIL with error "no hypervisor available in zone nbg1" or similar
```

### Step 6 — Re-activate and verify placement works again

```bash
syfrah hypervisor activate hv-nbg1-01
syfrah hypervisor activate hv-nbg1-02

syfrah compute vm create --name drain-test-3 --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone nbg1
# Expected: SUCCESS — VM placed on one of the now-active nbg1 hypervisors
```

## Assertions

| Check                                      | Expected                              |
|--------------------------------------------|---------------------------------------|
| Drain hv-nbg1-01                           | Status changes to drained             |
| VM with --zone nbg1 (one drained)         | Placed on hv-nbg1-02                 |
| VM with --zone nbg1 (all drained)         | Error: no hypervisor available        |
| Activate both, then create VM              | SUCCESS, placed on active hypervisor  |

## Pass criteria

- Drained hypervisors MUST NOT receive new VMs
- When all hypervisors in a zone are drained, VM creation MUST fail with a clear error
- Re-activating a hypervisor MUST allow it to receive VMs again
- Existing VMs on a drained hypervisor MUST NOT be affected (drain only blocks new placement)
