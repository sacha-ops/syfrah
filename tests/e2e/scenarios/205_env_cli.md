# Test: Environment CLI commands

## Objective

Verify the `syfrah env` CLI commands: create, list, destroy, and extend. Covers duration parsing, label handling, deletion protection, table output, and JSON output.

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

**Expected:** Output confirms environment `staging` was created in project `backend`, org `acme`.

### 3. Create an environment with TTL

```bash
syfrah env create ci-run --project backend --org acme --ttl 2h
```

**Expected:** Environment `ci-run` is created. Output shows TTL of `2h`.

### 4. Create an environment with all flags

```bash
syfrah env create production --project backend --org acme \
  --ttl 7d --deletion-protection \
  --label region=eu-west --label team=payments
```

**Expected:** Environment `production` is created with deletion protection enabled, TTL `7d`, and labels `region=eu-west`, `team=payments`.

### 5. List environments (table output)

```bash
syfrah env list --project backend --org acme
```

**Expected:** Table with columns NAME, PROJECT, TTL, PROTECTED, LABELS, CREATED. Three rows: `staging`, `ci-run`, `production`. The `production` row shows `yes` under PROTECTED and labels.

### 6. List environments (JSON output)

```bash
syfrah env list --project backend --org acme --json
```

**Expected:** Valid JSON array with three environment objects. Each has fields: `id`, `name`, `project_id`, `ttl`, `deletion_protection`, `labels`, `created_at`, `expires_at`.

### 7. Extend environment TTL

```bash
syfrah env extend ci-run --project backend --org acme --ttl 48h
```

**Expected:** Output confirms the TTL was extended to `48h` with a new expiration time.

### 8. Destroy without --yes is rejected

```bash
syfrah env destroy staging --project backend --org acme
```

**Expected:** Error message asking user to re-run with `--yes`. Exit code non-zero.

### 9. Destroy with --yes succeeds

```bash
syfrah env destroy staging --project backend --org acme --yes
```

**Expected:** Output confirms environment `staging` was destroyed.

### 10. Destroy a protected environment is rejected

```bash
syfrah env destroy production --project backend --org acme --yes
```

**Expected:** Error: environment is protected from deletion.

### 11. Destroy a nonexistent environment

```bash
syfrah env destroy nonexistent --project backend --org acme --yes
```

**Expected:** Error: environment not found.

### 12. Duration parsing variants

```bash
syfrah env create test-30m --project backend --org acme --ttl 30m
syfrah env create test-7d --project backend --org acme --ttl 7d
```

**Expected:** Both succeed. `test-30m` has TTL `30m`, `test-7d` has TTL `7d`.

### 13. Invalid duration is rejected

```bash
syfrah env create bad-ttl --project backend --org acme --ttl 10x
```

**Expected:** Error mentioning valid suffixes (m, h, d).

### 14. List with missing flags

```bash
syfrah env list
```

**Expected:** Error asking to specify `--org` and `--project`.

## Teardown

```bash
syfrah env destroy ci-run --project backend --org acme --yes
syfrah env destroy test-30m --project backend --org acme --yes
syfrah env destroy test-7d --project backend --org acme --yes
syfrah project delete backend --org acme
syfrah org delete acme
```
