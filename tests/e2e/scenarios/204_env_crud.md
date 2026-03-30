# Test: Environment CRUD operations

## Objective

Verify the full lifecycle of environments: create, get, list, delete, TTL computation, labels, and deletion protection.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running

## Steps

### 1. Create an org and project

```bash
syfrah org create acme
syfrah project create backend --org acme
```

**Expected:** Both commands succeed with no errors.

### 2. Create a basic environment

```bash
syfrah env create staging --project backend --org acme
```

**Expected:** Environment `staging` is created in project `backend`.

### 3. Create an environment with TTL

```bash
syfrah env create ci-run --project backend --org acme --ttl 3600
```

**Expected:** Environment `ci-run` is created. `ttl` is 3600. `expires_at` equals `created_at + 3600`.

### 4. Create an environment with labels

```bash
syfrah env create production --project backend --org acme \
  --label region=eu-west --label team=payments --deletion-protection
```

**Expected:** Environment `production` is created with `deletion_protection: true` and labels `region=eu-west`, `team=payments`.

### 5. List environments for a project

```bash
syfrah env list --project backend --org acme
```

**Expected:** Three environments listed: `staging`, `ci-run`, `production`.

### 6. Reject duplicate environment name

```bash
syfrah env create staging --project backend --org acme
```

**Expected:** Error: environment already exists.

### 7. Delete a non-protected environment

```bash
syfrah env destroy staging --project backend --org acme
```

**Expected:** Environment `staging` is deleted. Subsequent `env list` shows two environments.

### 8. Reject deletion of a protected environment

```bash
syfrah env destroy production --project backend --org acme
```

**Expected:** Error: environment is protected from deletion.

### 9. Verify environment not found

```bash
syfrah env destroy nonexistent --project backend --org acme
```

**Expected:** Error: environment not found.

### 10. Verify project must exist

```bash
syfrah env create test --project nonexistent --org acme
```

**Expected:** Error: project not found.

## Teardown

```bash
syfrah env destroy ci-run --project backend --org acme
syfrah env update production --no-deletion-protection
syfrah env destroy production --project backend --org acme
syfrah project delete backend --org acme
syfrah org delete acme
```
