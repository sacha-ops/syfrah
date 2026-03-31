# 342 — NAT Gateway nftables masquerade

## Preconditions
- Fabric initialized, daemon running
- Org, project, environment, subnet exist

## Steps

1. Create NAT GW:
   ```
   syfrah nat-gw create main-gw --vpc <vpc> --subnet frontend
   ```
   Verify: state transitions to Active.

2. Check nftables rules:
   ```
   nft list ruleset | grep masquerade
   ```
   Verify: masquerade rule exists for the subnet CIDR on the public interface.

3. Delete NAT GW (after removing routes):
   ```
   syfrah nat-gw delete main-gw --yes
   ```

4. Check nftables rules again:
   ```
   nft list ruleset | grep masquerade
   ```
   Verify: masquerade rule removed.

5. Error path: if nftables apply fails, NAT GW state = Failed.

## Pass criteria
- NAT GW creation applies masquerade nftables rules
- NAT GW transitions to Active after successful nftables apply
- NAT GW deletion removes masquerade rules
- Failed nftables apply results in Failed state
