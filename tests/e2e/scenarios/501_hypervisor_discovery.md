# 501 — Hypervisor Auto-Discovery

## Scope

Validates that Forge/daemon startup probes local hardware and auto-registers
a hypervisor when KVM is available.

## Preconditions

- Node has `/dev/kvm` (KVM-capable server)
- Fabric mesh initialized with `syfrah fabric init`

## Assertions

### KVM detection

- If `/dev/kvm` exists: hypervisor record is created in `hypervisor.redb`
- If `/dev/kvm` does not exist: no hypervisor record, node is mesh-only

### Hardware probing

- CPU model parsed from `/proc/cpuinfo`
- Physical cores and logical threads detected
- Total memory parsed from `/proc/meminfo`
- Disk type and size detected via `lsblk`
- GPU detected via `lspci` (optional, None if no GPU)
- Architecture detected from runtime

### State transitions

- New discovery: Registering → NotReady (automatic)
- Existing record on restart: recovered by `fabric_node_id` match
- Hardware re-probed on restart, capacity updated

### Capacity computation

- `allocatable_vcpus = threads * 2.0 - 1` (2x overcommit, 1 reserved)
- `allocatable_memory_mb = memory_mb * 1.0 - 1024` (no overcommit, 1GB reserved)
- `local_allocatable_gb = total_gb - 20` (20GB OS reserve)

## Test steps

```bash
syfrah fabric init --name test --node-name n1 --endpoint IP:51820 --region eu-west --zone az-1
sleep 3
# Check hypervisor was discovered:
syfrah state list hypervisor  # Should show 1 entry
```

## Result

PASS — discovery integrates with daemon startup, probes hardware, creates record.
