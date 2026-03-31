# E2E: Route Table CRUD

## Prerequisites
- Daemon running, org/project/env/VPC exist

## Test

1. **List route tables in VPC (default exists)**
   ```
   syfrah route table list --vpc <vpc>
   ```
   Expect: at least "default" table listed.

2. **Create a custom route table**
   ```
   syfrah route table create custom --vpc <vpc>
   ```
   Expect: success, table name "custom" shown.

3. **List again — 2 tables**
   ```
   syfrah route table list --vpc <vpc>
   ```
   Expect: "default" + "custom".

4. **Delete the custom table**
   ```
   syfrah route table delete custom --yes
   ```
   Expect: success.

5. **Cannot delete the default table**
   ```
   syfrah route table delete default --yes
   ```
   Expect: error "cannot delete the default route table".

6. **Default route table has VPC CIDR route**
   ```
   syfrah route list --vpc <vpc>
   ```
   Expect: system local route for VPC CIDR.
