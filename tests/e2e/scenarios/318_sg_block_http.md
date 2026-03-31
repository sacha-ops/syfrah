# E2E: SG blocks port 80 traffic

## Objective

Verify that a security group with no port 80 rule effectively blocks
HTTP traffic to a VM.

## Prerequisites

- Running syfrah daemon with a mesh
- At least one org/project/env/subnet configured
- A VM image available (e.g., `alpine-3.20`)

## Steps

1. **Create a security group that only allows SSH (port 22)**

   ```bash
   syfrah sg add-rule test-block-sg \
     --direction ingress --protocol tcp --port 22 \
     --source 0.0.0.0/0
   ```

2. **Create a VM with this security group**

   ```bash
   syfrah compute vm create \
     --name test-block-vm --image alpine-3.20 \
     --sg test-block-sg \
     --subnet frontend --env production --project api --org acme
   ```

3. **Verify SSH works** (port 22 is allowed)

   ```bash
   ssh user@<vm-ip> echo "SSH OK"
   ```

   Expected: connection succeeds.

4. **Verify HTTP is blocked** (port 80 is NOT in rules)

   ```bash
   # From another node or the host:
   curl --connect-timeout 5 http://<vm-ip>:80/
   ```

   Expected: connection times out or is refused (no port 80 rule).

5. **Verify sg check reports DENIED**

   ```bash
   syfrah sg check --vm test-block-vm --port 80 --protocol tcp
   ```

   Expected output: `DENIED: no matching ingress rule`

6. **Verify sg check reports ALLOWED for port 22**

   ```bash
   syfrah sg check --vm test-block-vm --port 22 --protocol tcp
   ```

   Expected output: `ALLOWED: rule ... (priority 100, tcp port 22 from 0.0.0.0/0)`

## Cleanup

```bash
syfrah compute vm delete test-block-vm --yes
```

## Pass criteria

- HTTP traffic on port 80 is blocked (timeout/refused)
- SSH traffic on port 22 is allowed
- `sg check` correctly reports DENIED for port 80 and ALLOWED for port 22
