# 503 — Hypervisor Labels and Taints CLI

## Scope

Validates label and taint management on hypervisors.

## Assertions

- `syfrah hypervisor label <name> --set key=value` adds a label
- `syfrah hypervisor label <name> --remove key` removes a label
- `syfrah hypervisor taint <name> --add key=value:NoSchedule` adds a taint
- `syfrah hypervisor taint <name> --remove key` removes a taint
- Labels and taints are persisted in redb
- Invalid taint format returns an error

## Result

PASS — Label/taint operations implemented in HypervisorLayerHandler.
