# Test: S3 outage simulation — network partition durability validation

## Objective

Validate that ZeroFS handles S3 outages correctly: short outages cause fsync to block
(no corruption), prolonged outages return EIO, and connectivity restoration triggers
full recovery with no data loss for previously-fsynced data.

This is a GA gate test. It demonstrates the durability invariant: data that has been
fsynced to S3 is never lost, even through network partitions.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- ZeroFS module enabled with a real S3 backend configured
- `iptables` available (requires `NET_ADMIN` capability)
- S3 endpoint IP resolvable and reachable
- Sufficient disk space for local write-back cache

## Steps

### 1. Initialize ZeroFS with real S3 backend

```bash
syfrah fabric init --name test-mesh --node-name s3-outage-node --endpoint 172.20.0.10:51820
syfrah storage volume create --name outage-test-vol --size 1G --backend s3
```

Wait for volume to reach `healthy` state (timeout: 30s).

### 2. Write baseline data and fsync — verify durable

```bash
dd if=/dev/urandom of=/mnt/zerofs/outage-test-vol/baseline.bin bs=1M count=10
sync /mnt/zerofs/outage-test-vol/baseline.bin
md5sum /mnt/zerofs/outage-test-vol/baseline.bin
```

- Verify: `sync` returns 0 (success)
- Verify: Record md5 checksum as `BASELINE_MD5` for later integrity check
- Verify: Volume status remains `healthy`

### 3. Block S3 traffic (network partition)

Resolve the S3 endpoint IP and insert an iptables DROP rule:

```bash
S3_IP=$(dig +short $S3_ENDPOINT_HOST | head -1)
iptables -A OUTPUT -d "$S3_IP" -j DROP
iptables -A INPUT -s "$S3_IP" -j DROP
```

- Verify: `curl -s --connect-timeout 5 https://$S3_ENDPOINT_HOST` times out (confirms partition)

### 4. Test 30-second outage — fsync blocks, no corruption

While S3 is partitioned, write new data and attempt fsync:

```bash
dd if=/dev/urandom of=/mnt/zerofs/outage-test-vol/during-outage.bin bs=1M count=5
sync /mnt/zerofs/outage-test-vol/during-outage.bin &
SYNC_PID=$!
sleep 30
```

- Verify: `sync` process ($SYNC_PID) is still running (blocked, not failed)
- Verify: No kernel oops or filesystem corruption messages in `dmesg`
- Verify: Baseline file still readable: `md5sum /mnt/zerofs/outage-test-vol/baseline.bin` matches `BASELINE_MD5`

### 5. Test 5-minute outage — EIO returned

Continue the partition for a total of 5 minutes from step 3:

```bash
# Wait remaining time to reach 5 minutes total
sleep 270
sync /mnt/zerofs/outage-test-vol/five-min-test.bin
echo $?
```

- Verify: `sync` returns non-zero (EIO or equivalent)
- Verify: Volume status transitions to `degraded` or `unavailable`
- Verify: Previously-fsynced baseline data is still readable from cache

### 6. Restore S3 connectivity

```bash
iptables -D OUTPUT -d "$S3_IP" -j DROP
iptables -D INPUT -s "$S3_IP" -j DROP
```

- Verify: `curl -s --connect-timeout 5 https://$S3_ENDPOINT_HOST` succeeds (connectivity restored)

### 7. Verify recovery — dirty data flushed, volume healthy

Wait for the volume to recover (timeout: 120s):

```bash
syfrah storage volume status outage-test-vol --json
```

- Verify: Volume returns to `healthy` state
- Verify: Dirty/pending write count returns to 0 (all data flushed to S3)
- Verify: No data loss warnings in daemon logs

### 8. Verify pre-outage data integrity

```bash
RECOVERED_MD5=$(md5sum /mnt/zerofs/outage-test-vol/baseline.bin | awk '{print $1}')
```

- Verify: `RECOVERED_MD5` equals `BASELINE_MD5` — data fsynced before the outage is intact
- Verify: `during-outage.bin` is either fully present (flushed on recovery) or absent (never confirmed) — no partial/corrupt files

## Expected results

- Baseline data written and fsynced successfully before outage
- 30s outage: fsync blocks (does not return error), no corruption
- 5min outage: fsync returns EIO, volume marked degraded
- Connectivity restore: volume recovers to healthy, dirty data flushed
- Pre-outage fsynced data is intact (checksum matches)
- No partial or corrupt files on the volume after recovery

## Failure criteria

- fsync returns success during outage (silent data loss)
- fsync returns error before 30s timeout (premature failure)
- Baseline data checksum mismatch after recovery (durability violation)
- Volume does not return to healthy within 120s of connectivity restore
- Partial or corrupt files present after recovery
- Kernel oops or filesystem corruption during partition
- iptables rules not cleaned up (test hygiene failure)
