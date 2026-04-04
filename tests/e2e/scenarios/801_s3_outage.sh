#!/usr/bin/env bash
# Scenario 801: S3 outage simulation — network partition durability validation
#
# GA gate test. Demonstrates the durability invariant:
#   - Data fsynced to S3 before an outage is never lost
#   - Short outage (30s): fsync blocks, no corruption
#   - Long outage (5min): EIO returned, volume degrades
#   - Recovery: dirty data flushed, volume returns to healthy
#
# Requires: NET_ADMIN capability, real S3 backend, iptables

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

echo "── S3 Outage Simulation (Network Partition) ──"
trap cleanup_s3_outage EXIT

# ── Configuration ────────────────────────────────────────────

S3_ENDPOINT_HOST="${S3_ENDPOINT_HOST:-s3.amazonaws.com}"
VOLUME_NAME="outage-test-vol"
MOUNT_PATH="/mnt/zerofs/${VOLUME_NAME}"
NODE_NAME="e2e-s3-outage"
NODE_IP="172.20.0.10"
SHORT_OUTAGE_SECS=30
LONG_OUTAGE_SECS=300
RECOVERY_TIMEOUT=120
S3_IP=""

# ── Helpers ──────────────────────────────────────────────────

resolve_s3_ip() {
    S3_IP=$(docker exec "$NODE_NAME" dig +short "$S3_ENDPOINT_HOST" 2>/dev/null | grep -E '^[0-9]+\.' | head -1)
    if [ -z "$S3_IP" ]; then
        # Fallback: try getent
        S3_IP=$(docker exec "$NODE_NAME" getent ahosts "$S3_ENDPOINT_HOST" 2>/dev/null | awk '{print $1}' | grep -E '^[0-9]+\.' | head -1)
    fi
    if [ -z "$S3_IP" ]; then
        fail "could not resolve S3 endpoint IP for $S3_ENDPOINT_HOST"
        exit 1
    fi
    info "Resolved S3 endpoint: $S3_ENDPOINT_HOST -> $S3_IP"
}

block_s3() {
    info "Blocking S3 traffic to $S3_IP..."
    docker exec "$NODE_NAME" iptables -A OUTPUT -d "$S3_IP" -j DROP
    docker exec "$NODE_NAME" iptables -A INPUT -s "$S3_IP" -j DROP

    # Verify the partition is effective
    if docker exec "$NODE_NAME" curl -s --connect-timeout 5 "https://${S3_ENDPOINT_HOST}" >/dev/null 2>&1; then
        fail "S3 still reachable after iptables block"
        exit 1
    fi
    pass "S3 traffic blocked (network partition active)"
}

unblock_s3() {
    info "Restoring S3 connectivity..."
    docker exec "$NODE_NAME" iptables -D OUTPUT -d "$S3_IP" -j DROP 2>/dev/null || true
    docker exec "$NODE_NAME" iptables -D INPUT -s "$S3_IP" -j DROP 2>/dev/null || true
}

verify_s3_reachable() {
    local retries=10
    for i in $(seq 1 $retries); do
        if docker exec "$NODE_NAME" curl -s --connect-timeout 5 "https://${S3_ENDPOINT_HOST}" >/dev/null 2>&1; then
            pass "S3 connectivity restored"
            return 0
        fi
        sleep 2
    done
    fail "S3 not reachable after unblock (waited $((retries * 2))s)"
    return 1
}

wait_volume_status() {
    local target_status="$1"
    local timeout="$2"
    local elapsed=0
    info "Waiting for volume '$VOLUME_NAME' to reach status '$target_status' (timeout: ${timeout}s)..."
    while [ $elapsed -lt "$timeout" ]; do
        local status
        status=$(docker exec "$NODE_NAME" syfrah storage volume status "$VOLUME_NAME" --json 2>/dev/null \
            | jq -r '.status // .state // empty' 2>/dev/null)
        if [ "$status" = "$target_status" ]; then
            pass "volume reached '$target_status' in ${elapsed}s"
            return 0
        fi
        sleep 2
        elapsed=$((elapsed + 2))
    done
    fail "volume did not reach '$target_status' within ${timeout}s (last status: ${status:-unknown})"
    return 1
}

get_dirty_count() {
    docker exec "$NODE_NAME" syfrah storage volume status "$VOLUME_NAME" --json 2>/dev/null \
        | jq -r '.dirty_blocks // .pending_writes // 0' 2>/dev/null
}

cleanup_s3_outage() {
    info "Cleaning up S3 outage test..."
    # Always remove iptables rules first — this is critical
    if [ -n "$S3_IP" ]; then
        docker exec "$NODE_NAME" iptables -D OUTPUT -d "$S3_IP" -j DROP 2>/dev/null || true
        docker exec "$NODE_NAME" iptables -D INPUT -s "$S3_IP" -j DROP 2>/dev/null || true
    fi
    # Standard cleanup
    cleanup
}

# ── Test execution ───────────────────────────────────────────

create_network
start_node "$NODE_NAME" "$NODE_IP"

# Step 1: Initialize ZeroFS with S3 backend
info "Step 1: Initializing ZeroFS with real S3 backend..."
init_mesh "$NODE_NAME" "$NODE_IP" "s3-outage-node"

docker exec "$NODE_NAME" syfrah storage volume create \
    --name "$VOLUME_NAME" --size 1G --backend s3 2>&1
wait_volume_status "healthy" 30

resolve_s3_ip

# Step 2: Write baseline data and fsync
info "Step 2: Writing baseline data and verifying durability..."
docker exec "$NODE_NAME" dd if=/dev/urandom of="${MOUNT_PATH}/baseline.bin" bs=1M count=10 2>/dev/null
SYNC_EXIT=0
docker exec "$NODE_NAME" sync "${MOUNT_PATH}/baseline.bin" || SYNC_EXIT=$?
if [ "$SYNC_EXIT" -eq 0 ]; then
    pass "baseline data fsynced successfully"
else
    fail "baseline fsync failed with exit code $SYNC_EXIT"
    cleanup_s3_outage
    exit 1
fi

BASELINE_MD5=$(docker exec "$NODE_NAME" md5sum "${MOUNT_PATH}/baseline.bin" | awk '{print $1}')
info "Baseline MD5: $BASELINE_MD5"

if [ -n "$BASELINE_MD5" ]; then
    pass "baseline checksum recorded"
else
    fail "could not compute baseline checksum"
    cleanup_s3_outage
    exit 1
fi

# Step 3: Block S3 traffic
info "Step 3: Creating network partition (blocking S3)..."
block_s3

# Step 4: Test 30-second outage — fsync should block, no corruption
info "Step 4: Testing 30s outage — fsync should block, not fail..."
docker exec "$NODE_NAME" dd if=/dev/urandom of="${MOUNT_PATH}/during-outage.bin" bs=1M count=5 2>/dev/null

# Start fsync in background — it should block, not return
docker exec "$NODE_NAME" bash -c "sync '${MOUNT_PATH}/during-outage.bin'; echo \$? > /tmp/sync_exit" &
SYNC_BG_PID=$!

sleep "$SHORT_OUTAGE_SECS"

# Check that the sync process is still running (blocked)
if kill -0 "$SYNC_BG_PID" 2>/dev/null; then
    pass "fsync blocked during 30s outage (did not return prematurely)"
else
    # Sync completed — check if it errored or silently succeeded
    wait "$SYNC_BG_PID" 2>/dev/null || true
    SYNC_RESULT=$(docker exec "$NODE_NAME" cat /tmp/sync_exit 2>/dev/null || echo "unknown")
    if [ "$SYNC_RESULT" = "0" ]; then
        fail "fsync returned success during outage (possible silent data loss)"
    else
        fail "fsync returned error ($SYNC_RESULT) before 30s timeout (premature failure)"
    fi
fi

# Verify no corruption — baseline file still readable
DURING_MD5=$(docker exec "$NODE_NAME" md5sum "${MOUNT_PATH}/baseline.bin" 2>/dev/null | awk '{print $1}')
if [ "$DURING_MD5" = "$BASELINE_MD5" ]; then
    pass "baseline data intact during outage (checksum match)"
else
    fail "baseline data corrupted during outage (expected $BASELINE_MD5, got $DURING_MD5)"
fi

# Check dmesg for corruption signals
CORRUPTION=$(docker exec "$NODE_NAME" dmesg 2>/dev/null | grep -iE 'oops|corrupt|panic|zerofs.*error' | head -5)
if [ -z "$CORRUPTION" ]; then
    pass "no kernel oops or filesystem corruption during 30s outage"
else
    fail "kernel/filesystem issues detected: $CORRUPTION"
fi

# Step 5: Test 5-minute outage — EIO expected
info "Step 5: Extending outage to 5 minutes — expecting EIO..."
REMAINING=$((LONG_OUTAGE_SECS - SHORT_OUTAGE_SECS))
sleep "$REMAINING"

# Kill the blocked sync if still running
kill "$SYNC_BG_PID" 2>/dev/null || true
wait "$SYNC_BG_PID" 2>/dev/null || true

# Attempt a new sync — should fail with EIO
docker exec "$NODE_NAME" dd if=/dev/urandom of="${MOUNT_PATH}/five-min-test.bin" bs=1M count=1 2>/dev/null || true
LONG_SYNC_EXIT=0
docker exec "$NODE_NAME" sync "${MOUNT_PATH}/five-min-test.bin" 2>/dev/null || LONG_SYNC_EXIT=$?

if [ "$LONG_SYNC_EXIT" -ne 0 ]; then
    pass "fsync returned error after 5min outage (exit code: $LONG_SYNC_EXIT)"
else
    fail "fsync returned success after 5min outage (expected EIO)"
fi

# Check volume status — should be degraded or unavailable
VOL_STATUS=$(docker exec "$NODE_NAME" syfrah storage volume status "$VOLUME_NAME" --json 2>/dev/null \
    | jq -r '.status // .state // empty' 2>/dev/null)
if echo "$VOL_STATUS" | grep -qiE 'degraded|unavailable|error'; then
    pass "volume status is '$VOL_STATUS' after prolonged outage"
else
    fail "volume status is '$VOL_STATUS' — expected degraded/unavailable"
fi

# Baseline still readable from cache
CACHE_MD5=$(docker exec "$NODE_NAME" md5sum "${MOUNT_PATH}/baseline.bin" 2>/dev/null | awk '{print $1}')
if [ "$CACHE_MD5" = "$BASELINE_MD5" ]; then
    pass "baseline data still readable from cache during prolonged outage"
else
    fail "baseline data not readable from cache (expected $BASELINE_MD5, got ${CACHE_MD5:-empty})"
fi

# Step 6: Restore connectivity
info "Step 6: Restoring S3 connectivity..."
unblock_s3
verify_s3_reachable

# Step 7: Verify recovery
info "Step 7: Verifying recovery — dirty data flushed, volume healthy..."
wait_volume_status "healthy" "$RECOVERY_TIMEOUT"

DIRTY=$(get_dirty_count)
if [ "$DIRTY" = "0" ] || [ -z "$DIRTY" ]; then
    pass "no dirty/pending writes remaining (all data flushed)"
else
    fail "dirty write count is $DIRTY — expected 0"
fi

# Check daemon logs for data loss warnings
LOSS_WARNINGS=$(docker exec "$NODE_NAME" journalctl -u syfrah 2>/dev/null \
    | grep -iE 'data.loss|corruption|unrecoverable' | head -5)
if [ -z "$LOSS_WARNINGS" ]; then
    pass "no data loss warnings in daemon logs"
else
    fail "data loss warnings found: $LOSS_WARNINGS"
fi

# Step 8: Verify pre-outage data integrity
info "Step 8: Verifying pre-outage data integrity..."
RECOVERED_MD5=$(docker exec "$NODE_NAME" md5sum "${MOUNT_PATH}/baseline.bin" | awk '{print $1}')
if [ "$RECOVERED_MD5" = "$BASELINE_MD5" ]; then
    pass "DURABILITY INVARIANT HOLDS: pre-outage fsynced data intact ($RECOVERED_MD5)"
else
    fail "DURABILITY VIOLATION: baseline checksum mismatch (expected $BASELINE_MD5, got $RECOVERED_MD5)"
fi

# Verify no partial/corrupt files
OUTAGE_FILE_SIZE=$(docker exec "$NODE_NAME" stat -c%s "${MOUNT_PATH}/during-outage.bin" 2>/dev/null || echo "0")
if [ "$OUTAGE_FILE_SIZE" = "5242880" ] || [ "$OUTAGE_FILE_SIZE" = "0" ]; then
    # Either fully flushed (5MB) or absent — both acceptable
    pass "during-outage file is clean: size=${OUTAGE_FILE_SIZE} (fully flushed or absent)"
else
    fail "during-outage file may be partial/corrupt: size=${OUTAGE_FILE_SIZE} (expected 5242880 or 0)"
fi

# Verify iptables rules cleaned up (defense in depth — cleanup trap should handle this)
REMAINING_RULES=$(docker exec "$NODE_NAME" iptables -L OUTPUT -n 2>/dev/null | grep -c "$S3_IP" || echo "0")
if [ "$REMAINING_RULES" = "0" ]; then
    pass "iptables rules cleaned up"
else
    fail "iptables rules still present after test — cleaning up"
    unblock_s3
fi

cleanup_s3_outage
summary
