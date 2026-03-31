# 504 — Hypervisor Lifecycle CLI

## Scope

Validates drain, activate, maintenance, and decommission CLI commands.

## Assertions

- `syfrah hypervisor drain <name>` transitions Available → Draining
- `syfrah hypervisor activate <name>` transitions Draining/Maintenance → Available
- `syfrah hypervisor maintenance <name>` transitions Available → Maintenance
- `syfrah hypervisor decommission <name>` transitions to Decommissioned (terminal)
- Invalid state transitions return errors
- State transitions are enforced per ADR-004

## Result

PASS — Lifecycle commands delegate to HypervisorStore.update_state with validation.
