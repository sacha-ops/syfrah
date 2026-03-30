# Test: UX Errors — join error messages

## Objective

UX Errors — join error messages.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Set up the mesh

On the first node (err-join-srv-1):
```bash
syfrah fabric init --name test-mesh --node-name err-join-srv-1 --endpoint 172.20.0.10:51820
syfrah fabric start
```

On err-join-srv-2:
```bash
syfrah fabric join --leader 172.20.0.10:51820 --node-name err-join-srv-2 --endpoint 172.20.0.11:51820
```

Wait until each node sees 1 active peer(s) (timeout: 30s).

### 2. Testing: join to unreachable IP

```bash
syfrah fabric join 172.20.0.99:51821 --node-name err-join-2 --endpoint 172.20.0.11:51820 --pin 1234
```


### 3. Testing: join when peering not active

```bash
syfrah fabric join 172.20.0.10:51821 --node-name err-join-2 --endpoint 172.20.0.11:51820 --pin 1234
```


### 4. Testing: join when state exists

```bash
syfrah fabric join 172.20.0.10:51821 --node-name err-join-2b --endpoint 172.20.0.11:51820 --pin "<PIN>"
```


### 5. Testing: join no arguments

```bash
syfrah fabric join
```


### 6. Testing: join retry after failure

- Verify: assert_join_retry_works "e2e-err-join-3" "172.20.0.10:51821" "172.20.0.12" "err-join-3"

## Expected results

- join unreachable: no raw OS error
- join no peering: no raw error
- join with state: mentions existing state
- join with state: suggests full command path
- join with state: mentions leave
- join no args: shows usage

## Failure criteria

- join unreachable: raw OS error visible
- join no peering: raw 'early eof' visible
- join with state: unclear
- join with state: no leave suggestion
- join no args: no usage info
