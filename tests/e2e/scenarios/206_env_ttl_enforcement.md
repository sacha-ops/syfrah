# 206: Environment TTL Enforcement

## Purpose
Verify that environments with expired TTLs are automatically detected and destroyed,
that non-expired environments are left untouched, and that `syfrah env extend` correctly
resets the expiration time.

## Preconditions
- Syfrah daemon is running
- An organization and project exist

## Steps

### 1. Create an environment with a short TTL
```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create ci-run --project backend --org acme --ttl 5s
```
**Expected:** Environment `ci-run` is created with `expires_at` set to `now + 5s`.

### 2. Verify environment exists
```bash
syfrah env list --project backend --org acme
```
**Expected:** `ci-run` appears in the list.

### 3. Wait for TTL expiration
Wait at least 65 seconds (one sweep interval of 60s plus the 5s TTL).

### 4. Verify environment was destroyed
```bash
syfrah env list --project backend --org acme
```
**Expected:** `ci-run` no longer appears. Daemon logs show destruction message.

### 5. Create an environment with a longer TTL
```bash
syfrah env create staging --project backend --org acme --ttl 24h
```
**Expected:** Environment `staging` is created.

### 6. Verify non-expired environment is not destroyed
Wait 65 seconds (one sweep interval).
```bash
syfrah env list --project backend --org acme
```
**Expected:** `staging` still exists (TTL has not expired).

### 7. Extend the environment TTL
```bash
syfrah env extend staging --project backend --org acme --ttl 24h
```
**Expected:** Output confirms extension. `expires_at` is updated to `old_expires_at + 24h`.

### 8. Verify extended environment persists
```bash
syfrah env list --project backend --org acme
```
**Expected:** `staging` still exists with updated expiration.

### 9. Deletion protection blocks TTL destruction
```bash
syfrah env create protected-env --project backend --org acme --ttl 5s --deletion-protection
```
Wait 65 seconds.
```bash
syfrah env list --project backend --org acme
```
**Expected:** `protected-env` still exists despite expired TTL. Daemon logs show a warning
that deletion was skipped due to `deletion_protection`.

## Teardown
```bash
syfrah env destroy staging --project backend --org acme
syfrah env destroy protected-env --project backend --org acme --force
syfrah project delete backend --org acme
syfrah org delete acme
```
