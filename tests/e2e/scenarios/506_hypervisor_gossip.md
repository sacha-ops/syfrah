# 506 — Hypervisor Gossip Report

## Scope

Validates the HypervisorReport type for gossip-based health reporting.

## Assertions

- `HypervisorReport` has fields: hypervisor_id, fabric_node_id, state, capacity,
  vm_count, host_cpu_percent, host_memory_percent, host_disk_percent, labels, taints, timestamp
- `HypervisorReport::from_hypervisor()` builds a report from a Hypervisor record
- Report serializes/deserializes correctly for gossip transport

## Result

PASS — Type defined, constructor implemented.
