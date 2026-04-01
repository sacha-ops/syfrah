# Test: Graceful shutdown protocol

## Objective

Verify graceful shutdown:
- SIGTERM triggers graceful shutdown
- In-flight requests complete (up to 30s grace)
- VMs continue running after Forge restart
- Reconciler re-discovers resources on restart

## Steps

### 1. Create a VM before shutdown test

```bash
syfrah compute vm create --name shutdown-test --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme --ssh-key ~/.ssh/id_ed25519.pub
sleep 15
syfrah compute vm get shutdown-test
```

### 2. Send SIGTERM to the daemon

```bash
kill -TERM $(cat ~/.syfrah/daemon.pid)
sleep 5
```

**Expected:** Daemon exits cleanly. VM process continues running.

### 3. Restart and verify VM survived

```bash
syfrah fabric start
sleep 3
syfrah compute vm get shutdown-test
```

**Expected:** VM is re-discovered and shows as running.

### 4. Cleanup

```bash
syfrah compute vm delete shutdown-test --yes
```

## Pass criteria

- SIGTERM triggers graceful shutdown (stop accepting requests)
- In-flight request grace period (30s default)
- VMs survive daemon restart (separate processes)
- ShutdownController tracks in-flight request count
