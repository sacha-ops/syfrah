# E2E: Blackhole Routes

## Prerequisites
- Daemon running, org/project/env/VPC/subnet exist

## Test

1. **Add a blackhole route**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target blackhole
   ```
   Expect: success, status=active.

2. **Verify route in list**
   ```
   syfrah route list --vpc <vpc>
   ```
   Expect: blackhole route with target "blackhole", status "active".

3. **Delete the blackhole route**
   ```
   syfrah route delete --vpc <vpc> --destination 10.99.0.0/24
   ```
   Expect: success.

4. **Verify route removed**
   ```
   syfrah route list --vpc <vpc>
   ```
   Expect: blackhole route no longer present.

## Note
nftables DROP rule enforcement is best tested with actual network traffic.
The data plane (nftables rules) is applied by the daemon reconciliation loop.
