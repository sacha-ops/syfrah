# 520 — FDB cold rebuild from Raft placements

## Objective

Verify that on daemon startup (after Raft state machine loads), FDB and ARP
proxy entries are rebuilt from the placement store for all remote VMs in local
VPCs.

## Preconditions

- Two-node mesh (hv-eu-1, hv-eu-2) with Raft initialized and both nodes joined.
- At least one VM created on each node in the same VPC/subnet.
- Both VMs have different IPs (distributed IPAM).

## Steps

1. **Create VMs on both nodes** — one on hv-eu-1, one on hv-eu-2, same subnet.
2. **Verify FDB entries exist** on both nodes:
   ```bash
   ssh root@hv-eu-1 "bridge fdb show | grep '02:00'"
   ssh root@hv-eu-2 "bridge fdb show | grep '02:00'"
   ```
   Each should have an FDB entry pointing to the other node's fabric IPv6.

3. **Flush FDB entries manually** to simulate a cold state:
   ```bash
   ssh root@hv-eu-1 "bridge fdb flush dev syfx-*"
   ssh root@hv-eu-2 "bridge fdb flush dev syfx-*"
   ```

4. **Restart the daemon** on both nodes:
   ```bash
   ssh root@hv-eu-1 "syfrah fabric stop && sleep 2 && syfrah fabric start"
   ssh root@hv-eu-2 "syfrah fabric stop && sleep 2 && syfrah fabric start"
   ```

5. **Verify FDB entries are rebuilt**:
   ```bash
   ssh root@hv-eu-1 "bridge fdb show | grep '02:00'"
   ssh root@hv-eu-2 "bridge fdb show | grep '02:00'"
   ```
   FDB entries must be present again, pointing to the correct remote fabric
   IPv6 addresses.

6. **Verify ARP proxy entries are rebuilt**:
   ```bash
   ssh root@hv-eu-1 "ip neigh show | grep '10.1.0'"
   ssh root@hv-eu-2 "ip neigh show | grep '10.1.0'"
   ```

## Expected results

- After daemon restart, FDB entries for remote VMs are automatically restored.
- ARP proxy entries for remote VMs are automatically restored.
- Local VM placements are skipped (no self-FDB entries).
- The rebuild is logged: `FDB cold rebuild from Raft placements complete`.

## Pass criteria

- FDB entries present on both nodes after restart.
- ARP proxy entries present on both nodes after restart.
- No errors in daemon logs related to FDB rebuild.
