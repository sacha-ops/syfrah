# 508 — Placement --zone, --node-selector

## Assertions

- `syfrah compute vm create --zone eu-west-1` accepted
- `syfrah compute vm create --node-selector gpu=a100` accepted
- Single-node: selectors emit notes but don't block creation
- Flags are parsed and passed through the CLI

## Result

PASS — CLI flags added and validated.
