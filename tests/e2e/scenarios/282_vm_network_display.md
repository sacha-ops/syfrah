# 282 — VM network info in list/get output

## Goal

Verify that `vm list` and `vm get` display network information (IP, subnet, VPC)
when present, and gracefully show placeholders when absent.

## Preconditions

- Daemon is running (`syfrah fabric init --name test-mesh`)
- At least one VM exists (networking fields may be empty until overlay integration)

## Steps

### 1. List VMs — IP column present

```bash
syfrah compute vm list
```

**Expected**: Output table includes an `IP` column header between `PHASE` and `RUNTIME`.

### 2. Get VM details — Subnet field present

```bash
syfrah compute vm get <vm-name>
```

**Expected**: Detail output includes `IP:`, `Subnet:`, and `VPC:` lines. When no
network is configured the values show `-`.

### 3. JSON output includes network fields

```bash
syfrah compute vm get <vm-name> --json
```

**Expected**: JSON output contains `"ip"`, `"subnet"`, and `"vpc"` keys. Values are
`null` when no network is configured.

### 4. List JSON includes network fields

```bash
syfrah compute vm list --json
```

**Expected**: Each VM object in the JSON array contains `"ip"`, `"subnet"`, and
`"vpc"` keys.

## Teardown

None required — read-only validation of display output.
