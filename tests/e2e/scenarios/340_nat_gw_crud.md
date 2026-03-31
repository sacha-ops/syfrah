# 340 — NAT Gateway CRUD

## Preconditions
- Fabric initialized, daemon running
- Org, project, environment, and subnet exist

## Steps

1. Create a NAT Gateway:
   ```
   syfrah nat-gw create main-gw --vpc <vpc> --subnet <subnet>
   ```
   Verify: NAT GW created with Pending state, public IP auto-detected.

2. List NAT Gateways:
   ```
   syfrah nat-gw list --vpc <vpc>
   ```
   Verify: main-gw appears in list.

3. Show NAT Gateway:
   ```
   syfrah nat-gw show main-gw
   ```
   Verify: all fields displayed (name, VPC, subnet, public IP, state).

4. Verify state transitions:
   - After creation: state = Pending
   - After nftables apply: state = Active
   - On failure: state = Failed

5. Delete NAT Gateway:
   ```
   syfrah nat-gw delete main-gw --yes
   ```
   Verify: NAT GW removed from list.

6. Duplicate name rejected:
   - Create main-gw, then create main-gw again → error.

## Pass criteria
- All CRUD operations work
- State lifecycle follows Pending → Active → Deleting → Deleted
- Public IP is auto-detected from the node's default interface
