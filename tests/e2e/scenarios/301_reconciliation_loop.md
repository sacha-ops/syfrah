# 301 — Reconciliation Loop

## Purpose

Verify the periodic reconciliation loop detects and fixes network state drift
between redb (expected state) and the Linux kernel (actual state).

## Preconditions

- `syfrah` daemon running on a single node
- At least one VPC with a bridge and one VM with a TAP interface
- nftables rules applied for the VM

## Scenario 1: Missing bridge re-created

1. Create a VM in a VPC (bridge `syfbr-{vpc_id}` exists).
2. Delete the bridge manually: `ip link del syfbr-{vpc_id}`.
3. Wait for the next reconciliation tick (30s max).
4. Verify the bridge is re-created: `ip link show syfbr-{vpc_id}`.

**Expected**: bridge re-created, `ReconcileReport.bridges_fixed == 1`.

## Scenario 2: Orphaned TAP detected

1. Create a TAP manually: `ip tuntap add mode tap name syftap-orphan`.
2. Ensure `syftap-orphan` is **not** tracked in redb.
3. Wait for reconciliation tick.
4. Check daemon logs for orphaned TAP warning.

**Expected**: warning logged: `"orphaned TAP in kernel: syftap-orphan"`.

## Scenario 3: nftables rules re-applied after reboot

1. Create a VM (anti-spoofing + ingress/egress rules applied).
2. Flush all nftables rules: `nft flush ruleset`.
3. Wait for reconciliation tick.
4. Verify rules are re-applied: `nft list ruleset | grep syftap-{vm_id}`.

**Expected**: `ReconcileReport.rules_reapplied >= 1`, rules present in nftables.

## Scenario 4: Orphaned IP reclaimed

1. Allocate an IP via IPAM (state = `Reserved`, no VM created).
2. Wait > 5 minutes (or set `allocated_at` to 6 min ago in test setup).
3. Trigger reconciliation.
4. Verify the IP allocation is reclaimed (bitmap bit cleared, allocation removed).

**Expected**: `ReconcileReport.orphans_reclaimed == 1`.

## Scenario 5: Missing TAP warns but does not re-create

1. Create a VM (TAP `syftap-{vm_id}` exists).
2. Delete the TAP manually: `ip link del syftap-{vm_id}`.
3. Wait for reconciliation tick.
4. Check daemon logs for missing TAP warning.
5. Verify the TAP is **not** re-created (VM may be gone).

**Expected**: warning logged mentioning `syftap-{vm_id}`, TAP not re-created.

## Scenario 6: Clean state produces no actions

1. Create a VM with full networking (bridge, TAP, rules, IP assigned).
2. Do not disturb any state.
3. Wait for reconciliation tick.
4. Verify report: `bridges_fixed == 0`, `rules_reapplied == N` (re-apply is
   always done), `orphans_reclaimed == 0`, `warnings` empty.

**Expected**: no corrective actions taken (except rule re-application).

## Scenario 7: Full drift recovery

1. Create two VMs in different VPCs.
2. Simultaneously: delete one bridge, flush nftables, create an orphaned TAP.
3. Set an old `Reserved` IP allocation in IPAM.
4. Wait for reconciliation tick.
5. Verify:
   - Missing bridge re-created
   - Rules re-applied for both VMs
   - Orphaned TAP warning logged
   - Orphaned IP reclaimed

**Expected**: all drift corrected in a single reconciliation cycle.

## Validation

- `cargo test -p syfrah-overlay -- reconcile` passes all unit tests
- Daemon logs show reconciliation activity at 30-second intervals
- No panics or unhandled errors during reconciliation
