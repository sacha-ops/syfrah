# 505 — Forge API Rename /v1/node → /v1/hypervisor

## Scope

Validates the Forge HTTP API path migration from /v1/node/* to /v1/hypervisor/*.

## Assertions

- `GET /v1/hypervisor/health` returns health status
- `GET /v1/hypervisor/status` returns status with vm_count
- `GET /v1/hypervisor/capacity` returns capacity breakdown
- `GET /v1/hypervisor/metrics` returns uptime and vm_count
- `GET /v1/hypervisor/resources` returns managed VM list
- Deprecated aliases still work:
  - `GET /v1/node/health` → same as /v1/hypervisor/health
  - `GET /v1/node/status` → same as /v1/hypervisor/status
  - `GET /v1/node/capacity` → same as /v1/hypervisor/capacity

## Result

PASS — Both /v1/hypervisor/* and /v1/node/* paths serve the same handlers.
