# 524 — ARP proxy from Raft state — zero broadcast

## Objective

Verify that ARP proxy entries are correctly derived from Raft placement state,
so VMs can resolve remote VM MAC addresses without any ARP broadcast crossing
the network.

## Preconditions

- Two-node mesh with Raft initialized.
- VMs on different nodes in the same VPC/subnet.

## Steps

1. **Verify VXLAN is created with `proxy` flag**:
   ```bash
   ssh root@hv-eu-1 "ip -d link show type vxlan | grep proxy"
   ```
   Output should include `proxy` in the VXLAN flags.

2. **Verify proxy_arp is enabled on the VXLAN interface**:
   ```bash
   ssh root@hv-eu-1 "sysctl net.ipv4.conf.syfx-*.proxy_arp"
   ```
   Should output `= 1`.

3. **Verify ARP proxy entries exist for remote VMs**:
   ```bash
   ssh root@hv-eu-1 "ip neigh show dev syfx-HASH nud permanent"
   ```
   Should show an entry mapping web-2's IP to web-2's MAC.

4. **Verify ARP resolution works without broadcast**:
   ```bash
   # From inside web-1, ARP for web-2's IP
   ssh root@hv-eu-1 "ssh root@$WEB1_IP 'arping -c1 -I eth0 $WEB2_IP'"
   ```
   Should get a response immediately from the local VXLAN interface (not
   from the remote VM over the tunnel).

5. **Verify ARP entries are included in cold rebuild**:
   - Restart daemon
   - Check `ip neigh show` for permanent entries on VXLAN interfaces

6. **Verify ARP entries are updated incrementally**:
   - Create a new VM on hv-eu-2
   - Check hv-eu-1 immediately has a permanent neighbor entry for it

## Expected results

- VXLAN interface has `proxy` and `nolearning` flags.
- `proxy_arp=1` sysctl is set on VXLAN interfaces.
- Permanent neighbor entries exist for all remote VMs in local VPCs.
- ARP resolution is local (no broadcast over VXLAN tunnel).
- ARP entries survive daemon restart (cold rebuild).
- ARP entries are added/removed on PlaceVm/RemoveVm (incremental).

## Pass criteria

- `ip neigh show` has permanent entries for remote VMs on VXLAN interfaces.
- ARP resolution from inside a VM returns the correct MAC instantly.
- No ARP broadcast packets observed on the WireGuard tunnel.
