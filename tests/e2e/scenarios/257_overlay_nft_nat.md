# Test: nftables SNAT masquerade for overlay subnets

## Objective

Verify that SNAT masquerade rules are correctly applied and removed for VM
subnets, enabling internet egress through the host's public IP.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Root / NET_ADMIN capabilities
- `nft` (nftables) available in PATH
- `net.ipv4.ip_forward=1` enabled

## Steps

### 1. Create an org, project, environment, and subnet

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create staging --project backend --org acme
syfrah subnet create web --env staging --project backend --org acme
```

Record the subnet CIDR (e.g. `10.1.1.0/24`) and VPC bridge name (e.g. `syfbr-100`).

### 2. Create a VM in the subnet

```bash
syfrah compute vm create --name nat-test-1 --image alpine-3.20 \
  --subnet web --project backend --org acme --vcpus 1 --memory 512
```

Record the assigned IP address.

### 3. Verify NAT rules exist

```bash
nft list table ip syfrah_nat
```

- Verify: the `syfrah_nat` table exists
- Verify: the `postrouting` chain exists with `type nat hook postrouting priority 100`
- Verify: a masquerade rule matches `oif != "syfbr-..."` and `ip saddr 10.1.1.0/24`

### 4. Verify IP forwarding

```bash
sysctl net.ipv4.ip_forward
```

- Verify: value is `1`

### 5. Verify internet egress from VM

```bash
ssh root@<vm-ip> "ping -c 3 8.8.8.8"
```

- Verify: ping succeeds (ICMP replies received)

### 6. Create a second subnet and VM

```bash
syfrah subnet create api --env staging --project backend --org acme
syfrah compute vm create --name nat-test-2 --image alpine-3.20 \
  --subnet api --project backend --org acme --vcpus 1 --memory 512
```

```bash
nft list table ip syfrah_nat
```

- Verify: a second masquerade rule exists for the new subnet CIDR

### 7. Delete the first VM and verify rule cleanup

```bash
syfrah compute vm delete nat-test-1 --project backend --org acme
```

If no other VMs remain in the `web` subnet:

```bash
nft list table ip syfrah_nat
```

- Verify: the masquerade rule for `10.1.1.0/24` has been removed
- Verify: the masquerade rule for the `api` subnet still exists

### 8. Clean up

```bash
syfrah compute vm delete nat-test-2 --project backend --org acme
syfrah subnet delete web --env staging --project backend --org acme
syfrah subnet delete api --env staging --project backend --org acme
syfrah env destroy staging --project backend --org acme
syfrah project delete backend --org acme
syfrah org delete acme
```

## Expected results

- `syfrah_nat` table and `postrouting` chain are created automatically on first VM
- Each subnet gets its own masquerade rule with the correct CIDR
- Masquerade rules exclude the VPC bridge interface (`oif != "syfbr-..."`)
- VMs can reach the internet via SNAT through the host's public IP
- NAT rules are cleaned up when the last VM in a subnet is deleted
- IP forwarding is enabled (`net.ipv4.ip_forward=1`)

## Failure criteria

- `syfrah_nat` table or postrouting chain is missing after VM creation
- Masquerade rule does not match the correct subnet CIDR
- VM cannot reach the internet (ping to 8.8.8.8 fails)
- NAT rules are not removed after VM and subnet deletion
- IP forwarding is not enabled
