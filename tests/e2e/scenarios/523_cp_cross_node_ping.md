# 523 — Cross-node VXLAN connectivity — VMs on different hypervisors can ping

## Objective

Verify end-to-end cross-node connectivity: a VM on hypervisor A can ping a VM
on hypervisor B through VXLAN over WireGuard.

## Architecture

```
VM-A (10.1.0.x on hv-eu-1) sends ping to 10.1.0.y
  -> eth0 -> veth -> bridge syfb-{vpc}
  -> bridge FDB lookup: MAC 02:00:0a:01:00:yy -> dst {hv-eu-2 fabric IPv6}
  -> VXLAN encap (VNI) -> syfrah0 (WireGuard encrypt)
  -> internet -> hv-eu-2
  -> syfrah0 (WireGuard decrypt) -> VXLAN decap
  -> bridge syfb-{vpc} -> veth -> VM-B (10.1.0.y)
```

## Preconditions

- Two-node mesh: hv-eu-1 (65.109.130.108), hv-eu-2 (37.27.12.205)
- WireGuard tunnel established (fabric init + join)
- Raft cluster initialized (controlplane init + join)
- Org/project/env/subnet/VPC created
- Hypervisors registered and enabled

## Steps

1. **Create VM on hv-eu-1**:
   ```bash
   ssh root@65.109.130.108 "syfrah compute vm create --name web-1 \
     --image alpine-3.20 --vcpus 1 --memory 512 --env prod --subnet web \
     --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg"
   ```

2. **Create VM on hv-eu-2**:
   ```bash
   ssh root@37.27.12.205 "syfrah compute vm create --name web-2 \
     --image alpine-3.20 --vcpus 1 --memory 512 --env prod --subnet web \
     --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg"
   ```

3. **Get IPs** — they MUST be different (distributed IPAM):
   ```bash
   WEB1_IP=$(ssh root@65.109.130.108 "syfrah compute vm get web-1 --json" | jq -r .ip)
   WEB2_IP=$(ssh root@37.27.12.205 "syfrah compute vm get web-2 --json" | jq -r .ip)
   ```

4. **Verify FDB entries**:
   ```bash
   # hv-eu-1 should have FDB entry for web-2's MAC
   ssh root@65.109.130.108 "bridge fdb show | grep '02:00'"
   # hv-eu-2 should have FDB entry for web-1's MAC
   ssh root@37.27.12.205 "bridge fdb show | grep '02:00'"
   ```

5. **Verify ARP proxy entries**:
   ```bash
   ssh root@65.109.130.108 "ip neigh show | grep '10.1.0'"
   ssh root@37.27.12.205 "ip neigh show | grep '10.1.0'"
   ```

6. **THE MOMENT OF TRUTH — Cross-node ping**:
   ```bash
   # From web-1 (hv-eu-1) -> web-2 (hv-eu-2)
   ssh root@65.109.130.108 "ssh root@$WEB1_IP 'ping -c3 -W5 $WEB2_IP'"
   # From web-2 (hv-eu-2) -> web-1 (hv-eu-1)
   ssh root@37.27.12.205 "ssh root@$WEB2_IP 'ping -c3 -W5 $WEB1_IP'"
   ```

7. **Verify SSH between VMs**:
   ```bash
   ssh root@65.109.130.108 "ssh root@$WEB1_IP 'hostname'"
   ssh root@37.27.12.205 "ssh root@$WEB2_IP 'hostname'"
   ```

8. **Verify internet from both VMs**:
   ```bash
   ssh root@65.109.130.108 "ssh root@$WEB1_IP 'ping -c1 8.8.8.8'"
   ssh root@37.27.12.205 "ssh root@$WEB2_IP 'ping -c1 8.8.8.8'"
   ```

## Expected results

- VMs get different IPs from distributed IPAM.
- FDB entries exist on both nodes pointing to each other's fabric IPv6.
- ARP proxy entries exist for remote VMs.
- Cross-node ping succeeds in both directions.
- SSH to both VMs works.
- Internet access works from both VMs.

## Pass criteria

- `ping -c3` from web-1 to web-2 exits with code 0 (3/3 packets received).
- `ping -c3` from web-2 to web-1 exits with code 0 (3/3 packets received).
- `ping -c1 8.8.8.8` from both VMs exits with code 0.
