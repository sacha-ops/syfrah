# Test: topology --json output schema validation

## Objective

topology --json output schema validation.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Execute the test

```bash
syfrah fabric topology --json
```

```bash
syfrah fabric topology --json --region eu-west
```


## Expected results

- topology --json produces valid JSON
- JSON has mesh_name field
- JSON has total_nodes field
- JSON has regions array
- mesh_name is correct: json-mesh
- total_nodes >= 2
- regions array has <value> entries
- region has name field
- region has zones array with <value> entries
- zone has name field
- zone has nodes array with <value> entries
- node has name field
- node has mesh_ipv6 field
- node has valid status
- topology --json --region produces valid JSON

## Failure criteria

- topology --json is not valid JSON
- JSON missing mesh_name field
- JSON missing total_nodes field
- JSON missing regions array
- mesh_name incorrect
- total_nodes unexpected
- regions array empty
- region missing name field
- region zones array empty
- zone missing name field
- zone nodes array empty
- node missing name field
- node missing mesh_ipv6 field
- node has unexpected status
- topology --json --region is not valid JSON
