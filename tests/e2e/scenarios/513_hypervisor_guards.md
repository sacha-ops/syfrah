# 513 — Hypervisor Deletion Guards

## Scope

Validates that destructive hypervisor operations are guarded against unsafe state.

## Assertions

### Decommission guard
- Cannot decommission with running VMs (checked via placement store)
- Returns error: "cannot decommission: N VM(s) still running"
- After all VMs deleted: decommission succeeds

### Maintenance guard
- Cannot enter maintenance with running VMs
- Returns error: "cannot enter maintenance: N VM(s) still running. Drain first."
- After drain completes (no VMs): maintenance succeeds

### Terminal state
- Decommissioned is terminal — no transitions out
- Attempting to activate a decommissioned hypervisor returns InvalidStateTransition

## Result

PASS — Guards implemented in HypervisorLayerHandler with placement store VM count checks.
