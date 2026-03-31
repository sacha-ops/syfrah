# E2E 261 — FDB VM placement announcements

## Objective

Verify that VM placement announcements are correctly serialized, broadcast, and
handled by fabric peers, enabling FDB and ARP proxy updates across nodes.

## Preconditions

- Two or more fabric nodes joined to the same mesh.
- At least one VPC and subnet configured.

## Steps

1. **Create a VM on Node 1**
   - `syfrah compute vm create --name web-1 --image alpine-3.20 --subnet frontend --project backend --org acme`
   - Verify the VM boots and gets an IP.

2. **Check local FDB entry on Node 1**
   - Node 1 should have a local FDB entry for web-1's MAC pointing to the local bridge (no VTEP).

3. **Check remote FDB entry on Node 2**
   - Node 2 should have received a `VmPlacementAnnouncement` with `action: "add"`.
   - Node 2 should have a static FDB entry: `bridge fdb show dev syfvx-{vpc_id}` includes web-1's MAC with `dst` pointing to Node 1's fabric IPv6.
   - Node 2 should have an ARP proxy entry: `ip neigh show dev syfvx-{vpc_id}` includes web-1's IP/MAC pair.

4. **Delete the VM on Node 1**
   - `syfrah compute vm delete web-1 --project backend --org acme`

5. **Check FDB cleanup on Node 2**
   - Node 2 should have received a `VmPlacementAnnouncement` with `action: "remove"`.
   - The FDB entry and ARP proxy entry for web-1 should be removed from Node 2.

## Expected results

- Add announcements create correct FDB + ARP proxy entries on remote nodes.
- Remove announcements clean up FDB + ARP proxy entries on remote nodes.
- Announcements are JSON-serialized and contain all required fields (vpc_id, vm_id, vm_mac, vm_ip, subnet_id, hosting_node, action).
- No broadcast storms; all FDB entries are static.

## Verification commands

```bash
# On Node 2, after VM creation on Node 1:
bridge fdb show dev syfvx-100 | grep 02:00:0a:00:01:05
ip neigh show dev syfvx-100 | grep 10.0.1.5

# After VM deletion:
bridge fdb show dev syfvx-100 | grep -c 02:00:0a:00:01:05  # should be 0
ip neigh show dev syfvx-100 | grep -c 10.0.1.5              # should be 0
```
