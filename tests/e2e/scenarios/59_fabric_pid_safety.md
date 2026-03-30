# Test: Fabric Pid Safety

## Objective

Fabric Pid Safety.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820
```

Wait for the daemon to be responsive (up to 30s).

### 2. Execute the test

```bash
syfrah fabric stop
```

```bash
bash -c 'echo 1 > /root/.syfrah/daemon.pid'
```

```bash
syfrah fabric stop
```


## Expected results

- refuses to kill non-syfrah PID
- PID 1 (init) still alive

## Failure criteria

- did not refuse to kill fake PID
- PID 1 was killed!
