# Test: UX — lifecycle command output validation

## Objective

UX — lifecycle command output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name life-node --endpoint 172.20.0.10:51820
```

Wait for the daemon to be responsive (up to 30s).

### 2. Testing: stop when running

Wait 2 seconds.

```bash
syfrah fabric stop
```


### 3. Testing: stop when already stopped

```bash
syfrah fabric stop
```


### 4. Testing: start after stop

```bash
syfrah fabric start
```

- Verify: The syfrah daemon is running

### 5. Testing: token output

```bash
syfrah fabric token
```


### 6. Testing: leave output

```bash
syfrah fabric leave --yes
```


### 7. Testing: double leave

```bash
syfrah fabric leave --yes
```


## Expected results

- stop: clean stop message
- stop when stopped: says not running
- stop when stopped: no raw error
- token: shows syf_sk_ format
- leave: clean message
- leave: no WireGuard warnings
- double leave: says nothing to do

## Failure criteria

- stop: unclear message
- stop when stopped: unclear
- stop when stopped: raw error
- token: missing syf_sk_ format
- leave: unclear message
- leave: WireGuard warnings visible
- double leave: unclear
