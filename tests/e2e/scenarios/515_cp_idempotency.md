# 515 — Control Plane: Idempotency journal — request deduplication

## Goal
Verify that the state machine deduplicates commands with the same idempotency key.

## Steps

### 1. Submit a command with an idempotency key
```bash
syfrah org create test-idem --idempotency-key "create-test-idem-001"
# Should succeed: org "test-idem" created
```

### 2. Retry the same command with the same key
```bash
syfrah org create test-idem --idempotency-key "create-test-idem-001"
# Should return the cached result (same Created response), NOT AlreadyExists
```

### 3. Same key, different payload → 409 Conflict
```bash
syfrah org create different-org --idempotency-key "create-test-idem-001"
# Should return 409 Conflict (same key but different payload fingerprint)
```

### 4. Expired key is treated as new
```bash
# After 24h, the key expires and the command is treated as new
```

## Expected Outcome
- Same key + same payload → cached result returned (no re-execution)
- Same key + different payload → 409 Conflict
- Expired keys → treated as new commands
- GC runs during snapshots, removing entries older than 24h
