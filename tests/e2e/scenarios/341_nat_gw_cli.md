# 341 — NAT Gateway CLI

## Preconditions
- Fabric initialized, daemon running
- Org, project, environment, subnet exist

## Steps

1. Create NAT GW:
   ```
   syfrah nat-gw create main-gw --vpc acme-backend-default --subnet frontend
   ```
   Verify: shows name, VPC, subnet, public IP, state.

2. List NAT GWs:
   ```
   syfrah nat-gw list --vpc acme-backend-default
   syfrah nat-gw list --json
   ```
   Verify: table and JSON output.

3. Show NAT GW:
   ```
   syfrah nat-gw show main-gw
   ```
   Verify: all fields displayed.

4. Delete NAT GW:
   ```
   syfrah nat-gw delete main-gw --yes
   ```
   Verify: deleted, no longer in list.

5. Error paths:
   - Create with non-existent VPC → error
   - Create with non-existent subnet → error
   - Show non-existent → error
   - Delete non-existent → error

## Pass criteria
- All CLI commands work as documented in --help
- Error messages are clear and actionable
- JSON output is valid
