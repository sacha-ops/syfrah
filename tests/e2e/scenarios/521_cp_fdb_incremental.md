# 521 — FDB incremental update on PlaceVm/RemoveVm commit

## Objective

Verify that when a PlaceVm or RemoveVm command is committed through Raft, each
node incrementally updates its local FDB + ARP proxy entries without a full
rebuild.

## Preconditions

- Two-node mesh (hv-eu-1, hv-eu-2) with Raft initialized.
- Both daemons running.

## Steps

1. **Create a VM on hv-eu-1** in a VPC/subnet:
   ```bash
   ssh root@hv-eu-1 "syfrah compute vm create --name inc-1 --image alpine-3.20 \
     --vcpus 1 --memory 512 --env prod --subnet web --project backend --org acme \
     --ssh-key ~/.ssh/id_ed25519.pub"
   ```

2. **Create a VM on hv-eu-2** in the same subnet:
   ```bash
   ssh root@hv-eu-2 "syfrah compute vm create --name inc-2 --image alpine-3.20 \
     --vcpus 1 --memory 512 --env prod --subnet web --project backend --org acme \
     --ssh-key ~/.ssh/id_ed25519.pub"
   ```

3. **Check FDB entries appear within seconds** (not waiting for reconcile):
   ```bash
   # hv-eu-1 should have FDB entry for inc-2's MAC pointing to hv-eu-2
   ssh root@hv-eu-1 "bridge fdb show | grep '02:00'"
   # hv-eu-2 should have FDB entry for inc-1's MAC pointing to hv-eu-1
   ssh root@hv-eu-2 "bridge fdb show | grep '02:00'"
   ```

4. **Delete inc-2**:
   ```bash
   ssh root@hv-eu-2 "syfrah compute vm delete inc-2 --yes"
   ```

5. **Verify FDB entry for inc-2 is removed from hv-eu-1** within seconds.

## Expected results

- FDB + ARP proxy entries are added within 1-2 seconds of PlaceVm commit.
- FDB + ARP proxy entries are removed within 1-2 seconds of RemoveVm commit.
- Local placements are skipped (no self-FDB entries).
- Daemon logs show: `FDB incremental update applied`.

## Pass criteria

- FDB entry appears on the remote node within 5 seconds of VM creation.
- FDB entry disappears from the remote node within 5 seconds of VM deletion.
