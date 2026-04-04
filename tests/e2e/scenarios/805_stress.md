# 805 — Stress Tests

Validate system resilience under extreme conditions: sustained load, cache pressure, network degradation, and rapid migration. Each test monitors memory, checks for corruption, and produces a PASS/FAIL verdict.

## Prerequisites

- Cluster bootstrapped with Raft leader elected
- At least 2 hypervisors registered
- Default VPC, subnet, and security group configured
- `tc` (iproute2) available on hypervisor nodes for latency injection
- `jq` available for JSON output

## Test 1 — Long-Running Compaction (24h continuous write)

Continuously write key-value entries to the control-plane state store for the configured duration. Periodically verify SST integrity, check for memory leaks via RSS tracking, and confirm cache size stays bounded.

### Setup

```bash
# Start continuous writes at ~100 ops/sec
./805_stress.sh --test compaction --duration 86400
```

### Assertions

1. **No SST corruption** — all keys written are readable after compaction cycles.
2. **No memory leak** — RSS at end is within 2x of RSS at start.
3. **No cache bloat** — cache size never exceeds configured max + 10% headroom.
4. **All compaction cycles complete** without errors in the daemon log.

## Test 2 — Cache Overflow

Write more unique keys than the configured cache capacity. Verify that LRU eviction kicks in, the process does not OOM, and reads of recently-written keys still succeed (from disk if evicted).

### Setup

```bash
# Write 2x cache capacity worth of entries
./805_stress.sh --test cache-overflow --duration 300
```

### Assertions

1. **No OOM** — process remains alive throughout.
2. **LRU eviction observed** — cache hit ratio drops as expected.
3. **Graceful degradation** — read latency increases but all reads succeed.
4. **Memory stays bounded** — RSS never exceeds 2x baseline.

## Test 3 — S3 Latency Spike

Inject 500ms network latency on the path to the S3-compatible storage backend using `tc qdisc`. Verify that fsync latency increases proportionally but no data corruption occurs.

### Setup

```bash
# Inject latency and run write workload
./805_stress.sh --test s3-latency --duration 300
```

### Assertions

1. **Fsync latency increases** — p99 latency rises above 500ms during injection.
2. **No corruption** — all written data is readable and checksums match after latency is removed.
3. **Recovery** — latency returns to baseline within 10s after `tc` rule is removed.
4. **No timeouts** — all operations complete (possibly slowly), none abort.

## Test 4 — Rapid Migration

Reschedule a running VM to a different hypervisor every 60 seconds for the configured duration. Verify that fencing is enforced on every migration, the VM is never running on two nodes simultaneously, and no data is lost.

### Setup

```bash
# Migrate VM every 60s
./805_stress.sh --test rapid-migration --duration 3600
```

### Assertions

1. **Fencing enforced** — every migration log entry shows fence-before-start.
2. **No dual-run** — at no point is the VM reported running on two hypervisors.
3. **No data loss** — a checksum file written before the first migration matches after the last.
4. **All migrations succeed** — no migration left the VM in a stuck state.

## Cleanup

```bash
# The script cleans up automatically; for manual cleanup:
./805_stress.sh --cleanup
```

## Output

Results are saved to `stress_results.json` with per-test PASS/FAIL, timing, and memory stats. The script exits 0 only if all executed tests pass.
