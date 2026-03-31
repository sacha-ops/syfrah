# Test: Forge internal module structure

## Objective

- All 6 Forge modules compile and have defined trait boundaries
- No circular dependencies between modules
- Each module serves its documented purpose

## Prerequisites

- Rust toolchain installed
- Repository checked out

## Steps

### 1. Build the forge crate

```bash
cargo build -p syfrah-forge
```

### 2. Run module boundary tests

```bash
cargo test -p syfrah-forge -- module_boundaries_compile no_circular_deps
```

### 3. Verify module count

```bash
ls layers/forge/src/*.rs | wc -l
# Should be 8 (lib.rs + 6 modules + ownership)
```

## Expected Results

- `cargo build -p syfrah-forge` succeeds with no errors
- `module_boundaries_compile` test passes — all modules accessible
- `no_circular_deps` test passes — standalone modules don't depend on api
- Module trait boundaries:
  - `runtime::ComputeBackend` — abstraction over VmManager
  - `health::HealthChecker` — pluggable health checks
  - `reconciler::ReconcileTarget` / `DriftDetector` — reconciliation traits
  - `capacity::CapacityTracker` — admission control
  - `task::TaskStore` — operation tracking via LayerDb
  - `ownership::OwnershipRegistry` — resource tracking via LayerDb
