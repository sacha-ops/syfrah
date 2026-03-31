# E2E: Route Types (System/User/Propagated)

## Prerequisites
- Daemon running, org/project/env/VPC/subnet exist

## Test

1. **System routes auto-created**
   ```
   syfrah route list --vpc <vpc>
   ```
   Expect: system local routes for VPC CIDR and subnet CIDRs.

2. **Add a user route (blackhole)**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target blackhole
   ```
   Expect: success, origin=user, priority=100.

3. **Cannot delete system routes**
   ```
   syfrah route delete --vpc <vpc> --destination <vpc-cidr>
   ```
   Expect: error "cannot delete system-managed route".

4. **Can delete user routes**
   ```
   syfrah route delete --vpc <vpc> --destination 10.99.0.0/24
   ```
   Expect: success.

5. **Propagated routes are undeletable**
   (Requires peering to generate propagated routes — tested in 337)
