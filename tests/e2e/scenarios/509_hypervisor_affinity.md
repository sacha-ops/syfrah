# 509 — Placement --anti-affinity, --spread-topology

## Assertions

- `syfrah compute vm create --anti-affinity web-group` accepted with warning
- `syfrah compute vm create --spread-topology zone` accepted with warning
- Single-node: warnings emitted (cannot spread on 1 node)
- Flags parsed and available for multi-node extension

## Result

PASS — CLI flags added with single-node warnings.
