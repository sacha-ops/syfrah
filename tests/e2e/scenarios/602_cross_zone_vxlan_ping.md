# 602 — Cross-Zone VXLAN Ping

Verify that VMs in the same subnet but different zones can communicate over VXLAN tunnels (WireGuard-backed).

## Prerequisites

- Cluster bootstrapped with Raft leader elected
- Hypervisors registered in at least 2 zones (fsn1 and hel1)
- Default VPC and subnet `web` exist under org `acme`, project `backend`, env `prod`
- Security group `web-sg` allows ICMP and SSH

## Setup

```bash
# Create two VMs in different zones, same subnet
syfrah compute vm create --name xzone-1 --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone fsn1

syfrah compute vm create --name xzone-2 --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg web-sg --zone hel1
```

## Assertions

1. **Different IPs assigned** — distributed IPAM must not collide.
   ```bash
   syfrah compute vm list --project backend --org acme
   # xzone-1 and xzone-2 have different IPs in the same subnet range
   ```

2. **Bidirectional ping works** — VXLAN over WireGuard mesh.
   ```bash
   # From xzone-1: ping <xzone-2-ip> -c 10
   # Expected: 0% packet loss
   
   # From xzone-2: ping <xzone-1-ip> -c 10
   # Expected: 0% packet loss
   ```

3. **Latency is reasonable** — fsn1 ↔ hel1 physical distance adds ~20-30ms.
   ```bash
   # ping <xzone-2-ip> -c 10 | tail -1
   # Expected: rtt avg ~20-30ms (reflecting Falkenstein ↔ Helsinki distance)
   ```

4. **FDB entries present on both nodes**.
   ```bash
   # On the hypervisor hosting xzone-1:
   # bridge fdb show dev vxlan-<vni> | grep <xzone-2-mac>
   # Expected: MAC entry pointing to remote hypervisor's WireGuard IP
   ```

## Expected results

| Check                        | Expected              |
|------------------------------|-----------------------|
| Unique IPs                   | Yes (different IPs)   |
| xzone-1 → xzone-2 ping      | 0% loss               |
| xzone-2 → xzone-1 ping      | 0% loss               |
| Latency (fsn1 ↔ hel1)       | ~20-30ms avg          |
| FDB entries                  | Present on both nodes |

## Pass criteria

- Both VMs get unique IPs from the same subnet
- Bidirectional ping succeeds with 0% loss
- Latency reflects physical distance (not local loopback)
- FDB entries are correctly populated on both hypervisors
