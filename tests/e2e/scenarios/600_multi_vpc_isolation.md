# 600 — Multi-VPC Isolation

Verify that VMs in different VPCs on the same zone CANNOT communicate (different VNI = total isolation).

## Prerequisites

- Cluster bootstrapped with Raft leader elected
- At least 1 hypervisor registered in zone `fsn1`
- Org `acme`, project `backend`, env `prod` exist

## Setup

```bash
# Create two isolated VPCs
syfrah vpc create vpc-a --project backend --org acme --cidr 10.1.0.0/16
syfrah subnet create web-a --env prod --project backend --org acme --vpc vpc-a

syfrah vpc create vpc-b --project backend --org acme --cidr 10.2.0.0/16
syfrah subnet create web-b --env prod --project backend --org acme --vpc vpc-b

# Security groups for both VPCs (allow SSH + ICMP for testing)
syfrah sg create sg-a --vpc vpc-a
syfrah sg add-rule sg-a --direction ingress --protocol tcp --port 22 --source 0.0.0.0/0
syfrah sg add-rule sg-a --direction ingress --protocol icmp --source 0.0.0.0/0

syfrah sg create sg-b --vpc vpc-b
syfrah sg add-rule sg-b --direction ingress --protocol tcp --port 22 --source 0.0.0.0/0
syfrah sg add-rule sg-b --direction ingress --protocol icmp --source 0.0.0.0/0

# NAT gateways for internet egress
syfrah nat-gw create gw-a --vpc vpc-a --subnet web-a
syfrah nat-gw create gw-b --vpc vpc-b --subnet web-b

# Create VMs in the SAME zone but DIFFERENT VPCs
syfrah compute vm create --name iso-a --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web-a --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg sg-a --zone fsn1

syfrah compute vm create --name iso-b --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web-b --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg sg-b --zone fsn1
```

## Assertions

1. **VM-A CANNOT ping VM-B** — different VPCs have different VNIs, so VXLAN traffic is completely isolated even on the same physical host.
   ```bash
   # From the hypervisor hosting iso-a, exec into the VM network namespace:
   # ping <iso-b-ip> -c 4 -W 2
   # Expected: 100% packet loss, exit code != 0
   ```

2. **VM-A CAN ping its own gateway** — intra-VPC connectivity works.
   ```bash
   # ping 10.1.0.1 -c 4 -W 2
   # Expected: 0% packet loss
   ```

3. **VM-B CAN ping its own gateway** — second VPC also functional.
   ```bash
   # ping 10.2.0.1 -c 4 -W 2
   # Expected: 0% packet loss
   ```

## Expected results

| Check                        | Expected        |
|------------------------------|-----------------|
| iso-a ping iso-b             | TIMEOUT / 100% loss |
| iso-a ping 10.1.0.1 (gw)    | 0% loss         |
| iso-b ping 10.2.0.1 (gw)    | 0% loss         |

## Pass criteria

- Cross-VPC ping MUST fail with 100% packet loss
- Intra-VPC gateway ping MUST succeed
- No nftables leaks between VNIs
