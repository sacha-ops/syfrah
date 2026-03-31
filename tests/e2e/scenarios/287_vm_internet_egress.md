# 287 — Internet egress via SNAT

## Goal
Verify that a VM can access the internet via SNAT masquerade through the
host's network. The overlay NAT rule should masquerade outbound traffic
from the subnet CIDR.

## Prerequisites
- Syfrah daemon running on a node with internet access
- Image `alpine-3.20` available locally
- Org hierarchy with VPC and subnet configured
- Host has IP forwarding enabled (`sysctl net.ipv4.ip_forward=1`)

## Steps

### 1. Create VM with networking
```bash
syfrah compute vm create --name egress-test --image alpine-3.20 \
  --subnet frontend --env production --project backend --org acme \
  --vcpus 1 --memory 1024 --ssh-key ~/.ssh/id_ed25519.pub
```
**Expected**: VM created with IP in the subnet range.

### 2. Verify NAT rules on host
```bash
nft list ruleset | grep masquerade
```
**Expected**: masquerade rule for subnet CIDR (e.g. `10.0.1.0/24`).

### 3. Verify IP forwarding
```bash
sysctl net.ipv4.ip_forward
```
**Expected**: `net.ipv4.ip_forward = 1`.

### 4. Test DNS resolution from inside VM
```bash
ssh root@10.0.1.3 "nslookup example.com"
```
**Expected**: DNS resolves successfully via `8.8.8.8` or `1.1.1.1`.

### 5. Test HTTP egress
```bash
ssh root@10.0.1.3 "wget -q -O /dev/null -T 10 http://example.com && echo OK"
```
**Expected**: prints `OK` — HTTP request succeeds through NAT.

### 6. Test HTTPS egress
```bash
ssh root@10.0.1.3 "wget -q -O /dev/null -T 10 https://example.com && echo OK"
```
**Expected**: prints `OK` — HTTPS works through NAT.

### 7. Verify source IP is masqueraded
From the VM, check the outbound source:
```bash
ssh root@10.0.1.3 "wget -q -O - https://ifconfig.me"
```
**Expected**: returns the host's public IP, NOT the VM's private IP.

### 8. Cleanup
```bash
syfrah compute vm delete egress-test
```
**Expected**: VM deleted, NAT rules remain (other VMs may use them).

## Pass criteria
- VM can resolve DNS
- VM can reach external HTTP and HTTPS endpoints
- Outbound traffic is masqueraded through the host's public IP
- No direct exposure of VM private IP to the internet
