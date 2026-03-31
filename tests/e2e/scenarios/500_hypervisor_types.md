# 500 — Hypervisor Types and Store

## Scope

Validates the hypervisor type definitions and redb-backed CRUD operations
introduced by ADR-004.

## Preconditions

- `cargo test -p syfrah-org` passes (unit tests cover store operations)

## Assertions

### Types exist and serialize correctly

- `HypervisorId`, `Hypervisor`, `HardwareSpec`, `AllocatableCapacity`,
  `HypervisorState`, `HypervisorStatus`, `Taint`, `TaintEffect`,
  `DiskType`, `CpuArchitecture`, `GpuSpec` all compile and derive
  `Serialize` + `Deserialize`.
- `HypervisorState` has six variants: `Registering`, `NotReady`,
  `Available`, `Draining`, `Maintenance`, `Decommissioned`.
- `AllocatableCapacity::default()` uses sane defaults: `overcommit_cpu = 2.0`,
  `overcommit_memory = 1.0`, `reserved_vcpus = 1`, `reserved_memory_mb = 1024`.

### Store CRUD

- `HypervisorStore::create` persists a record and rejects duplicates.
- `HypervisorStore::get` retrieves by name; returns `None` for missing.
- `HypervisorStore::list` returns all records.
- `HypervisorStore::list_by_region` / `list_by_zone` filter correctly.
- `HypervisorStore::delete` removes the record; errors on missing.
- `HypervisorStore::get_by_fabric_node_id` finds by fabric identity.
- `HypervisorStore::get_by_id` finds by hypervisor ID.

### State transitions

- Valid: `Registering -> NotReady`, `NotReady -> Available`,
  `Available -> Draining`, `Available -> Maintenance`,
  `Available -> Decommissioned`, `Draining -> Available`,
  `Draining -> Maintenance`, `Maintenance -> Available`,
  `Maintenance -> Decommissioned`.
- Invalid transitions return `InvalidStateTransition` error.
- `Decommissioned` is terminal — no transitions out.

### Capacity updates

- `update_capacity` modifies only the capacity field.
- `update` replaces the full record (used for hardware re-probe).

## Result

PASS — all unit tests in `syfrah-org::hypervisor::tests` cover the above.
