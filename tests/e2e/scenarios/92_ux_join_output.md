# Test: UX — join command output validation

## Objective

UX — join command output validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

```bash
syfrah fabric init --name test-mesh --node-name server-1 --endpoint 172.20.0.10:51820
```

Start the peering service:
```bash
syfrah fabric start
```

Wait for the daemon to be responsive (up to 30s).

### 2. Testing: join happy path output

```bash
syfrah fabric join 172.20.0.10:51821 --node-name server-2 --endpoint 172.20.0.11:51820 --pin "<PIN>"
```

- Verify: Output contains `joined\|approved\|accepted`
- Verify: Output contains `fd[0-9a-f]`

### 3. Testing: join target unreachable

```bash
syfrah fabric join 172.20.0.99:51821 --node-name server-3 --endpoint 172.20.0.12:51820 --pin 1234
```


### 4. Testing: join when state exists

```bash
syfrah fabric join 172.20.0.10:51821 --node-name server-2b --endpoint 172.20.0.11:51820 --pin "<PIN>"
```


### 5. Testing: join no args

```bash
syfrah fabric join
```


## Expected results

- join output shows approval
- join output shows IPv6
- join output shows approval method
- join unreachable: no raw OS error
- join with state: mentions existing state
- join with state: suggests 'syfrah fabric leave' (full path)
- join with state: mentions leave command
- join no args: shows usage/help

## Failure criteria

- join output missing approval confirmation
- join output missing IPv6
- join output missing approval method
- join unreachable shows raw OS error
- join with state: unclear message
- join with state: doesn't suggest leave
- join no args: no usage info
