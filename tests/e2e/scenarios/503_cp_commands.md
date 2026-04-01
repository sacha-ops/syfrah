# E2E: Control Plane Command Enum

**ID**: 503_cp_commands
**Layer**: controlplane
**Priority**: P0

## Objective
Verify the StateMachineCommand enum covers all Raft-replicated operations and that all variants serialize/deserialize correctly.

## Steps

1. **All 35 command variants serialize**
   ```bash
   cargo test -p syfrah-controlplane -- commands::tests::all_command_variants_serialize
   ```

2. **All response variants serialize**
   ```bash
   cargo test -p syfrah-controlplane -- commands::tests::all_response_variants_serialize
   ```

3. **Default response**
   ```bash
   cargo test -p syfrah-controlplane -- commands::tests::default_response_is_ok
   ```

## Pass criteria
- Every command variant roundtrips through JSON
- Every response variant roundtrips through JSON
- Default response is Ok
