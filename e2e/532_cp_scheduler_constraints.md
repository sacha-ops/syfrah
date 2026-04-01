# E2E 532 — Scheduler Constraints (zone, labels, taints)

## Objective
Verify that `--zone`, `--node-selector`, and taint/toleration filtering
are wired from the CLI through to the scheduler.

## Test Steps

### 1. CLI passes zone constraint
```bash
syfrah compute vm create --name test --image alpine-3.20 --zone az-2 ...
```
The `--zone` flag is included in the ComputeRequest and available for the
daemon's scheduler integration.

### 2. CLI passes node-selector labels
```bash
syfrah compute vm create --name test --image alpine-3.20 --node-selector gpu=a100 ...
```
The `--node-selector` key=value pairs are parsed into a HashMap.

### 3. PlacementConstraints::from_cli parses correctly
- `from_cli(Some("az-2"), &["gpu=a100"], None, None)` yields
  zone=Some("az-2"), node_selector={"gpu": "a100"}

### 4. Zone filter error message is clear
- When no hypervisor matches `--zone az-99`:
  "no hypervisor matches constraints: zone=az-99"

### 5. Label filter error message includes selectors
- When no hypervisor matches `--node-selector gpu=a100`:
  "no hypervisor matches constraints: gpu=a100"

### 6. Taint filtering works
- Hypervisor with `gpu=true:NoSchedule` taint is skipped
- Unless VM has matching toleration

## Pass Criteria
- CLI flags are parsed and included in ComputeRequest
- PlacementConstraints builder works correctly
- Error messages are clear and actionable
- Full workspace builds cleanly
