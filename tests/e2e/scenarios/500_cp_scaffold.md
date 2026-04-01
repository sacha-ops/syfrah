# E2E: Control Plane Scaffold

**ID**: 500_cp_scaffold
**Layer**: controlplane
**Priority**: P0

## Objective
Verify that the control plane crate compiles, is registered in the workspace, and the core types are correctly defined.

## Prerequisites
- Workspace builds successfully with `cargo build --workspace`

## Steps

1. **Workspace registration**
   ```bash
   grep "layers/controlplane" Cargo.toml
   ```
   Expected: `layers/controlplane` appears in `[workspace.members]`

2. **Crate builds**
   ```bash
   cargo build -p syfrah-controlplane
   ```
   Expected: clean compilation, no errors

3. **Type configuration compiles**
   Verify `SyfrahRaftConfig` type is declared with correct associated types:
   - `D = StateMachineCommand`
   - `R = StateMachineResponse`
   - `Node = SyfrahNode`

4. **Node type serialization**
   ```bash
   cargo test -p syfrah-controlplane -- types::tests
   ```
   Expected: `node_display` and `node_serde_roundtrip` pass

5. **Command enum serialization**
   ```bash
   cargo test -p syfrah-controlplane -- commands::tests
   ```
   Expected: all command/response serde roundtrip tests pass

6. **Clippy clean**
   ```bash
   cargo clippy -p syfrah-controlplane -- -D warnings
   ```
   Expected: no warnings or errors

## Pass criteria
- All steps succeed
- No compilation warnings with `-D warnings`
- Unit tests pass
