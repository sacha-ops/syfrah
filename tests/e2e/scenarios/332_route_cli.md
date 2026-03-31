# E2E: Route CLI

## Prerequisites
- Daemon running, org/project/env/VPC/subnet exist

## Test

1. **route list --vpc**
   ```
   syfrah route list --vpc <vpc>
   ```
   Expect: table with DESTINATION, TARGET, ORIGIN, STATUS, PRIORITY columns.

2. **route add --vpc --destination --target**
   ```
   syfrah route add --vpc <vpc> --destination 10.99.0.0/24 --target blackhole
   ```
   Expect: success, route displayed.

3. **route delete --vpc --destination**
   ```
   syfrah route delete --vpc <vpc> --destination 10.99.0.0/24
   ```
   Expect: success.

4. **route table create**
   ```
   syfrah route table create isolated --vpc <vpc>
   ```
   Expect: success.

5. **route table list --vpc**
   ```
   syfrah route table list --vpc <vpc>
   ```
   Expect: default + isolated.

6. **route table delete**
   ```
   syfrah route table delete isolated --yes
   ```
   Expect: success.

7. **Error: route delete on system route**
   ```
   syfrah route delete --vpc <vpc> --destination <vpc-cidr>
   ```
   Expect: error.

8. **Error: route table delete on default**
   ```
   syfrah route table delete default --yes
   ```
   Expect: error.

9. **JSON output**
   ```
   syfrah route list --vpc <vpc> --json
   syfrah route table list --vpc <vpc> --json
   ```
   Expect: valid JSON.
