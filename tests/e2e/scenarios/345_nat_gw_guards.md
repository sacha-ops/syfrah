# 345 — NAT Gateway deletion guards

## Preconditions
- Fabric initialized, daemon running
- Org, project, environment, subnet, NAT GW, and route exist

## Steps

1. Try to delete NAT GW with route referencing it:
   ```
   syfrah nat-gw delete main-gw --yes
   ```
   Expected error: "cannot delete nat-gw 'main-gw': referenced by route 0.0.0.0/0 in route table 'rtb-default'"

2. Try to delete NAT GW with VMs in the VPC:
   - Create VM in VPC with NAT GW
   - Remove the route
   - Try delete → error: "cannot delete nat-gw 'main-gw': N VM(s) in VPC are actively using it"

3. Proper deletion sequence:
   - Delete route first: `syfrah route delete --vpc <vpc> --destination 0.0.0.0/0`
   - Delete VM: `syfrah compute vm delete <vm> --yes`
   - Delete NAT GW: `syfrah nat-gw delete main-gw --yes` → success

4. State transitions during deletion:
   - Active → Deleting (nftables rules being removed) → Deleted (record removed)

## Pass criteria
- Cannot delete NAT GW if routes reference it
- Cannot delete NAT GW if VMs in VPC are using it
- Clear error messages with resource names
- Proper state transition through Deleting
