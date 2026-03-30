# Test: nftables peering FORWARD rules between VPC bridges

## Objective

- Peered VPC bridges allow FORWARD traffic in both directions
- Unpeered VPC bridges block cross-bridge forwarding (default deny)
- Removing peering rules restores the default deny policy

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with at least one org, project, and environment
- Two VPCs with bridges created (e.g. `syfbr-100` and `syfbr-200`)
- nftables installed and the `syfrah` table present
- Root access

## Steps

### 1. Verify default isolation (no peering)

Before peering, confirm that the `syfrah` forward chain has no rules for
cross-bridge traffic between the two VPC bridges.

```bash
nft list chain inet syfrah forward
```

- Expected: no rules matching `iif syfbr-100 oif syfbr-200` or the reverse

### 2. Peer the two VPCs

```bash
syfrah vpc peer --from vpc-a --to vpc-b
```

- Expected: command succeeds

### 3. Verify FORWARD rules exist

```bash
nft list chain inet syfrah forward
```

- Expected: two rules present:
  - `iif "syfbr-100" oif "syfbr-200" accept`
  - `iif "syfbr-200" oif "syfbr-100" accept`

### 4. Test traffic flows (if VMs exist in both VPCs)

From a VM in VPC-A, ping a VM in VPC-B:

```bash
ping -c 3 <vpc-b-vm-ip>
```

- Expected: ping succeeds (3/3 packets)

### 5. Unpeer the VPCs

```bash
syfrah vpc unpeer --from vpc-a --to vpc-b
```

- Expected: command succeeds

### 6. Verify FORWARD rules removed

```bash
nft list chain inet syfrah forward
```

- Expected: no rules matching `iif syfbr-100 oif syfbr-200` or the reverse

### 7. Confirm traffic blocked after unpeer (if VMs exist)

```bash
ping -c 3 -W 2 <vpc-b-vm-ip>
```

- Expected: ping fails (0/3 packets received)

## Expected results

- Peering creates symmetric FORWARD accept rules between the two bridges
- Unpeering removes those rules completely
- Default VPC isolation (deny cross-bridge forward) is restored after unpeer

## Failure criteria

- Peering only adds a rule in one direction (asymmetric)
- Rules persist after unpeer
- Cross-bridge traffic succeeds without explicit peering
- nftables errors during rule application or removal
