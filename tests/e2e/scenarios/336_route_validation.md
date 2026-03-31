# E2E: Route Target Validation

## Prerequisites
- Daemon running, org/project/env/VPC exist

## Test

1. **Invalid target type**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target invalid
   ```
   Expect: error about invalid route target.

2. **Peering target that doesn't exist**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target peering:nonexistent
   ```
   Expect: error about target resource not found.

3. **Blackhole target is always valid**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target blackhole
   ```
   Expect: success.

4. **Local target is always valid**
   ```
   syfrah route add --vpc <vpc> --destination 10.88.0.0/24 --target local
   ```
   Expect: success.

5. **Cleanup**
   ```
   syfrah route delete --vpc <vpc> --destination 10.99.0.0/24
   syfrah route delete --vpc <vpc> --destination 10.88.0.0/24
   ```
