# Test: Full org hierarchy lifecycle

## Objective

- Create a complete Org -> Project -> Environment hierarchy via CLI
- Verify listing counts at each level
- Verify cascade protection (cannot delete parent with children)
- Verify deletion protection on environments
- Verify full teardown leaves clean state

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- Org CLI commands implemented (`syfrah org`, `syfrah project`, `syfrah env`)

## Steps

### 1. Create the org

```bash
syfrah org create acme
```

- Verify: output contains "acme"

### 2. Create projects under the org

```bash
syfrah project create backend --org acme
syfrah project create frontend --org acme
```

- Verify: each command succeeds

### 3. Create environments under projects

```bash
syfrah env create production --project backend --org acme --deletion-protection
syfrah env create staging --project backend --org acme --ttl 48h
syfrah env create dev --project frontend --org acme --label team=fe
```

- Verify: each command succeeds

### 4. List and verify counts

```bash
syfrah org list
```

- Verify: exactly 1 org (acme)

```bash
syfrah project list --org acme
```

- Verify: exactly 2 projects (backend, frontend)

```bash
syfrah env list --project backend --org acme
```

- Verify: exactly 2 envs (production, staging)

```bash
syfrah env list --project frontend --org acme
```

- Verify: exactly 1 env (dev)

### 5. Try deleting project with children (must fail)

```bash
syfrah project delete backend --org acme
```

- Verify: command fails with error about existing environments

### 6. Delete staging environment

```bash
syfrah env destroy staging --project backend --org acme
```

- Verify: command succeeds

### 7. Try deleting protected environment (must fail)

```bash
syfrah env destroy production --project backend --org acme
```

- Verify: command fails with deletion protection error

### 8. Unprotect and delete production

```bash
syfrah env update production --project backend --org acme --no-deletion-protection
syfrah env destroy production --project backend --org acme
```

- Verify: both commands succeed

### 9. Delete dev environment

```bash
syfrah env destroy dev --project frontend --org acme
```

- Verify: command succeeds

### 10. Delete all projects

```bash
syfrah project delete backend --org acme
syfrah project delete frontend --org acme
```

- Verify: both commands succeed

### 11. Delete the org

```bash
syfrah org delete acme
```

- Verify: command succeeds

### 12. Verify clean state

```bash
syfrah org list
syfrah project list
syfrah env list
```

- Verify: all lists return empty (0 items)
