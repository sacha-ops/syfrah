# 300 — Partial create rollback

## Purpose

Verify that when `vm create` fails mid-way through network setup, all
partially-created resources are rolled back cleanly: no leaked IPs, no
orphaned TAPs, no dangling bridges.

## Prerequisites

- Org, project, environment, VPC, and subnet already exist
- `syfrah` daemon running

## Scenario 1 — TAP failure rolls back IP allocation

1. Inject a TAP creation failure (e.g. mock `create_tap` to error)
2. Run `syfrah compute vm create --name fail-vm --image alpine-3.20 --subnet frontend --project backend --org acme`
3. Expect: command returns an error indicating TAP creation failed
4. Verify: IPAM bitmap shows no allocation for `fail-vm`
5. Verify: no `syftap-fail-vm` interface exists
6. Verify: `vm list` does not include `fail-vm`

## Scenario 2 — Bridge cleanup when VM was first in VPC

1. Ensure no VMs exist in VPC (bridge not yet created)
2. Inject a failure at nftables rule application (after bridge + TAP created)
3. Run `syfrah compute vm create --name fail-vm2 --image alpine-3.20 --subnet frontend --project backend --org acme`
4. Expect: command returns an error
5. Verify: no `syfbr-{vpc_id}` bridge exists (it was created and rolled back)
6. Verify: no `syftap-fail-vm2` TAP exists
7. Verify: IPAM shows no allocation

## Scenario 3 — Existing bridge preserved when rollback for second VM

1. Create a successful VM (`vm-ok`) so the bridge exists
2. Inject a TAP failure for the second VM
3. Run `syfrah compute vm create --name fail-vm3 ...`
4. Expect: error, rollback runs
5. Verify: bridge still exists (it was not created by this VM)
6. Verify: `vm-ok` is unaffected and still running
7. Verify: IPAM allocation for `fail-vm3` is released

## Scenario 4 — Best-effort rollback continues on cleanup errors

1. Inject failures at both `create_tap` (to trigger rollback) and `delete_bridge`
   (to make rollback partially fail)
2. Run `syfrah compute vm create --name fail-vm4 ...`
3. Expect: error returned to user
4. Verify: IP is still released even though bridge deletion failed
5. Verify: logs show warnings for the failed rollback step

## Pass criteria

- No resources leaked after any failed `vm create`
- IPAM allocations released on every failure path
- Bridges only deleted if created by the failing VM
- Rollback errors logged but do not cause panics
