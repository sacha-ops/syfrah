# Test: nftables anti-spoofing + base rules for VM TAP interfaces

## Objective

- The `syfrah` nftables table and `forward` chain are created idempotently
- Per-VM rules enforce anti-spoofing (source MAC + IP validation)
- Default-deny ingress is applied with exceptions for SSH (TCP 22) and ICMP
- Egress traffic is allowed after anti-spoofing checks
- Conntrack allows established/related connections inbound
- Rules are removed cleanly when a VM is deleted

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with at least one org, project, environment, and subnet configured
- `nft` (nftables) is available and the user has root privileges
- No pre-existing `syfrah` nftables table (clean state)

## Steps

### 1. Create a VM to trigger rule application

```bash
syfrah compute vm create --name nft-test-1 --image alpine-3.20 \
  --subnet test-subnet --project test-project --org test-org \
  --vcpus 1 --memory 512 --ssh-key ~/.ssh/id.pub
```

Record the assigned IP and TAP interface name (e.g., `syftap-nft-test-1`).

### 2. Verify table and chain exist

```bash
nft list table inet syfrah
```

Expected: the table exists with a `forward` chain of type `filter`, hook `forward`, priority 0.

### 3. Verify anti-spoofing rules

```bash
nft list chain inet syfrah forward
```

Expected rules (where `{tap}` is the VM's TAP, `{mac}` is the assigned MAC, `{ip}` is the assigned IP):
- `iif {tap} ether saddr != {mac} drop`
- `iif {tap} ip saddr != {ip} drop`

### 4. Verify default-deny ingress

Expected rule:
- `oif {tap} drop`

### 5. Verify SSH allowed

Expected rule:
- `oif {tap} tcp dport 22 accept`

### 6. Verify ICMP allowed

Expected rule:
- `oif {tap} icmp type echo-request accept`

### 7. Verify conntrack

Expected rule:
- `oif {tap} ct state established,related accept`

### 8. Verify egress allowed

Expected rule:
- `iif {tap} accept`

### 9. Verify rule ordering

Anti-spoofing rules (iif rules) must appear before the egress allow rule. The default-deny drop must appear before SSH/ICMP/conntrack accept rules in the chain.

### 10. Delete the VM and verify cleanup

```bash
syfrah compute vm delete --name nft-test-1 --project test-project --org test-org
```

Verify that no rules referencing the deleted VM's TAP remain:
```bash
nft list chain inet syfrah forward | grep -c "syftap-nft-test-1"
```
Expected: 0 matches.

## Expected results

- All rules are applied atomically (no partial state visible)
- Table creation is idempotent (running `vm create` twice does not duplicate the table)
- Anti-spoofing prevents a VM from sending traffic with a spoofed MAC or IP
- Only SSH and ICMP are reachable inbound; all other ports are dropped
- Outbound connections work (egress allow + conntrack for return traffic)
- VM deletion cleanly removes all per-TAP rules

## Failure criteria

- Missing `syfrah` table or `forward` chain after VM creation
- Anti-spoofing rules absent or referencing wrong MAC/IP
- Default-deny ingress missing (all inbound traffic allowed)
- SSH or ICMP blocked when they should be allowed
- Egress blocked (VM cannot reach the internet)
- Stale rules remain after VM deletion
- Non-atomic rule application (partial rules visible during apply)
