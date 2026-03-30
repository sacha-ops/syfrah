# Test: UX Errors — lifecycle command error messages

## Objective

UX Errors — lifecycle command error messages.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name life-node-1 --endpoint 172.20.0.10:51820
```

### 2. Testing: stop when not running

```bash
syfrah fabric stop
```


### 3. Testing: start without init

```bash
syfrah fabric start
```


### 4. Testing: leave without mesh

```bash
syfrah fabric leave --yes
```


### 5. Testing: double leave

```bash
syfrah fabric leave --yes
```


### 6. Testing: peers without mesh

```bash
syfrah fabric peers
```


### 7. Testing: status without mesh

```bash
syfrah fabric status
```


### 8. Testing: leave then join cycle

Wait 2 seconds.

```bash
syfrah fabric leave --yes
```

```bash
syfrah fabric init --name re-mesh --node-name life-node-1 --endpoint 172.20.0.10:51820
```


## Expected results

- stop not running: helpful message
- start without init: suggests init/join
- leave no mesh: says nothing to do
- double leave: says nothing to do
- peers no mesh: suggests init/join
- status no mesh: suggests init/join
- leave then init: works first try

## Failure criteria

- stop not running: unclear
- start without init: unclear
- leave no mesh: unclear
- double leave: unclear
- peers no mesh: unclear
- status no mesh: unclear
- leave then init: failed
