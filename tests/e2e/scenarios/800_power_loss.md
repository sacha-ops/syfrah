# Test: Power loss resilience — kill -9 during write + fsync

## Objective

Validate that ZeroFS recovers correctly after abrupt process termination
(simulating power loss) during active writes, fsync (S3 PUT), and compaction.
This is a GA gate test.

## Prerequisites

- ZeroFS binary available (either in $PATH or via E2E_ZEROFS_BINARY)
- S3-compatible backend (MinIO container or real S3 endpoint)
- NBD kernel module loaded (`modprobe nbd`)
- `fio`, `sha256sum`, and `nbd-client` available on the test host
- Privileged Docker containers (for /dev/nbd* access)

## Steps

### 1. Start MinIO as S3 backend

```bash
docker run -d --name e2e-minio \
  -p 9000:9000 \
  -e MINIO_ROOT_USER=minioadmin \
  -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio server /data

# Create the test bucket
mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
mc mb local/zerofs-test
```

### 2. Start ZeroFS with NBD

```bash
zerofs start \
  --s3-endpoint http://127.0.0.1:9000 \
  --s3-bucket zerofs-test \
  --s3-access-key minioadmin \
  --s3-secret-key minioadmin \
  --nbd-device /dev/nbd0 \
  --volume vol-test \
  --size 1G
```

Mount and format:
```bash
mkfs.ext4 /dev/nbd0
mkdir -p /mnt/test
mount /dev/nbd0 /mnt/test
```

### 3. Test A: kill -9 during active writes

Write continuously with fio, then kill ZeroFS mid-write:

```bash
# Start continuous writes with fsync
fio --name=continuous --filename=/mnt/test/data.bin \
    --rw=randwrite --bs=4k --size=64M \
    --fsync=16 --time_based --runtime=30 &
FIO_PID=$!

# Let writes accumulate for a few seconds
sleep 5

# Capture checksums of all files that were fully fsynced
sync
CHECKSUM_BEFORE=$(find /mnt/test -type f -exec sha256sum {} + | sort)

# Kill ZeroFS process abruptly
kill -9 $(pgrep -f zerofs)
```

Verify recovery:
```bash
# Restart ZeroFS — WAL replay should recover fsynced data
zerofs start --s3-endpoint ... --nbd-device /dev/nbd0 --volume vol-test

mount /dev/nbd0 /mnt/test
CHECKSUM_AFTER=$(find /mnt/test -type f -exec sha256sum {} + | sort)

# All data that was fsynced before the kill must be intact
# Data written after last fsync may or may not be present (both acceptable)
```

### 4. Test B: kill -9 during fsync (S3 PUT in flight)

Inject a delay on S3 PUTs (via MinIO throttling or iptables), then kill
during the slow fsync:

```bash
# Throttle S3 to make PUTs slow
iptables -A OUTPUT -p tcp --dport 9000 -m statistic \
    --mode random --probability 0.5 -j DROP

# Write and fsync
dd if=/dev/urandom of=/mnt/test/critical.bin bs=1M count=4 oflag=sync

# Kill during the slow PUT
kill -9 $(pgrep -f zerofs)

# Remove throttle
iptables -D OUTPUT -p tcp --dport 9000 -m statistic \
    --mode random --probability 0.5 -j DROP
```

Verify: after restart, either the data is fully present and correct,
or the write is cleanly absent. No partial/corrupt data.

### 5. Test C: kill -9 during compaction

Trigger compaction by writing enough data to fill multiple SSTs, then kill
during the background compaction:

```bash
# Write enough data to trigger compaction (multiple SST flushes)
for i in $(seq 1 20); do
    dd if=/dev/urandom of=/mnt/test/file_${i}.bin bs=1M count=2 oflag=sync
done

# Wait briefly for compaction to start
sleep 2

# Kill during compaction
kill -9 $(pgrep -f zerofs)
```

Verify: after restart, all fsynced files are intact. SST metadata is
consistent (no orphaned or partially-written SSTs).

### 6. After each kill: verify data integrity

For every restart:
```bash
# 1. ZeroFS starts without error (WAL replay succeeds)
# 2. Mount succeeds
# 3. fsck reports no corruption
fsck.ext4 -fn /dev/nbd0

# 4. All files that were fsynced before the kill are intact
# 5. Checksums match for fsynced data
```

## Expected results

- **Test A**: All data fsynced before kill -9 is recovered via WAL replay.
  Un-fsynced data may be lost (acceptable).
- **Test B**: After restart, inflight data is either fully committed or
  cleanly absent. No partial chunks, no corrupt blocks.
- **Test C**: After restart, SST files are consistent. No orphaned SSTs,
  no missing data from completed compaction cycles.
- All filesystem checks (fsck) pass with no errors after every restart.
- Checksums of fsynced data match before and after each crash.

## Failure criteria

- ZeroFS fails to start after crash (WAL replay error)
- fsck reports corruption after restart
- Fsynced data is missing or has different checksum after recovery
- Partial/corrupt data present (neither complete nor cleanly absent)
- ZeroFS panics or hangs during WAL replay
- NBD device fails to reconnect after restart
