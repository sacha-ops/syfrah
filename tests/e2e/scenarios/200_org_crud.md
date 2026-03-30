# Test: Org CRUD operations

## Objective

- Create, list, get, and delete organizations via the CLI
- Validate that name constraints are enforced
- Verify persistence across operations

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Clean state (no existing orgs)

## Steps

### 1. Create an organization

```bash
syfrah org create acme
```

Expected output:
```
Organization created: acme
```

### 2. Create a second organization

```bash
syfrah org create beta-corp
```

Expected output:
```
Organization created: beta-corp
```

### 3. List organizations

```bash
syfrah org list
```

Expected: both `acme` and `beta-corp` appear in the output.

### 4. Reject duplicate name

```bash
syfrah org create acme
```

Expected: error indicating the org already exists. Exit code non-zero.

### 5. Reject invalid names

```bash
syfrah org create "My Org"
syfrah org create AB
syfrah org create ORG
```

Expected: each command returns an error about an invalid org name. Exit code non-zero.

### 6. Delete an organization

```bash
syfrah org delete acme
```

Expected output confirms deletion. The org no longer appears in `syfrah org list`.

### 7. Delete non-existent organization

```bash
syfrah org delete acme
```

Expected: error indicating org not found. Exit code non-zero.

## Pass criteria

- All create/list/delete operations succeed as described
- Invalid and duplicate names are rejected with clear error messages
- Deleted orgs do not appear in subsequent list calls
