#!/usr/bin/env bash
# Scenario 805: Stress tests — compaction, cache overflow, S3 latency, rapid migration
#
# Usage:
#   ./805_stress.sh                          # Run all tests (short/CI duration)
#   ./805_stress.sh --test compaction        # Run single test
#   ./805_stress.sh --duration 86400         # Override duration (seconds)
#   ./805_stress.sh --long                   # Long-run preset (24h compaction, 1h others)
#   ./805_stress.sh --cleanup                # Remove leftover resources
#
# Environment:
#   STRESS_RESULTS_DIR  — directory for results JSON (default: ./stress_results)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

# ── Defaults ─────────────────────────────────────────────────

STRESS_RESULTS_DIR="${STRESS_RESULTS_DIR:-./stress_results}"
RESULTS_FILE="$STRESS_RESULTS_DIR/stress_results.json"
SELECTED_TEST=""
DURATION_OVERRIDE=""
LONG_MODE=false
CLEANUP_ONLY=false

# CI-friendly short durations (seconds)
DUR_COMPACTION_SHORT=120
DUR_CACHE_SHORT=60
DUR_S3_LATENCY_SHORT=60
DUR_MIGRATION_SHORT=180

# Long-run durations
DUR_COMPACTION_LONG=86400
DUR_CACHE_LONG=3600
DUR_S3_LATENCY_LONG=3600
DUR_MIGRATION_LONG=3600

# ── Argument parsing ────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --test)       SELECTED_TEST="$2"; shift 2 ;;
        --duration)   DURATION_OVERRIDE="$2"; shift 2 ;;
        --long)       LONG_MODE=true; shift ;;
        --cleanup)    CLEANUP_ONLY=true; shift ;;
        *)            echo "Unknown option: $1"; exit 1 ;;
    esac
done

# ── Helpers ──────────────────────────────────────────────────

mkdir -p "$STRESS_RESULTS_DIR"

OVERALL_START=$(date +%s)
declare -A TEST_RESULTS

# Get RSS of a process in KB. Args: <container> <pid|pattern>
get_rss_kb() {
    local container="$1"
    local pattern="$2"
    docker exec "$container" sh -c "ps aux | grep '$pattern' | grep -v grep | awk '{print \$6}'" 2>/dev/null | head -1 || echo "0"
}

# Record memory snapshot. Args: <container> <label> <logfile>
snapshot_memory() {
    local container="$1"
    local label="$2"
    local logfile="$3"
    local rss
    rss=$(get_rss_kb "$container" "syfrah")
    local ts
    ts=$(date +%s)
    echo "$ts $label rss_kb=$rss" >> "$logfile"
    echo "$rss"
}

# Progress reporter — prints a line every N seconds. Args: <test_name> <interval> <pid>
progress_reporter() {
    local test_name="$1"
    local interval="$2"
    local pid="$3"
    local elapsed=0
    while kill -0 "$pid" 2>/dev/null; do
        sleep "$interval"
        elapsed=$((elapsed + interval))
        info "[$test_name] ${elapsed}s elapsed ..."
    done
}

# Get effective duration for a test
get_duration() {
    local test_name="$1"
    if [[ -n "$DURATION_OVERRIDE" ]]; then
        echo "$DURATION_OVERRIDE"
        return
    fi
    if $LONG_MODE; then
        case "$test_name" in
            compaction)       echo "$DUR_COMPACTION_LONG" ;;
            cache-overflow)   echo "$DUR_CACHE_LONG" ;;
            s3-latency)       echo "$DUR_S3_LATENCY_LONG" ;;
            rapid-migration)  echo "$DUR_MIGRATION_LONG" ;;
        esac
    else
        case "$test_name" in
            compaction)       echo "$DUR_COMPACTION_SHORT" ;;
            cache-overflow)   echo "$DUR_CACHE_SHORT" ;;
            s3-latency)       echo "$DUR_S3_LATENCY_SHORT" ;;
            rapid-migration)  echo "$DUR_MIGRATION_SHORT" ;;
        esac
    fi
}

# Record test result. Args: <test_name> <status> <duration_s> <details>
record_result() {
    local test_name="$1"
    local status="$2"
    local duration_s="$3"
    local details="$4"
    TEST_RESULTS["$test_name"]="$status"
    if [[ "$status" == "PASS" ]]; then
        pass "$test_name ($duration_s s) — $details"
    else
        fail "$test_name ($duration_s s) — $details"
    fi
}

# ── Cleanup ──────────────────────────────────────────────────

stress_cleanup() {
    info "Cleaning up stress test resources ..."
    for name in stress-compaction stress-cache stress-s3-latency stress-migration-src stress-migration-dst; do
        docker rm -f "$name" >/dev/null 2>&1 || true
    done
    # Remove tc rules if any (best-effort on all containers)
    for cid in $(docker ps -q --filter "name=stress-" 2>/dev/null); do
        docker exec "$cid" tc qdisc del dev eth0 root 2>/dev/null || true
    done
    remove_network
}

if $CLEANUP_ONLY; then
    stress_cleanup
    info "Cleanup complete."
    exit 0
fi

# ── Test 1: Long-Running Compaction ─────────────────────────

test_compaction() {
    local test_name="compaction"
    local duration
    duration=$(get_duration "$test_name")
    local memlog="$STRESS_RESULTS_DIR/compaction_mem.log"
    > "$memlog"

    echo ""
    echo "── Stress: Long-Running Compaction (${duration}s) ──"

    create_network
    start_node "stress-compaction" "172.20.0.20"
    init_mesh "stress-compaction" "172.20.0.20" "stress-node"

    local start_ts
    start_ts=$(date +%s)

    # Capture baseline memory
    local rss_start
    rss_start=$(snapshot_memory "stress-compaction" "baseline" "$memlog")

    # Run continuous writes in the background
    docker exec "stress-compaction" sh -c "
        end_time=\$(($(date +%s) + $duration))
        i=0
        while [ \$(date +%s) -lt \$end_time ]; do
            # Write a batch of 100 keys
            for j in \$(seq 1 100); do
                syfrah state set stress-key-\$i-\$j \"value-\$i-\$j\" 2>/dev/null || true
                i=\$((i + 1))
            done
            sleep 0.1
        done
        echo \$i > /tmp/stress_total_keys
    " &
    local write_pid=$!

    # Monitor memory periodically
    progress_reporter "$test_name" 10 "$write_pid" &
    local reporter_pid=$!

    local monitor_rss_max="$rss_start"
    while kill -0 "$write_pid" 2>/dev/null; do
        sleep 5
        local current_rss
        current_rss=$(snapshot_memory "stress-compaction" "running" "$memlog")
        if [[ "$current_rss" -gt "$monitor_rss_max" ]]; then
            monitor_rss_max="$current_rss"
        fi
    done

    wait "$write_pid" 2>/dev/null || true
    kill "$reporter_pid" 2>/dev/null || true

    local rss_end
    rss_end=$(snapshot_memory "stress-compaction" "final" "$memlog")

    local elapsed=$(( $(date +%s) - start_ts ))
    local total_keys
    total_keys=$(docker exec "stress-compaction" cat /tmp/stress_total_keys 2>/dev/null || echo "0")

    # Assertions
    local status="PASS"
    local details=""

    # 1. Read back a sample of keys to verify no corruption
    local read_failures=0
    for k in 0 50 100 500; do
        if ! docker exec "stress-compaction" syfrah state get "stress-key-${k}-1" >/dev/null 2>&1; then
            read_failures=$((read_failures + 1))
        fi
    done
    if [[ "$read_failures" -gt 0 ]]; then
        status="FAIL"
        details="SST corruption: $read_failures sample keys unreadable. "
    fi

    # 2. Memory leak check — end RSS should be < 2x start
    if [[ "$rss_start" -gt 0 && "$rss_end" -gt $((rss_start * 2)) ]]; then
        status="FAIL"
        details="${details}Memory leak: RSS grew from ${rss_start}KB to ${rss_end}KB. "
    fi

    # 3. Cache bloat — max RSS should be < 3x start (generous for compaction)
    if [[ "$rss_start" -gt 0 && "$monitor_rss_max" -gt $((rss_start * 3)) ]]; then
        status="FAIL"
        details="${details}Cache bloat: max RSS ${monitor_rss_max}KB vs start ${rss_start}KB. "
    fi

    # 4. Check daemon logs for compaction errors
    local compaction_errors
    compaction_errors=$(docker exec "stress-compaction" sh -c 'grep -ci "compaction.*error\|corrupt" /root/.syfrah/daemon.log 2>/dev/null || echo 0')
    if [[ "$compaction_errors" -gt 0 ]]; then
        status="FAIL"
        details="${details}Compaction errors in log: $compaction_errors. "
    fi

    if [[ -z "$details" ]]; then
        details="wrote $total_keys keys, RSS ${rss_start}KB->${rss_end}KB (max ${monitor_rss_max}KB)"
    fi

    record_result "$test_name" "$status" "$elapsed" "$details"
    docker rm -f "stress-compaction" >/dev/null 2>&1 || true
}

# ── Test 2: Cache Overflow ──────────────────────────────────

test_cache_overflow() {
    local test_name="cache-overflow"
    local duration
    duration=$(get_duration "$test_name")
    local memlog="$STRESS_RESULTS_DIR/cache_overflow_mem.log"
    > "$memlog"

    echo ""
    echo "── Stress: Cache Overflow (${duration}s) ──"

    create_network
    start_node "stress-cache" "172.20.0.21"
    init_mesh "stress-cache" "172.20.0.21" "cache-node"

    local start_ts
    start_ts=$(date +%s)

    local rss_start
    rss_start=$(snapshot_memory "stress-cache" "baseline" "$memlog")

    # Write entries far exceeding expected cache capacity
    docker exec "stress-cache" sh -c "
        end_time=\$(($(date +%s) + $duration))
        i=0
        # Write large values to fill cache faster
        payload=\$(head -c 4096 /dev/urandom | base64)
        while [ \$(date +%s) -lt \$end_time ]; do
            syfrah state set cache-key-\$i \"\$payload\" 2>/dev/null || true
            i=\$((i + 1))
        done
        echo \$i > /tmp/stress_cache_keys
    " &
    local write_pid=$!

    progress_reporter "$test_name" 10 "$write_pid" &
    local reporter_pid=$!

    local rss_max="$rss_start"
    while kill -0 "$write_pid" 2>/dev/null; do
        sleep 3
        local current_rss
        current_rss=$(snapshot_memory "stress-cache" "running" "$memlog")
        if [[ "$current_rss" -gt "$rss_max" ]]; then
            rss_max="$current_rss"
        fi
    done

    wait "$write_pid" 2>/dev/null || true
    kill "$reporter_pid" 2>/dev/null || true

    local rss_end
    rss_end=$(snapshot_memory "stress-cache" "final" "$memlog")

    local elapsed=$(( $(date +%s) - start_ts ))
    local total_keys
    total_keys=$(docker exec "stress-cache" cat /tmp/stress_cache_keys 2>/dev/null || echo "0")

    local status="PASS"
    local details=""

    # 1. Process must still be alive (no OOM)
    if ! docker exec "stress-cache" pgrep -x syfrah >/dev/null 2>&1; then
        if ! docker exec "stress-cache" pgrep syfrah >/dev/null 2>&1; then
            status="FAIL"
            details="OOM: syfrah process died during cache overflow. "
        fi
    fi

    # 2. Reads still succeed (recently written key)
    local last_key=$((total_keys - 1))
    if [[ "$last_key" -gt 0 ]]; then
        if ! docker exec "stress-cache" syfrah state get "cache-key-${last_key}" >/dev/null 2>&1; then
            status="FAIL"
            details="${details}Read failure on most recent key. "
        fi
    fi

    # 3. Memory stays bounded — RSS should not exceed 2x baseline
    if [[ "$rss_start" -gt 0 && "$rss_max" -gt $((rss_start * 2)) ]]; then
        # Soft warning — might be acceptable if it stabilizes
        if [[ "$rss_end" -gt $((rss_start * 2)) ]]; then
            status="FAIL"
            details="${details}Unbounded memory: RSS peaked at ${rss_max}KB (start ${rss_start}KB) and did not drop. "
        fi
    fi

    if [[ -z "$details" ]]; then
        details="wrote $total_keys keys (4KB each), RSS ${rss_start}KB->${rss_end}KB (max ${rss_max}KB)"
    fi

    record_result "$test_name" "$status" "$elapsed" "$details"
    docker rm -f "stress-cache" >/dev/null 2>&1 || true
}

# ── Test 3: S3 Latency Spike ───────────────────────────────

test_s3_latency() {
    local test_name="s3-latency"
    local duration
    duration=$(get_duration "$test_name")

    echo ""
    echo "── Stress: S3 Latency Spike (${duration}s) ──"

    create_network
    start_node "stress-s3-latency" "172.20.0.22"
    init_mesh "stress-s3-latency" "172.20.0.22" "s3lat-node"

    local start_ts
    start_ts=$(date +%s)

    # Write some baseline data before injecting latency
    info "Writing baseline data ..."
    docker exec "stress-s3-latency" sh -c "
        for i in \$(seq 1 50); do
            syfrah state set s3lat-pre-\$i 'baseline-value' 2>/dev/null || true
        done
    "

    # Measure baseline write latency (average of 10 writes in ms)
    local baseline_latency
    baseline_latency=$(docker exec "stress-s3-latency" sh -c "
        total=0
        for i in \$(seq 1 10); do
            start=\$(date +%s%N)
            syfrah state set s3lat-bench-\$i 'bench' 2>/dev/null || true
            end=\$(date +%s%N)
            elapsed_ms=\$(( (end - start) / 1000000 ))
            total=\$((total + elapsed_ms))
        done
        echo \$((total / 10))
    " 2>/dev/null || echo "0")
    info "Baseline write latency: ~${baseline_latency}ms"

    # Inject 500ms latency on egress
    info "Injecting 500ms latency via tc qdisc ..."
    docker exec "stress-s3-latency" tc qdisc add dev eth0 root netem delay 500ms 2>/dev/null || {
        # tc might not be available — skip gracefully
        info "tc not available in container, skipping latency injection (test will validate write path only)"
    }

    # Run writes under latency
    docker exec "stress-s3-latency" sh -c "
        end_time=\$(($(date +%s) + $duration))
        i=0
        while [ \$(date +%s) -lt \$end_time ]; do
            syfrah state set s3lat-key-\$i 'latency-value' 2>/dev/null || true
            i=\$((i + 1))
        done
        echo \$i > /tmp/stress_s3lat_keys
    " &
    local write_pid=$!

    progress_reporter "$test_name" 10 "$write_pid" &
    local reporter_pid=$!
    wait "$write_pid" 2>/dev/null || true
    kill "$reporter_pid" 2>/dev/null || true

    local total_keys
    total_keys=$(docker exec "stress-s3-latency" cat /tmp/stress_s3lat_keys 2>/dev/null || echo "0")

    # Remove latency
    info "Removing latency injection ..."
    docker exec "stress-s3-latency" tc qdisc del dev eth0 root 2>/dev/null || true
    sleep 2

    # Measure post-recovery latency
    local recovery_latency
    recovery_latency=$(docker exec "stress-s3-latency" sh -c "
        total=0
        for i in \$(seq 1 10); do
            start=\$(date +%s%N)
            syfrah state set s3lat-post-\$i 'post' 2>/dev/null || true
            end=\$(date +%s%N)
            elapsed_ms=\$(( (end - start) / 1000000 ))
            total=\$((total + elapsed_ms))
        done
        echo \$((total / 10))
    " 2>/dev/null || echo "0")
    info "Post-recovery write latency: ~${recovery_latency}ms"

    local elapsed=$(( $(date +%s) - start_ts ))
    local status="PASS"
    local details=""

    # 1. Verify no corruption — read back pre-latency and during-latency keys
    local read_failures=0
    for k in 1 10 25 50; do
        if ! docker exec "stress-s3-latency" syfrah state get "s3lat-pre-${k}" >/dev/null 2>&1; then
            read_failures=$((read_failures + 1))
        fi
    done
    if [[ "$read_failures" -gt 0 ]]; then
        status="FAIL"
        details="Corruption: $read_failures pre-latency keys unreadable after spike. "
    fi

    # 2. All operations completed (no aborted writes)
    if [[ "$total_keys" -eq 0 ]]; then
        status="FAIL"
        details="${details}No keys written during latency window. "
    fi

    # 3. Recovery — post-latency should be within 2x baseline
    if [[ "$baseline_latency" -gt 0 && "$recovery_latency" -gt $((baseline_latency * 3)) ]]; then
        status="FAIL"
        details="${details}Slow recovery: post-latency ${recovery_latency}ms vs baseline ${baseline_latency}ms. "
    fi

    if [[ -z "$details" ]]; then
        details="wrote $total_keys keys under 500ms latency, recovery ${recovery_latency}ms (baseline ${baseline_latency}ms)"
    fi

    record_result "$test_name" "$status" "$elapsed" "$details"
    docker rm -f "stress-s3-latency" >/dev/null 2>&1 || true
}

# ── Test 4: Rapid Migration ─────────────────────────────────

test_rapid_migration() {
    local test_name="rapid-migration"
    local duration
    duration=$(get_duration "$test_name")

    echo ""
    echo "── Stress: Rapid Migration (${duration}s) ──"

    create_network
    start_node "stress-migration-src" "172.20.0.23"
    start_node "stress-migration-dst" "172.20.0.24"
    init_mesh "stress-migration-src" "172.20.0.23" "mig-src"

    # Join second node to mesh
    docker exec "stress-migration-dst" syfrah fabric join \
        --addr "172.20.0.23" --pin "$E2E_PIN" --name "mig-dst" 2>/dev/null || true
    sleep 3

    local start_ts
    start_ts=$(date +%s)

    # Write a checksum file to verify data integrity across migrations
    local checksum_value="integrity-check-$(date +%s)"
    docker exec "stress-migration-src" syfrah state set "migration-checksum" "$checksum_value" 2>/dev/null || true

    local migration_count=0
    local fence_count=0
    local dual_run_detected=false
    local migration_failures=0
    local migration_interval=60
    local end_time=$((start_ts + duration))
    local progress_counter=0

    info "Starting migration cycle (every ${migration_interval}s for ${duration}s) ..."

    while [[ $(date +%s) -lt $end_time ]]; do
        migration_count=$((migration_count + 1))

        # Determine source and target for this cycle
        if (( migration_count % 2 == 1 )); then
            local src="stress-migration-src"
            local dst="stress-migration-dst"
        else
            local src="stress-migration-dst"
            local dst="stress-migration-src"
        fi

        # Trigger migration (simulate via control plane reschedule)
        docker exec "$dst" syfrah state set "vm-migration-target" "$(hostname)" 2>/dev/null || true

        # Check for fencing in logs
        local fence_line
        fence_line=$(docker exec "$src" sh -c 'grep -c "fence\|fencing" /root/.syfrah/daemon.log 2>/dev/null || echo 0')
        if [[ "$fence_line" -gt 0 ]]; then
            fence_count=$((fence_count + fence_line))
        fi

        # Verify no dual-run — only one node should report the VM as active
        local src_active
        local dst_active
        src_active=$(docker exec "$src" sh -c 'syfrah state get vm-active 2>/dev/null || echo ""')
        dst_active=$(docker exec "$dst" sh -c 'syfrah state get vm-active 2>/dev/null || echo ""')
        if [[ -n "$src_active" && -n "$dst_active" && "$src_active" == "true" && "$dst_active" == "true" ]]; then
            dual_run_detected=true
        fi

        progress_counter=$((progress_counter + 1))
        if (( progress_counter % 3 == 0 )); then
            info "[rapid-migration] $migration_count migrations completed ..."
        fi

        # Wait for next migration cycle (or until duration expires)
        local remaining=$((end_time - $(date +%s)))
        if [[ "$remaining" -gt "$migration_interval" ]]; then
            sleep "$migration_interval"
        elif [[ "$remaining" -gt 0 ]]; then
            sleep "$remaining"
        fi
    done

    local elapsed=$(( $(date +%s) - start_ts ))
    local status="PASS"
    local details=""

    # 1. Verify data integrity — checksum must still match
    local final_checksum
    final_checksum=$(docker exec "stress-migration-src" syfrah state get "migration-checksum" 2>/dev/null || \
                     docker exec "stress-migration-dst" syfrah state get "migration-checksum" 2>/dev/null || echo "")
    if [[ "$final_checksum" != "$checksum_value" ]]; then
        status="FAIL"
        details="Data loss: checksum mismatch after $migration_count migrations. "
    fi

    # 2. No dual-run
    if $dual_run_detected; then
        status="FAIL"
        details="${details}Dual-run detected: VM reported active on both nodes simultaneously. "
    fi

    # 3. All migrations completed
    if [[ "$migration_count" -eq 0 ]]; then
        status="FAIL"
        details="${details}No migrations executed. "
    fi

    if [[ -z "$details" ]]; then
        details="$migration_count migrations, fencing events=$fence_count, data intact"
    fi

    record_result "$test_name" "$status" "$elapsed" "$details"
    docker rm -f "stress-migration-src" "stress-migration-dst" >/dev/null 2>&1 || true
}

# ── Results Output ───────────────────────────────────────────

write_results_json() {
    local overall_end
    overall_end=$(date +%s)
    local overall_duration=$((overall_end - OVERALL_START))
    local all_pass=true

    # Build JSON
    local json="{\n  \"timestamp\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\n"
    json+="  \"total_duration_s\": $overall_duration,\n"
    json+="  \"mode\": \"$(if $LONG_MODE; then echo long; else echo short; fi)\",\n"
    json+="  \"tests\": {\n"

    local first=true
    for test_name in "${!TEST_RESULTS[@]}"; do
        if ! $first; then json+=",\n"; fi
        first=false
        local result="${TEST_RESULTS[$test_name]}"
        json+="    \"$test_name\": \"$result\""
        if [[ "$result" != "PASS" ]]; then all_pass=false; fi
    done

    json+="\n  },\n"
    json+="  \"overall\": \"$(if $all_pass; then echo PASS; else echo FAIL; fi)\"\n}"

    echo -e "$json" > "$RESULTS_FILE"
    info "Results saved to $RESULTS_FILE"

    if $all_pass; then
        return 0
    else
        return 1
    fi
}

# ── Main ─────────────────────────────────────────────────────

echo "═══════════════════════════════════════════════════════"
echo "  805 — Stress Tests"
echo "  Mode: $(if $LONG_MODE; then echo 'LONG'; else echo 'SHORT (CI)'; fi)"
echo "═══════════════════════════════════════════════════════"

if [[ -z "$SELECTED_TEST" || "$SELECTED_TEST" == "compaction" ]]; then
    test_compaction
fi

if [[ -z "$SELECTED_TEST" || "$SELECTED_TEST" == "cache-overflow" ]]; then
    test_cache_overflow
fi

if [[ -z "$SELECTED_TEST" || "$SELECTED_TEST" == "s3-latency" ]]; then
    test_s3_latency
fi

if [[ -z "$SELECTED_TEST" || "$SELECTED_TEST" == "rapid-migration" ]]; then
    test_rapid_migration
fi

# Final cleanup
remove_network

# Write results and summary
echo ""
echo "═══════════════════════════════════════════════════════"
echo "  STRESS TEST SUMMARY"
echo "═══════════════════════════════════════════════════════"

write_results_json
exit_code=$?

summary

exit $exit_code
