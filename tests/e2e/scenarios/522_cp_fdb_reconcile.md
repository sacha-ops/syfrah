# 522 — FDB reconciliation — drift detection and correction

## Objective

Verify that the periodic reconciliation loop (every 5s) detects and corrects
FDB drift without a full rebuild.

## Preconditions

- Two-node mesh with Raft and VMs on different nodes in the same VPC/subnet.
- FDB entries exist on both nodes.

## Steps

1. **Manually delete an FDB entry** to simulate drift:
   ```bash
   # Get the MAC of web-2 on server 1
   ssh root@hv-eu-1 "bridge fdb show | grep '02:00'"
   # Delete it
   ssh root@hv-eu-1 "bridge fdb del 02:00:xx:xx:xx:xx dev syfx-HASH"
   ```

2. **Wait 10 seconds** for reconciliation to detect drift.

3. **Verify the FDB entry is restored**:
   ```bash
   ssh root@hv-eu-1 "bridge fdb show | grep '02:00'"
   ```

4. **Manually add a stale FDB entry** to simulate an old placement:
   ```bash
   ssh root@hv-eu-1 "bridge fdb add 02:00:ff:ff:ff:ff dev syfx-HASH dst fd12::99"
   ```

5. **Wait 10 seconds** for reconciliation.

6. **Verify the stale entry is removed**:
   ```bash
   ssh root@hv-eu-1 "bridge fdb show | grep '02:00:ff'"
   ```
   Should be empty.

## Expected results

- Missing FDB entries are re-added within one reconciliation cycle (5s).
- Stale FDB entries (for MACs not in the expected set) are removed.
- Only drifted entries are touched — not the entire FDB table.
- Reconciliation logs: `fdb_added`, `fdb_removed` counts.

## Pass criteria

- Deleted FDB entry is restored within 10 seconds.
- Stale FDB entry is removed within 10 seconds.
- No spurious re-application of existing correct entries.
