# E2E: Auto-Add Local Routes on Subnet Creation

## Prerequisites
- Daemon running, org/project/env/VPC exist

## Test

1. **Create subnet and verify local route**
   ```
   syfrah subnet create test-subnet --env <env> --project <project> --org <org> --vpc <vpc>
   syfrah route list --vpc <vpc>
   ```
   Expect: system local route for the subnet CIDR appears automatically.

2. **Delete subnet and verify route removed**
   ```
   syfrah subnet delete test-subnet --yes
   syfrah route list --vpc <vpc>
   ```
   Expect: subnet CIDR route is removed, VPC CIDR route remains.

3. **Multiple subnets = multiple routes**
   Create 2 subnets, verify 2 local routes (plus VPC CIDR).
