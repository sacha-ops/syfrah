# Test: Structured JSON logging

## Objective

Verify all Forge operations are logged as structured JSON with the required fields.

## Steps

### 1. Check daemon logs for JSON format

```bash
journalctl -u syfrah --no-pager -n 20 --output=cat 2>/dev/null || echo "Check ~/.syfrah/daemon.log"
```

**Expected:** JSON-formatted log entries with fields: timestamp, level, message, and operation-specific fields.

### 2. Unit test verification

```bash
cargo test -p syfrah-forge logging 2>&1
```

**Expected:** All logging tests pass.

## Pass criteria

- ForgeLogEntry struct with all required fields
- OperationTimer for tracking operation duration
- log_reconciliation helper for reconcile cycles
- init_json_logging configures tracing-subscriber with JSON formatter
- Tests verify serialization and timer functionality
