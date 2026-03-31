# 343 — NAT Gateway route table integration

## Preconditions
- Fabric initialized, daemon running
- Org, project, environment, subnet exist

## Steps

1. Create NAT GW:
   ```
   syfrah nat-gw create main-gw --vpc <vpc> --subnet frontend
   ```

2. Verify auto-created default route:
   ```
   syfrah route list --vpc <vpc>
   ```
   Expected: `0.0.0.0/0 -> nat-gw:main-gw` route exists in default table.

3. Manually add route pointing to NAT GW:
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/16 --target nat-gw:main-gw
   ```
   Verify: route added successfully.

4. Error: route to non-existent NAT GW:
   ```
   syfrah route add --vpc <vpc> --destination 10.98.0.0/16 --target nat-gw:nonexistent
   ```
   Expected: error "nat-gw 'nonexistent' not found".

5. No duplicate default route:
   Create a second NAT GW → should NOT add another 0.0.0.0/0 route.

## Pass criteria
- Auto-created default route on NAT GW creation
- Route validation checks NAT GW exists and is Active
- No duplicate default routes
