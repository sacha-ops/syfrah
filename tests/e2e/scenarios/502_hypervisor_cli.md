# 502 — Hypervisor CLI

## Scope

Validates the `syfrah hypervisor` CLI subcommands for listing, inspecting,
registering, and enabling hypervisors.

## Preconditions

- Fabric mesh initialized with `syfrah fabric init`
- KVM available on the node

## Assertions

### Commands exist

- `syfrah hypervisor list [--region] [--zone] [--json]`
- `syfrah hypervisor get <name> [--json]`
- `syfrah hypervisor register --region X --zone Y`
- `syfrah hypervisor enable <name>`
- `syfrah hypervisor status`
- `syfrah hypervisor capacity`

### Routing

- All commands route through the daemon's control socket via "hypervisor" layer
- Returns "cannot reach daemon" if daemon is not running

### Output

- `list` shows table with NAME, REGION, ZONE, STATE columns
- `get` shows detailed info including hardware, capacity, labels, taints
- `status` shows local hypervisor summary
- `capacity` shows detailed capacity breakdown

## Result

PASS — CLI commands route through daemon control socket to HypervisorLayerHandler.
