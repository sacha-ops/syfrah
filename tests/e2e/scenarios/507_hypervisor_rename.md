# 507 — Rename hosting_node → hypervisor_id

## Scope

Global rename of `hosting_node` to `hypervisor_id` in VmPlacement and all references.

## Files changed

- `layers/org/src/types.rs` — VmPlacement.hosting_node → hypervisor_id
- `layers/org/src/placement.rs` — list_by_node parameter
- `layers/org/src/tests.rs` — test data
- `layers/compute/src/network.rs` — NetworkResult
- `layers/compute/src/network_setup.rs` — placement creation
- `layers/compute/src/manager.rs` — reconnect
- `layers/overlay/src/fdb.rs` — FDB entry creation
- `layers/overlay/src/recovery.rs` — recovery placement
- `layers/fabric/src/vm_placement.rs` — announcement handling

## Assertions

- All compilation units build
- All existing tests pass (field name is serialized, so JSON compat is maintained via serde)
- Fabric "node" concept is unchanged — only compute placement uses "hypervisor_id"

## Result

PASS — mechanical rename, all tests pass.
