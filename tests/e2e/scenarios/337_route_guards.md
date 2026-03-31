# E2E: Deletion Guards + Reconciliation

## Prerequisites
- Daemon running, org/project/env/VPC/subnet exist

## Test

1. **Cannot delete default route table**
   ```
   syfrah route table delete default --yes
   ```
   Expect: error "cannot delete the default route table".

2. **Cannot delete route table with associated subnets**
   ```
   syfrah route table create guarded --vpc <vpc>
   syfrah route table associate guarded --subnet <subnet>
   syfrah route table delete guarded --yes
   ```
   Expect: error about associated subnets.

3. **Cannot delete system routes**
   ```
   syfrah route delete --vpc <vpc> --destination <vpc-cidr>
   ```
   Expect: error "cannot delete system-managed route".

4. **Can delete user routes**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target blackhole
   syfrah route delete --vpc <vpc> --destination 10.99.0.0/24
   ```
   Expect: success.

5. **Cleanup**
   ```
   syfrah route table disassociate --subnet <subnet>
   syfrah route table delete guarded --yes
   ```
