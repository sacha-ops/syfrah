# Test: Environment deletion protection

## Objective

- Environments with deletion protection enabled cannot be destroyed
- Deletion protection can be toggled on and off
- After disabling protection, the environment can be destroyed
- Error messages are actionable and tell the user exactly what to do

## Prerequisites

- `syfrah` binary installed and in PATH
- No pre-existing org state (clean `~/.syfrah/org.redb`)

## Steps

### 1. Set up org hierarchy

```bash
syfrah org create acme
syfrah project create backend --org acme
```

Expected: both commands succeed.

### 2. Create a protected environment

```bash
syfrah env create production --project backend --org acme --deletion-protection
```

Expected output includes:
- "Environment 'production' created in project 'backend'."
- "Deletion protection: enabled"

### 3. Attempt to destroy the protected environment

```bash
syfrah env destroy production --project backend --org acme
```

Expected: command fails with exit code 1 and error message containing:
- "deletion protection enabled"
- "syfrah env update production --project backend --org acme --no-deletion-protection"

### 4. Disable deletion protection

```bash
syfrah env update production --project backend --org acme --no-deletion-protection
```

Expected output:
- "Environment 'production': deletion protection disabled."

### 5. Destroy the environment

```bash
syfrah env destroy production --project backend --org acme
```

Expected output:
- "Environment 'production' destroyed."

### 6. Verify environment is gone

```bash
syfrah env list --project backend --org acme
```

Expected: "No environments found." or empty list (production should not appear).

### 7. Create environment without protection and destroy it directly

```bash
syfrah env create staging --project backend --org acme
syfrah env destroy staging --project backend --org acme
```

Expected: both commands succeed without any protection error.

### 8. Create environment, enable protection via update, verify it blocks

```bash
syfrah env create canary --project backend --org acme
syfrah env update canary --project backend --org acme --deletion-protection
syfrah env destroy canary --project backend --org acme
```

Expected: the destroy command fails with the same actionable error message.

### 9. Clean up

```bash
syfrah env update canary --project backend --org acme --no-deletion-protection
syfrah env destroy canary --project backend --org acme
syfrah project delete backend --org acme
syfrah org delete acme
```

Expected: all commands succeed.

## Pass criteria

- Protected environments reject deletion with actionable error messages
- `--deletion-protection` flag works on `env create`
- `--deletion-protection` / `--no-deletion-protection` flags work on `env update`
- Unprotected environments can be destroyed immediately
- The full create-protect-unprotect-destroy lifecycle works end to end
