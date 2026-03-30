# Test: Corrupted state files — daemon must fail cleanly, not panic

## Objective

Corrupted state files — daemon must fail cleanly, not panic.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
```

### 2. Test A: truncated JSON (no redb fallback)

```bash
sh -c 'rm -f /root/.syfrah/fabric.redb; echo "{\"mesh_na" > /root/.syfrah/state.json'
```

- Verify: assert_command_fails "e2e-corrupt-1" syfrah fabric start

### 3. Test B: empty file (no redb fallback)

```bash
sh -c 'rm -f /root/.syfrah/fabric.redb; > /root/.syfrah/state.json'
```

- Verify: assert_command_fails "e2e-corrupt-1" syfrah fabric start

### 4. Test C: binary garbage (no redb fallback)

```bash
sh -c 'rm -f /root/.syfrah/fabric.redb; dd if=/dev/urandom of=/root/.syfrah/state.json bs=256 count=1
```

```bash
syfrah fabric leave --yes
```

```bash
rm -rf /root/.syfrah
```

- Verify: assert_command_fails "e2e-corrupt-1" syfrah fabric start
- Verify: The syfrah daemon is running

## Expected results

- All steps complete without errors
- All verification checks pass

## Failure criteria

- Any syfrah command returns a non-zero exit code unexpectedly
- Expected output patterns are missing
- Timeouts exceeded waiting for convergence
