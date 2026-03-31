# E2E: Subnet Route Table Association

## Prerequisites
- Daemon running, org/project/env/VPC/subnet exist

## Test

1. **Default association**
   Subnet uses default route table when no explicit association exists.

2. **Associate subnet with custom table**
   ```
   syfrah route table create custom --vpc <vpc>
   syfrah route table associate custom --subnet <subnet>
   ```
   Expect: success message.

3. **Disassociate subnet**
   ```
   syfrah route table disassociate --subnet <subnet>
   ```
   Expect: success, subnet reverts to default table.

4. **Cannot delete table with associated subnets**
   ```
   syfrah route table associate custom --subnet <subnet>
   syfrah route table delete custom --yes
   ```
   Expect: error about associated subnets.

5. **Cleanup**
   ```
   syfrah route table disassociate --subnet <subnet>
   syfrah route table delete custom --yes
   ```
   Expect: success.
