# 512 — VM List/Get Shows Hypervisor Info

## Scope

Validates that vm list and vm get display hypervisor information.

## Assertions

- `syfrah compute vm list` includes HYPERVISOR column in table output
- `syfrah compute vm get <name>` shows Hypervisor, Region, Zone fields
- JSON output includes hypervisor_id, region, zone keys
- Fields are null/"-" when hypervisor info not yet populated

## Result

PASS — CLI display updated with hypervisor columns, JSON includes new fields.
