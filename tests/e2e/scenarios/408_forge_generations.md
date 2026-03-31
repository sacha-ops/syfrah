# Test: Forge generation tracking on resources

## Objective

- Every resource has spec_generation, reconcile_generation, last_observed_at
- spec_generation increments on desired-state changes
- reconcile_generation updates after successful reconciliation
- Drift detection: spec_generation != reconcile_generation
- Staleness detection: now - last_observed_at > threshold

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized

## Steps

### 1. Verify generation tracker is compiled and functional

The generation tracker is an in-memory module used by the reconciler.
Unit tests verify all edge cases. Integration is validated by:

```bash
# Build includes the generation module
cargo test -p syfrah-forge -- generation 2>&1
```

Expected: All generation tests pass.

### 2. Create a VM and verify generation tracking

When a VM is created through the Forge API, the generation tracker
registers the resource with spec_generation=1, reconcile_generation=0.

After reconciliation completes, reconcile_generation catches up to
spec_generation, clearing the drift flag.

## Expected Results

- spec_generation starts at 1, increments on each spec change
- reconcile_generation starts at 0, updated to match spec after reconcile
- Drift detected when spec != reconcile
- Staleness detected when last_observed_at is older than threshold
- Resource registration and removal work correctly
