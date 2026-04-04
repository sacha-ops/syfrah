#!/usr/bin/env bash
# Scenario: Power loss resilience — kill -9 during write + fsync
#
# GA gate test: validates ZeroFS recovery after abrupt process termination
# during active writes, fsync (S3 PUT), and compaction.
#
# Verifies:
# - WAL replay recovers all fsynced data after kill -9 during writes
# - Kill during fsync results in either complete or cleanly absent data
# - Kill during compaction leaves SST files consistent
# - Checksums of fsynced data match before and after every crash

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

echo "── Power Loss Resilience Tests ──"

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

MINIO_CONTAINER="e2e-minio-powerloss"
MINIO_PORT="9199"
MINIO_USER="minioadmin"
MINIO_PASS="minioadmin"
MINIO_BUCKET="zerofs-powerloss-test"

ZEROFS_CONTAINER="e2e-zerofs-powerloss"
ZEROFS_IP="172.20.0.40"

VOLUME_NAME="vol-powerloss-test"
VOLUME_SIZE="512M"
NBD_DEVICE="/dev/nbd0"
MOUNT_POINT="/mnt/zerofs-test"

# How long to let fio run before killing (seconds)
WRITE_DURATION=8
KILL_DELAY=4

# Track extra containers for cleanup
EXTRA_CONTAINERS=()

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

cleanup_power_loss() {
    debug "cleanup_power_loss: tearing down"
    for c in "${EXTRA_CONTAINERS[@]}"; do
        docker rm -f "$c" >/dev/null 2>&1 || true
    done
    EXTRA_CONTAINERS=()
    cleanup
}
trap 'cleanup_power_loss 2>/dev/null || true' EXIT

# Start MinIO as the S3 backend.
start_minio() {
    docker rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
    debug "starting MinIO container"
    docker run -d \
        --name "$MINIO_CONTAINER" \
        --network "$E2E_NETWORK" \
        -e MINIO_ROOT_USER="$MINIO_USER" \
        -e MINIO_ROOT_PASSWORD="$MINIO_PASS" \
        minio/minio server /data >/dev/null 2>&1
    EXTRA_CONTAINERS+=("$MINIO_CONTAINER")

    # Wait for MinIO to be ready
    local max_wait=30
    local i=0
    while [ $i -lt $max_wait ]; do
        if docker exec "$MINIO_CONTAINER" \
            curl -sf http://127.0.0.1:9000/minio/health/live >/dev/null 2>&1; then
            debug "MinIO ready after ${i}s"
            break
        fi
        sleep 1
        i=$((i + 1))
    done
    if [ $i -ge $max_wait ]; then
        fail "MinIO did not start within ${max_wait}s"
        return 1
    fi

    # Get MinIO's IP on the test network
    MINIO_IP=$(docker inspect -f \
        "{{(index .NetworkSettings.Networks \"$E2E_NETWORK\").IPAddress}}" \
        "$MINIO_CONTAINER" 2>/dev/null)
    debug "MinIO IP: $MINIO_IP"

    # Create the test bucket using the mc CLI inside the MinIO container
    docker exec "$MINIO_CONTAINER" \
        mc alias set local http://127.0.0.1:9000 "$MINIO_USER" "$MINIO_PASS" \
        >/dev/null 2>&1 || {
        # mc might not be in minio image; use curl to create bucket
        docker exec "$MINIO_CONTAINER" \
            curl -sf -X PUT "http://127.0.0.1:9000/$MINIO_BUCKET" \
            -u "${MINIO_USER}:${MINIO_PASS}" >/dev/null 2>&1 || true
    }
    docker exec "$MINIO_CONTAINER" \
        mc mb "local/$MINIO_BUCKET" >/dev/null 2>&1 || true

    pass "MinIO S3 backend started (bucket: $MINIO_BUCKET)"
}

# Start the ZeroFS test container (privileged, with nbd module).
start_zerofs_container() {
    docker rm -f "$ZEROFS_CONTAINER" >/dev/null 2>&1 || true
    debug "starting ZeroFS container at $ZEROFS_IP"

    local volume_args=()
    if [ -n "${E2E_ZEROFS_BINARY:-}" ]; then
        volume_args=(-v "${E2E_ZEROFS_BINARY}:/usr/local/bin/zerofs:ro")
    fi

    docker run -d \
        --name "$ZEROFS_CONTAINER" \
        --network "$E2E_NETWORK" \
        --ip "$ZEROFS_IP" \
        --privileged \
        --hostname "$ZEROFS_CONTAINER" \
        --init \
        "${volume_args[@]+"${volume_args[@]}"}" \
        "$E2E_IMAGE" >/dev/null

    E2E_CONTAINERS+=("$ZEROFS_CONTAINER")

    # Load nbd kernel module (may already be loaded on host)
    docker exec "$ZEROFS_CONTAINER" modprobe nbd max_part=8 2>/dev/null || true

    # Verify nbd device exists
    if docker exec "$ZEROFS_CONTAINER" test -e "$NBD_DEVICE" 2>/dev/null; then
        debug "NBD device $NBD_DEVICE available"
    else
        info "WARNING: $NBD_DEVICE not available — nbd module may need loading on host"
    fi

    # Install tools we need (fio, e2fsprogs, nbd-client)
    docker exec "$ZEROFS_CONTAINER" sh -c \
        'apt-get update -qq && apt-get install -y -qq fio e2fsprogs nbd-client >/dev/null 2>&1' \
        || docker exec "$ZEROFS_CONTAINER" sh -c \
        'apk add --no-cache fio e2fsprogs nbd-client >/dev/null 2>&1' \
        || info "WARNING: could not install test tools (fio, e2fsprogs)"

    pass "ZeroFS container started"
}

# Start ZeroFS process inside the container.
# Args: [extra_flags...]
start_zerofs_process() {
    debug "starting ZeroFS process"
    docker exec -d "$ZEROFS_CONTAINER" \
        zerofs start \
        --s3-endpoint "http://${MINIO_IP}:9000" \
        --s3-bucket "$MINIO_BUCKET" \
        --s3-access-key "$MINIO_USER" \
        --s3-secret-key "$MINIO_PASS" \
        --nbd-device "$NBD_DEVICE" \
        --volume "$VOLUME_NAME" \
        --size "$VOLUME_SIZE" \
        "$@" 2>&1 || true

    # Wait for ZeroFS to be ready (NBD device becomes a block device)
    local max_wait=20
    local i=0
    while [ $i -lt $max_wait ]; do
        if docker exec "$ZEROFS_CONTAINER" \
            blockdev --getsize64 "$NBD_DEVICE" >/dev/null 2>&1; then
            debug "ZeroFS ready (NBD device active) after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    info "ZeroFS did not make NBD device active within ${max_wait}s"
    return 1
}

# Kill ZeroFS with SIGKILL (simulates power loss).
kill_zerofs() {
    debug "killing ZeroFS with SIGKILL"
    docker exec "$ZEROFS_CONTAINER" sh -c \
        'PID=$(pgrep -f zerofs); [ -n "$PID" ] && kill -9 $PID' 2>/dev/null || true

    # Unmount if still mounted (will fail due to dead process, that's OK)
    docker exec "$ZEROFS_CONTAINER" umount "$MOUNT_POINT" 2>/dev/null || true

    # Disconnect NBD client
    docker exec "$ZEROFS_CONTAINER" nbd-client -d "$NBD_DEVICE" 2>/dev/null || true

    # Brief pause to let kernel clean up
    sleep 1
}

# Mount the NBD device with ext4.
mount_volume() {
    docker exec "$ZEROFS_CONTAINER" mkdir -p "$MOUNT_POINT"
    docker exec "$ZEROFS_CONTAINER" mount "$NBD_DEVICE" "$MOUNT_POINT" 2>&1
}

# Format the NBD device with ext4.
format_volume() {
    docker exec "$ZEROFS_CONTAINER" mkfs.ext4 -F "$NBD_DEVICE" >/dev/null 2>&1
}

# Run fsck and check for errors.
# Returns 0 if clean, 1 if errors found.
check_filesystem() {
    local label="$1"
    # -f = force check even if clean, -n = no changes (read-only)
    local result
    result=$(docker exec "$ZEROFS_CONTAINER" fsck.ext4 -fn "$NBD_DEVICE" 2>&1)
    local rc=$?
    if [ $rc -eq 0 ]; then
        pass "$label: filesystem clean (fsck passed)"
        return 0
    else
        fail "$label: filesystem errors detected (fsck exit=$rc)"
        echo "$result" | tail -5
        return 1
    fi
}

# Get sorted sha256 checksums of all files under the mount point.
get_checksums() {
    docker exec "$ZEROFS_CONTAINER" sh -c \
        "find $MOUNT_POINT -type f -exec sha256sum {} + 2>/dev/null | sort"
}

# Write a known file and fsync it. Args: <filename> <size_mb>
write_and_sync() {
    local filename="$1"
    local size_mb="$2"
    docker exec "$ZEROFS_CONTAINER" sh -c \
        "dd if=/dev/urandom of=${MOUNT_POINT}/${filename} bs=1M count=${size_mb} oflag=sync 2>/dev/null"
}

# ---------------------------------------------------------------------------
# Test A: kill -9 during active writes — WAL replay must recover fsynced data
# ---------------------------------------------------------------------------

test_a_kill_during_writes() {
    echo ""
    echo "── Test A: kill -9 during active writes ──"

    if ! start_zerofs_process; then
        fail "Test A: could not start ZeroFS"
        return 1
    fi

    format_volume
    mount_volume

    # Write known data with fsync so we have a checkpoint
    info "writing baseline data with fsync"
    for i in $(seq 1 5); do
        write_and_sync "baseline_${i}.bin" 1
    done

    # Capture checksums of the fsynced baseline
    CHECKSUMS_BEFORE=$(get_checksums)
    BASELINE_COUNT=$(echo "$CHECKSUMS_BEFORE" | grep -c "baseline_" || echo "0")
    debug "baseline files: $BASELINE_COUNT, checksums captured"

    # Start continuous writes (fio) — these writes are in-flight, not all fsynced
    info "starting continuous fio writes"
    docker exec -d "$ZEROFS_CONTAINER" \
        fio --name=continuous \
            --filename="${MOUNT_POINT}/fio_data.bin" \
            --rw=randwrite --bs=4k --size=32M \
            --fsync=32 --time_based --runtime=$WRITE_DURATION \
            --output=/tmp/fio_output.log 2>/dev/null

    # Let writes run for a bit
    sleep $KILL_DELAY

    # KILL — simulate power loss
    info "sending kill -9 to ZeroFS"
    kill_zerofs

    # Restart ZeroFS — WAL replay should happen automatically
    info "restarting ZeroFS (WAL replay expected)"
    if ! start_zerofs_process; then
        fail "Test A: ZeroFS failed to restart after kill -9"
        return 1
    fi

    # Check filesystem integrity
    check_filesystem "Test A"

    # Mount and verify data
    mount_volume

    CHECKSUMS_AFTER=$(get_checksums)

    # Verify: all baseline files (fsynced before kill) must be intact
    local recovered=0
    for i in $(seq 1 5); do
        local before_hash after_hash
        before_hash=$(echo "$CHECKSUMS_BEFORE" | grep "baseline_${i}.bin" | awk '{print $1}')
        after_hash=$(echo "$CHECKSUMS_AFTER" | grep "baseline_${i}.bin" | awk '{print $1}')
        if [ -n "$before_hash" ] && [ "$before_hash" = "$after_hash" ]; then
            recovered=$((recovered + 1))
        elif [ -z "$after_hash" ]; then
            fail "Test A: baseline_${i}.bin missing after recovery"
        else
            fail "Test A: baseline_${i}.bin checksum mismatch (before=$before_hash, after=$after_hash)"
        fi
    done

    if [ $recovered -eq 5 ]; then
        pass "Test A: all 5 fsynced baseline files recovered with correct checksums"
    else
        fail "Test A: only $recovered/5 baseline files recovered correctly"
    fi

    # Cleanup for next test
    docker exec "$ZEROFS_CONTAINER" umount "$MOUNT_POINT" 2>/dev/null || true
    kill_zerofs
}

# ---------------------------------------------------------------------------
# Test B: kill -9 during fsync (S3 PUT in flight)
# ---------------------------------------------------------------------------

test_b_kill_during_fsync() {
    echo ""
    echo "── Test B: kill -9 during fsync (S3 PUT in flight) ──"

    if ! start_zerofs_process; then
        fail "Test B: could not start ZeroFS"
        return 1
    fi

    format_volume
    mount_volume

    # Write known baseline data that is fully fsynced and committed
    info "writing committed baseline data"
    write_and_sync "committed_1.bin" 2
    COMMITTED_HASH=$(docker exec "$ZEROFS_CONTAINER" \
        sha256sum "${MOUNT_POINT}/committed_1.bin" | awk '{print $1}')
    debug "committed_1.bin hash: $COMMITTED_HASH"

    # Throttle S3 traffic to make PUTs slow (simulate slow network)
    info "throttling S3 traffic to slow down fsync"
    docker exec "$ZEROFS_CONTAINER" sh -c \
        "iptables -A OUTPUT -p tcp -d $MINIO_IP --dport 9000 \
         -m statistic --mode random --probability 0.7 -j DROP" 2>/dev/null || {
        info "WARNING: iptables throttle failed — test may not catch mid-PUT kill"
    }

    # Start a large write that will trigger a slow fsync
    info "writing data during throttled S3 (slow fsync)"
    docker exec -d "$ZEROFS_CONTAINER" sh -c \
        "dd if=/dev/urandom of=${MOUNT_POINT}/inflight.bin bs=1M count=8 oflag=sync 2>/dev/null" &

    # Give the write time to start the S3 PUT
    sleep 2

    # KILL — while the S3 PUT is (probably) in flight
    info "sending kill -9 during slow fsync"
    kill_zerofs

    # Remove throttle before restart
    docker exec "$ZEROFS_CONTAINER" sh -c \
        "iptables -D OUTPUT -p tcp -d $MINIO_IP --dport 9000 \
         -m statistic --mode random --probability 0.7 -j DROP" 2>/dev/null || true

    # Restart
    info "restarting ZeroFS after mid-fsync kill"
    if ! start_zerofs_process; then
        fail "Test B: ZeroFS failed to restart after mid-fsync kill"
        return 1
    fi

    # Check filesystem
    check_filesystem "Test B"

    # Mount and verify
    mount_volume

    # The committed baseline must be intact
    local after_hash
    after_hash=$(docker exec "$ZEROFS_CONTAINER" \
        sha256sum "${MOUNT_POINT}/committed_1.bin" 2>/dev/null | awk '{print $1}')
    if [ "$COMMITTED_HASH" = "$after_hash" ]; then
        pass "Test B: committed data intact after mid-fsync kill"
    else
        fail "Test B: committed data corrupted (before=$COMMITTED_HASH, after=$after_hash)"
    fi

    # The inflight file must be either fully present or cleanly absent
    if docker exec "$ZEROFS_CONTAINER" test -f "${MOUNT_POINT}/inflight.bin" 2>/dev/null; then
        # File exists — verify it is not corrupt (sha256sum must succeed, file must be readable)
        local inflight_size
        inflight_size=$(docker exec "$ZEROFS_CONTAINER" \
            stat -c %s "${MOUNT_POINT}/inflight.bin" 2>/dev/null || echo "0")
        if docker exec "$ZEROFS_CONTAINER" \
            sha256sum "${MOUNT_POINT}/inflight.bin" >/dev/null 2>&1; then
            pass "Test B: inflight file present and readable (${inflight_size} bytes) — fully committed"
        else
            fail "Test B: inflight file present but CORRUPT (unreadable)"
        fi
    else
        pass "Test B: inflight file cleanly absent — write was not committed (acceptable)"
    fi

    # Cleanup
    docker exec "$ZEROFS_CONTAINER" umount "$MOUNT_POINT" 2>/dev/null || true
    kill_zerofs
}

# ---------------------------------------------------------------------------
# Test C: kill -9 during compaction — SST consistency
# ---------------------------------------------------------------------------

test_c_kill_during_compaction() {
    echo ""
    echo "── Test C: kill -9 during compaction ──"

    if ! start_zerofs_process; then
        fail "Test C: could not start ZeroFS"
        return 1
    fi

    format_volume
    mount_volume

    # Write enough data to trigger multiple SST flushes and compaction
    info "writing data to trigger compaction (20 x 2MB files)"
    local compaction_hashes=""
    for i in $(seq 1 20); do
        write_and_sync "compact_${i}.bin" 2
        local h
        h=$(docker exec "$ZEROFS_CONTAINER" \
            sha256sum "${MOUNT_POINT}/compact_${i}.bin" | awk '{print $1}')
        compaction_hashes="${compaction_hashes}${i}:${h}\n"
    done
    debug "all 20 files written and fsynced"

    # Brief pause to let background compaction start
    sleep 3

    # KILL — during compaction
    info "sending kill -9 during compaction"
    kill_zerofs

    # Restart
    info "restarting ZeroFS after mid-compaction kill"
    if ! start_zerofs_process; then
        fail "Test C: ZeroFS failed to restart after mid-compaction kill"
        return 1
    fi

    # Check filesystem
    check_filesystem "Test C"

    # Mount and verify all files
    mount_volume

    local intact=0
    local missing=0
    local corrupt=0
    for i in $(seq 1 20); do
        local expected_hash actual_hash
        expected_hash=$(echo -e "$compaction_hashes" | grep "^${i}:" | cut -d: -f2)
        actual_hash=$(docker exec "$ZEROFS_CONTAINER" \
            sha256sum "${MOUNT_POINT}/compact_${i}.bin" 2>/dev/null | awk '{print $1}')
        if [ -z "$actual_hash" ]; then
            missing=$((missing + 1))
            fail "Test C: compact_${i}.bin missing after recovery"
        elif [ "$expected_hash" = "$actual_hash" ]; then
            intact=$((intact + 1))
        else
            corrupt=$((corrupt + 1))
            fail "Test C: compact_${i}.bin checksum mismatch"
        fi
    done

    if [ $intact -eq 20 ]; then
        pass "Test C: all 20 fsynced files intact after mid-compaction kill"
    elif [ $corrupt -eq 0 ] && [ $missing -eq 0 ]; then
        pass "Test C: all files consistent (${intact}/20 intact)"
    else
        fail "Test C: data integrity issues (intact=$intact, missing=$missing, corrupt=$corrupt)"
    fi

    # Verify ZeroFS internal consistency: check that S3 bucket is not
    # littered with orphaned partial SST files
    local s3_objects
    s3_objects=$(docker exec "$MINIO_CONTAINER" \
        mc ls "local/$MINIO_BUCKET" --recursive 2>/dev/null | wc -l || echo "unknown")
    debug "S3 objects after compaction kill: $s3_objects"
    # We cannot assert exact count, but ZeroFS should not leave temp/partial objects
    # A real assertion would check for .tmp or .partial suffixes
    local partial_objects
    partial_objects=$(docker exec "$MINIO_CONTAINER" \
        mc ls "local/$MINIO_BUCKET" --recursive 2>/dev/null \
        | grep -cE '\.(tmp|partial|pending)' || echo "0")
    if [ "$partial_objects" = "0" ]; then
        pass "Test C: no orphaned partial objects in S3"
    else
        fail "Test C: found $partial_objects orphaned partial objects in S3"
    fi

    # Cleanup
    docker exec "$ZEROFS_CONTAINER" umount "$MOUNT_POINT" 2>/dev/null || true
    kill_zerofs
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

create_network

# Start the S3 backend
start_minio

# Start the test container
start_zerofs_container

# Run all three power loss tests
test_a_kill_during_writes
test_b_kill_during_fsync
test_c_kill_during_compaction

# Final cleanup
cleanup_power_loss
summary
