# E2E: SG allows port 80 traffic + sg check verification

## Objective

Verify that a security group with a port 80 rule allows HTTP traffic
and that `sg check` accurately reports the verdict.

## Prerequisites

- Running syfrah daemon with a mesh
- At least one org/project/env/subnet configured
- A VM image available (e.g., `alpine-3.20`)

## Steps

1. **Create a security group allowing HTTP and SSH**

   ```bash
   syfrah sg add-rule test-allow-sg \
     --direction ingress --protocol tcp --port 80 \
     --source 0.0.0.0/0

   syfrah sg add-rule test-allow-sg \
     --direction ingress --protocol tcp --port 22 \
     --source 0.0.0.0/0
   ```

2. **Create a VM with this security group**

   ```bash
   syfrah compute vm create \
     --name test-allow-vm --image alpine-3.20 \
     --sg test-allow-sg \
     --subnet frontend --env production --project api --org acme
   ```

3. **Start a simple HTTP server inside the VM**

   ```bash
   ssh user@<vm-ip> "python3 -m http.server 80 &"
   ```

4. **Verify HTTP works** (port 80 is allowed)

   ```bash
   curl --connect-timeout 5 http://<vm-ip>:80/
   ```

   Expected: connection succeeds, returns directory listing or response.

5. **Verify sg check reports ALLOWED for port 80**

   ```bash
   syfrah sg check --vm test-allow-vm --port 80 --protocol tcp
   ```

   Expected output: `ALLOWED: rule ... (priority 100, tcp port 80 from 0.0.0.0/0)`

6. **Verify sg check reports DENIED for port 443** (not in rules)

   ```bash
   syfrah sg check --vm test-allow-vm --port 443 --protocol tcp
   ```

   Expected output: `DENIED: no matching ingress rule`

7. **Verify sg check with source filter**

   ```bash
   syfrah sg check --vm test-allow-vm --port 80 --protocol tcp --source 10.0.0.1
   ```

   Expected output: `ALLOWED: ...` (source 0.0.0.0/0 matches any IP)

## Cleanup

```bash
syfrah compute vm delete test-allow-vm --yes
```

## Pass criteria

- HTTP traffic on port 80 is allowed (server responds)
- `sg check` correctly reports ALLOWED for port 80 and DENIED for port 443
- `sg check` with `--source` flag works correctly
