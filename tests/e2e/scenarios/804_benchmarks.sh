#!/usr/bin/env bash
# Scenario 804: Performance benchmarks — fio latency + throughput on ZeroFS NBD device
# Validates against ADR-006 §19 design targets.
#
# Usage: ./804_benchmarks.sh [MOUNT_PATH] [RESULT_FILE]
#   MOUNT_PATH   — mount point of the ZeroFS-backed filesystem (default: /mnt/zerofs-test)
#   RESULT_FILE  — path to write JSON results (default: ./benchmark_results.json)

set -euo pipefail

###############################################################################
# Configuration
###############################################################################

MOUNT_PATH="${1:-/mnt/zerofs-test}"
RESULT_FILE="${2:-./benchmark_results.json}"
BENCH_FILE="${MOUNT_PATH}/bench.dat"
FIO_SIZE="256M"
SEQ_SIZE="1G"
RUNTIME=30
MIXED_RUNTIME=60

# ADR-006 §19 design targets (latency in microseconds, bandwidth in KB/s)
TARGET_HOT_READ_P50_US=10
TARGET_HOT_READ_P99_US=50
TARGET_COLD_READ_P50_US=50000       # 50 ms
TARGET_BUFFERED_WRITE_P50_US=10
TARGET_FSYNC_WRITE_P50_US=50000     # 50 ms
TARGET_SEQ_READ_BW_KBS=102400      # 100 MB/s
TARGET_SEQ_WRITE_BW_KBS=51200     # 50 MB/s
TARGET_CACHE_HIT_RATE=95

# WARN thresholds: 2x the PASS target (between PASS and FAIL)
WARN_FACTOR=2

###############################################################################
# Helpers
###############################################################################

PASS_COUNT=0
WARN_COUNT=0
FAIL_COUNT=0
TOTAL_COUNT=0
JSON_ENTRIES=""

red()    { printf '\033[1;31m%s\033[0m' "$1"; }
yellow() { printf '\033[1;33m%s\033[0m' "$1"; }
green()  { printf '\033[1;32m%s\033[0m' "$1"; }

record() {
    local name="$1" metric="$2" value="$3" unit="$4" target="$5" verdict="$6"
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    case "$verdict" in
        PASS) PASS_COUNT=$((PASS_COUNT + 1)); verdict_fmt=$(green "PASS") ;;
        WARN) WARN_COUNT=$((WARN_COUNT + 1)); verdict_fmt=$(yellow "WARN") ;;
        FAIL) FAIL_COUNT=$((FAIL_COUNT + 1)); verdict_fmt=$(red "FAIL")   ;;
    esac
    printf "  %-35s %-12s %12s %-6s  target: %-12s  %s\n" \
        "$name" "$metric" "$value" "$unit" "$target" "$verdict_fmt"

    # Append JSON entry
    local entry
    entry=$(printf '{"name":"%s","metric":"%s","value":%s,"unit":"%s","target":"%s","verdict":"%s"}' \
        "$name" "$metric" "$value" "$unit" "$target" "$verdict")
    if [ -n "$JSON_ENTRIES" ]; then
        JSON_ENTRIES="${JSON_ENTRIES},${entry}"
    else
        JSON_ENTRIES="${entry}"
    fi
}

judge_latency_us() {
    local name="$1" metric="$2" value_ns="$3" target_us="$4"
    local value_us
    # fio JSON reports latency in nanoseconds; convert to microseconds
    value_us=$(echo "$value_ns" | awk '{printf "%.2f", $1 / 1000}')
    local warn_us=$((target_us * WARN_FACTOR))

    local verdict="PASS"
    if awk "BEGIN {exit !($value_us > $warn_us)}" ; then
        verdict="FAIL"
    elif awk "BEGIN {exit !($value_us > $target_us)}" ; then
        verdict="WARN"
    fi

    record "$name" "$metric" "$value_us" "us" "<${target_us}us" "$verdict"
}

judge_bw_kbs() {
    local name="$1" value_kbs="$2" target_kbs="$3"
    local value_mbs
    value_mbs=$(echo "$value_kbs" | awk '{printf "%.1f", $1 / 1024}')
    local target_mbs=$((target_kbs / 1024))
    local warn_kbs=$((target_kbs / WARN_FACTOR))

    local verdict="PASS"
    if awk "BEGIN {exit !($value_kbs < $warn_kbs)}" ; then
        verdict="FAIL"
    elif awk "BEGIN {exit !($value_kbs < $target_kbs)}" ; then
        verdict="WARN"
    fi

    record "$name" "bandwidth" "$value_mbs" "MB/s" ">=${target_mbs}MB/s" "$verdict"
}

extract_lat_p50_ns() {
    # Extract clat percentile 50.000000 in nanoseconds from fio JSON
    echo "$1" | python3 -c "
import sys, json
data = json.load(sys.stdin)
job = data['jobs'][0]
rw = '$2'
clat = job[rw]['clat_ns']['percentile']
# fio uses string keys like '50.000000'
print(clat.get('50.000000', clat.get('50.00000', 0)))
"
}

extract_lat_p99_ns() {
    echo "$1" | python3 -c "
import sys, json
data = json.load(sys.stdin)
job = data['jobs'][0]
rw = '$2'
clat = job[rw]['clat_ns']['percentile']
print(clat.get('99.000000', clat.get('99.00000', 0)))
"
}

extract_bw_kbs() {
    echo "$1" | python3 -c "
import sys, json
data = json.load(sys.stdin)
job = data['jobs'][0]
rw = '$2'
print(int(job[rw]['bw']))
"
}

###############################################################################
# Pre-flight
###############################################################################

echo "================================================================================"
echo "  804 — ZeroFS Performance Benchmarks (fio)"
echo "  ADR-006 §19 design targets"
echo "  $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo "================================================================================"
echo ""

# Install fio if missing
if ! command -v fio &>/dev/null; then
    echo "fio not found — installing..."
    if command -v apt-get &>/dev/null; then
        apt-get update -qq && apt-get install -y -qq fio
    elif command -v dnf &>/dev/null; then
        dnf install -y fio
    elif command -v apk &>/dev/null; then
        apk add --no-cache fio
    else
        echo "ERROR: Cannot install fio — no supported package manager found." >&2
        exit 1
    fi
fi

# Install python3 if missing (needed for JSON parsing)
if ! command -v python3 &>/dev/null; then
    echo "python3 not found — installing..."
    if command -v apt-get &>/dev/null; then
        apt-get update -qq && apt-get install -y -qq python3
    elif command -v dnf &>/dev/null; then
        dnf install -y python3
    elif command -v apk &>/dev/null; then
        apk add --no-cache python3
    fi
fi

# Validate mount path
if [ ! -d "$MOUNT_PATH" ]; then
    echo "ERROR: Mount path $MOUNT_PATH does not exist." >&2
    echo "       Create and mount a ZeroFS volume there first." >&2
    exit 1
fi

echo "Mount path : $MOUNT_PATH"
echo "Result file: $RESULT_FILE"
echo "fio version: $(fio --version)"
echo ""

###############################################################################
# Benchmark 1: Hot read (4K randread, warm cache)
###############################################################################

echo "── Benchmark 1: Hot read (4K randread, warm cache) ──"

# Pre-warm: write test file then read it once to populate cache
fio --name=prewarm-write --filename="$BENCH_FILE" \
    --rw=write --bs=1M --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --output=/dev/null 2>/dev/null
fio --name=prewarm-read --filename="$BENCH_FILE" \
    --rw=read --bs=1M --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --output=/dev/null 2>/dev/null

HOT_READ_JSON=$(fio --name=hot-read --filename="$BENCH_FILE" \
    --rw=randread --bs=4k --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --runtime="$RUNTIME" --time_based --output-format=json 2>/dev/null)

p50_ns=$(extract_lat_p50_ns "$HOT_READ_JSON" "read")
p99_ns=$(extract_lat_p99_ns "$HOT_READ_JSON" "read")

judge_latency_us "Hot read (warm cache)" "p50 lat" "$p50_ns" "$TARGET_HOT_READ_P50_US"
judge_latency_us "Hot read (warm cache)" "p99 lat" "$p99_ns" "$TARGET_HOT_READ_P99_US"
echo ""

###############################################################################
# Benchmark 2: Cold read (4K randread, flushed cache)
###############################################################################

echo "── Benchmark 2: Cold read (4K randread, flushed cache) ──"

sync
echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true

COLD_READ_JSON=$(fio --name=cold-read --filename="$BENCH_FILE" \
    --rw=randread --bs=4k --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --runtime="$RUNTIME" --time_based --output-format=json 2>/dev/null)

p50_ns=$(extract_lat_p50_ns "$COLD_READ_JSON" "read")

judge_latency_us "Cold read (flushed cache)" "p50 lat" "$p50_ns" "$TARGET_COLD_READ_P50_US"
echo ""

###############################################################################
# Benchmark 3: Buffered write (4K randwrite, no fsync)
###############################################################################

echo "── Benchmark 3: Buffered write (4K randwrite, no fsync) ──"

BUFFERED_WRITE_JSON=$(fio --name=buffered-write --filename="$BENCH_FILE" \
    --rw=randwrite --bs=4k --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --runtime="$RUNTIME" --time_based --output-format=json 2>/dev/null)

p50_ns=$(extract_lat_p50_ns "$BUFFERED_WRITE_JSON" "write")

judge_latency_us "Buffered write (no fsync)" "p50 lat" "$p50_ns" "$TARGET_BUFFERED_WRITE_P50_US"
echo ""

###############################################################################
# Benchmark 4: fsync write (4K randwrite, fsync=1)
###############################################################################

echo "── Benchmark 4: fsync write (4K randwrite, fsync=1) ──"

FSYNC_WRITE_JSON=$(fio --name=fsync-write --filename="$BENCH_FILE" \
    --rw=randwrite --bs=4k --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --fsync=1 --runtime="$RUNTIME" --time_based --output-format=json 2>/dev/null)

p50_ns=$(extract_lat_p50_ns "$FSYNC_WRITE_JSON" "write")

judge_latency_us "fsync write" "p50 lat" "$p50_ns" "$TARGET_FSYNC_WRITE_P50_US"
echo ""

###############################################################################
# Benchmark 5: Sequential throughput (1M read/write, QD32)
###############################################################################

echo "── Benchmark 5: Sequential throughput (1M, QD32) ──"

# Sequential read
SEQ_READ_JSON=$(fio --name=seq-read --filename="$BENCH_FILE" \
    --rw=read --bs=1M --size="$SEQ_SIZE" --numjobs=1 --iodepth=32 \
    --runtime="$RUNTIME" --time_based --output-format=json 2>/dev/null)

bw_kbs=$(extract_bw_kbs "$SEQ_READ_JSON" "read")
judge_bw_kbs "Sequential read (1M, QD32)" "$bw_kbs" "$TARGET_SEQ_READ_BW_KBS"

# Sequential write
SEQ_WRITE_JSON=$(fio --name=seq-write --filename="$BENCH_FILE" \
    --rw=write --bs=1M --size="$SEQ_SIZE" --numjobs=1 --iodepth=32 \
    --runtime="$RUNTIME" --time_based --output-format=json 2>/dev/null)

bw_kbs=$(extract_bw_kbs "$SEQ_WRITE_JSON" "write")
judge_bw_kbs "Sequential write (1M, QD32)" "$bw_kbs" "$TARGET_SEQ_WRITE_BW_KBS"
echo ""

###############################################################################
# Benchmark 6: Cache hit rate under 90/10 read/write workload
###############################################################################

echo "── Benchmark 6: Cache hit rate (90/10 mixed r/w) ──"

# Pre-warm the working set
fio --name=prewarm-mixed --filename="$BENCH_FILE" \
    --rw=read --bs=4k --size="$FIO_SIZE" --numjobs=1 --iodepth=1 \
    --output=/dev/null 2>/dev/null

# Run the mixed workload
MIXED_JSON=$(fio --name=mixed-rw --filename="$BENCH_FILE" \
    --rw=randrw --rwmixread=90 --bs=4k --size="$FIO_SIZE" --numjobs=4 --iodepth=16 \
    --runtime="$MIXED_RUNTIME" --time_based --output-format=json 2>/dev/null)

# Attempt to read cache hit rate from syfrah storage metrics.
# If syfrah is not available, derive an estimate from fio clat distribution:
# cache hits have <1ms latency, cache misses have >1ms.
CACHE_HIT_RATE=""
if command -v syfrah &>/dev/null; then
    CACHE_HIT_RATE=$(syfrah storage metrics 2>/dev/null \
        | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data.get('cache_hit_rate', ''))
except:
    print('')
" 2>/dev/null || true)
fi

if [ -z "$CACHE_HIT_RATE" ]; then
    # Estimate from fio: count percentage of IOs with clat < 1ms (1000us = 1000000ns)
    CACHE_HIT_RATE=$(echo "$MIXED_JSON" | python3 -c "
import sys, json
data = json.load(sys.stdin)
job = data['jobs'][0]
# Use read clat percentile distribution
clat = job['read']['clat_ns']['percentile']
# Find highest percentile bucket that is still under 1ms (1000000 ns)
hit_pct = 0
for pct_str in sorted(clat.keys(), key=float):
    if clat[pct_str] <= 1000000:
        hit_pct = float(pct_str)
    else:
        break
print(f'{hit_pct:.1f}')
" 2>/dev/null || echo "N/A")
fi

if [ "$CACHE_HIT_RATE" = "N/A" ] || [ -z "$CACHE_HIT_RATE" ]; then
    record "Cache hit rate (90/10 rw)" "hit rate" "N/A" "%" ">=${TARGET_CACHE_HIT_RATE}%" "WARN"
else
    verdict="PASS"
    warn_threshold=$((TARGET_CACHE_HIT_RATE - 5))
    if awk "BEGIN {exit !($CACHE_HIT_RATE < $warn_threshold)}" ; then
        verdict="FAIL"
    elif awk "BEGIN {exit !($CACHE_HIT_RATE < $TARGET_CACHE_HIT_RATE)}" ; then
        verdict="WARN"
    fi
    record "Cache hit rate (90/10 rw)" "hit rate" "$CACHE_HIT_RATE" "%" ">=${TARGET_CACHE_HIT_RATE}%" "$verdict"
fi
echo ""

###############################################################################
# Summary
###############################################################################

echo "================================================================================"
echo "  RESULTS SUMMARY"
echo "================================================================================"
echo ""
printf "  Total: %d   $(green 'PASS'): %d   $(yellow 'WARN'): %d   $(red 'FAIL'): %d\n" \
    "$TOTAL_COUNT" "$PASS_COUNT" "$WARN_COUNT" "$FAIL_COUNT"
echo ""

# Save results to JSON
TIMESTAMP=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
cat > "$RESULT_FILE" <<ENDJSON
{
  "timestamp": "${TIMESTAMP}",
  "mount_path": "${MOUNT_PATH}",
  "fio_version": "$(fio --version)",
  "runtime_seconds": ${RUNTIME},
  "benchmarks": [${JSON_ENTRIES}],
  "summary": {
    "total": ${TOTAL_COUNT},
    "pass": ${PASS_COUNT},
    "warn": ${WARN_COUNT},
    "fail": ${FAIL_COUNT}
  }
}
ENDJSON

echo "Results saved to: $RESULT_FILE"
echo ""

# Clean up benchmark file
rm -f "$BENCH_FILE"

# Exit code: 0 if no FAILs, 1 otherwise
if [ "$FAIL_COUNT" -gt 0 ]; then
    echo "$(red 'BENCHMARK SUITE: FAIL') — $FAIL_COUNT benchmark(s) below design targets."
    exit 1
elif [ "$WARN_COUNT" -gt 0 ]; then
    echo "$(yellow 'BENCHMARK SUITE: WARN') — $WARN_COUNT benchmark(s) above target but within tolerance."
    exit 0
else
    echo "$(green 'BENCHMARK SUITE: PASS') — all benchmarks within ADR-006 §19 design targets."
    exit 0
fi
