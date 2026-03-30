# Test: Overlay scaffold — NetworkBackend trait and MockBackend

## Objective

Validate that the `syfrah-overlay` crate compiles, the `NetworkBackend` trait is
usable as a trait object, and the `MockBackend` correctly records and resets calls.

## Prerequisites

- Rust toolchain installed
- Repository cloned and workspace builds (`cargo build`)

## Steps

### 1. Build the overlay crate

```bash
cargo build -p syfrah-overlay
```

**Expected**: compiles without errors.

### 2. Run unit tests

```bash
cargo test -p syfrah-overlay
```

**Expected**: all tests pass, including:
- `mock_backend_records_calls`
- `trait_method_coverage`
- `handler_returns_not_implemented`

### 3. Verify trait object safety

```bash
cargo test -p syfrah-overlay -- trait_method_coverage
```

**Expected**: the mock implements `NetworkBackend` as `Send + Sync`, all 19
trait methods are exercised, and call recording matches expectations.

### 4. Verify reset clears state

Covered by `trait_method_coverage` test — after calling `reset()`, `calls()`
returns an empty vec.

## Pass criteria

- Crate compiles with no warnings under `cargo clippy`
- All three unit tests pass
- `MockBackend` records exactly one string per trait method call
- `reset()` clears the call log
