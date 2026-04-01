# Test: Control-degraded policy

## Objective

Verify the degraded policy framework is implemented and in bootstrap mode (always connected) allows all operations. The framework is ready for future control plane integration.

## Steps

### 1. Verify bootstrap mode

In bootstrap mode (no control plane), all operations should be allowed.

The DegradedController::new_bootstrap() returns Bootstrap state, which permits:
- Reads: allowed
- Reconcile: allowed
- Creates: allowed
- Deletes: allowed
- Start/stop: allowed

### 2. Unit test verification

```bash
cargo test -p syfrah-forge degraded 2>&1
```

**Expected:** All degraded tests pass.

## Pass criteria

- DegradedController module exists with operation checking framework
- Bootstrap mode allows all operations (no-op for now)
- Degraded mode denies creates and deletes
- Status snapshot returns full policy breakdown
- Framework ready for control plane integration
