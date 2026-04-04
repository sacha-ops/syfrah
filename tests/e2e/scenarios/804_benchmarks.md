# Test: Performance benchmarks — fio latency and throughput on ZeroFS NBD device

## Objective

Validate that ZeroFS NBD block devices meet the performance design targets defined in ADR-006 section 19. Uses `fio` to measure latency (p50, p99) and throughput under multiple I/O profiles, then compares results against documented expectations.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running with storage configured
- A ZeroFS-backed volume is created and attached (appears as `/dev/nbd*`)
- The NBD device is formatted with ext4 and mounted at a known path
- `fio` installed (the script installs it if missing)
- Root or sufficient permissions to flush caches (`sync; echo 3 > /proc/sys/vm/drop_caches`)

## ADR-006 section 19 — Design Targets

| Benchmark | Metric | Target |
|-----------|--------|--------|
| Hot read (4K randread, warm cache) | p50 latency | < 10 us |
| Hot read (4K randread, warm cache) | p99 latency | < 50 us |
| Cold read (4K randread, flushed cache) | p50 latency | < 50 ms |
| Buffered write (4K randwrite, no fsync) | p50 latency | < 10 us |
| fsync write (4K randwrite, fsync=1) | p50 latency | < 50 ms |
| Sequential throughput (1M read, QD32) | bandwidth | >= 100 MB/s |
| Sequential throughput (1M write, QD32) | bandwidth | >= 50 MB/s |
| Cache hit rate (90/10 read/write) | hit rate | >= 95% |

## Steps

### 1. Ensure fio is installed

```bash
fio --version || apt-get install -y fio
```

### 2. Hot read benchmark (warm cache)

Pre-warm the cache by reading the test file once, then run 4K random reads.

```bash
fio --name=hot-read --filename=/mnt/zerofs-test/bench.dat \
    --rw=randread --bs=4k --size=256M --numjobs=1 --iodepth=1 \
    --runtime=30 --time_based --output-format=json
```

**Pass criteria:** p50 < 10 us, p99 < 50 us

### 3. Cold read benchmark (flushed cache)

Flush all caches, then run 4K random reads.

```bash
sync; echo 3 > /proc/sys/vm/drop_caches
fio --name=cold-read --filename=/mnt/zerofs-test/bench.dat \
    --rw=randread --bs=4k --size=256M --numjobs=1 --iodepth=1 \
    --runtime=30 --time_based --output-format=json
```

**Pass criteria:** p50 < 50 ms

### 4. Buffered write benchmark (no fsync)

```bash
fio --name=buffered-write --filename=/mnt/zerofs-test/bench.dat \
    --rw=randwrite --bs=4k --size=256M --numjobs=1 --iodepth=1 \
    --runtime=30 --time_based --output-format=json
```

**Pass criteria:** p50 < 10 us

### 5. fsync write benchmark

```bash
fio --name=fsync-write --filename=/mnt/zerofs-test/bench.dat \
    --rw=randwrite --bs=4k --size=256M --numjobs=1 --iodepth=1 \
    --fsync=1 --runtime=30 --time_based --output-format=json
```

**Pass criteria:** p50 < 50 ms

### 6. Sequential throughput (read)

```bash
fio --name=seq-read --filename=/mnt/zerofs-test/bench.dat \
    --rw=read --bs=1M --size=1G --numjobs=1 --iodepth=32 \
    --runtime=30 --time_based --output-format=json
```

**Pass criteria:** bandwidth >= 100 MB/s

### 7. Sequential throughput (write)

```bash
fio --name=seq-write --filename=/mnt/zerofs-test/bench.dat \
    --rw=write --bs=1M --size=1G --numjobs=1 --iodepth=32 \
    --runtime=30 --time_based --output-format=json
```

**Pass criteria:** bandwidth >= 50 MB/s

### 8. Cache hit rate under 90/10 mixed workload

```bash
fio --name=mixed-rw --filename=/mnt/zerofs-test/bench.dat \
    --rw=randrw --rwmixread=90 --bs=4k --size=256M --numjobs=4 --iodepth=16 \
    --runtime=60 --time_based --output-format=json
```

Cache hit rate is derived from ZeroFS metrics (`syfrah storage metrics`), not from fio output directly.

**Pass criteria:** cache hit rate >= 95%

## Expected Results

All benchmarks should produce PASS or WARN. A FAIL on any benchmark indicates a regression or misconfiguration that must be investigated before GA.

Results are saved to `benchmark_results.json` for historical tracking and CI integration.
