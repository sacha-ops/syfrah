# Test: --region and --zone filters on topology command

## Objective

--region and --zone filters on topology command.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 3 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric topology --region eu-west
```

```bash
syfrah fabric topology --zone eu-west-1a
```

```bash
syfrah fabric topology --region nonexistent
```

```bash
syfrah fabric topology --zone nonexistent
```


## Expected results

- region filter shows eu-west
- region filter hides us-east
- region filter shows node-eu
- zone filter shows eu-west-1a
- zone filter shows node-eu in eu-west-1a
- zone filter shows node-eu
- invalid region filter gives helpful error
- invalid zone filter gives helpful error

## Failure criteria

- region filter missing eu-west
- region filter should hide us-east
- region filter missing node-eu
- zone filter missing eu-west-1a
- zone filter missing node-eu
- invalid region filter: unhelpful message
- invalid zone filter: unhelpful message
