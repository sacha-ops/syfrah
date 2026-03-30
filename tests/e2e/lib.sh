#!/usr/bin/env bash
# Shared functions for E2E test scenarios.
# Source this file at the top of each scenario.

set -euo pipefail

# Ensure cleanup runs even if the script exits early (e.g. set -e triggered).
# Each scenario also calls cleanup() explicitly; the trap handles unexpected exits
# so containers don't leak between scenarios.
trap 'cleanup 2>/dev/null || true' EXIT

# ── Config ────────────────────────────────────────────────────

E2E_IMAGE="${E2E_IMAGE:-syfrah-e2e-test}"
E2E_NETWORK="${E2E_NETWORK:-syfrah-e2e}"
E2E_SUBNET="${E2E_SUBNET:-172.20.0.0/24}"
E2E_PIN="${E2E_PIN:-4829}"
E2E_MESH="${E2E_MESH:-e2e-test}"
E2E_CONTAINERS=()

# ── Colors ────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

PASSED=0
FAILED=0

pass() { echo -e "  ${GREEN}✓ $1${NC}"; PASSED=$((PASSED + 1)); }
fail() { echo -e "  ${RED}✗ $1${NC}"; FAILED=$((FAILED + 1)); }
info() { echo -e "${YELLOW}→ $1${NC}"; }
debug() { echo -e "  ${NC}  [debug] $1${NC}"; }

# ── Network ───────────────────────────────────────────────────

create_network() {
    docker network create "$E2E_NETWORK" \
        --subnet "$E2E_SUBNET" \
        --driver bridge \
        >/dev/null 2>&1 || true
    preflight_check
}

# Verify Docker container networking works before any test.
preflight_check() {
    # Clean up any leftover probe containers
    docker rm -f preflight-probe-1 preflight-probe-2 >/dev/null 2>&1 || true
    docker run -d --name preflight-probe-1 --network "$E2E_NETWORK" --ip 172.20.0.253 "$E2E_IMAGE" sleep 10 || {
        fail "PREFLIGHT: Failed to start probe container (image=$E2E_IMAGE)"
        docker rm -f preflight-probe-1 preflight-probe-2 >/dev/null 2>&1 || true
        return 1
    }
    docker run -d --name preflight-probe-2 --network "$E2E_NETWORK" --ip 172.20.0.254 "$E2E_IMAGE" sleep 10 || {
        fail "PREFLIGHT: Failed to start probe container (image=$E2E_IMAGE)"
        docker rm -f preflight-probe-1 preflight-probe-2 >/dev/null 2>&1 || true
        return 1
    }
    if ! docker exec preflight-probe-1 ping -c1 -W2 172.20.0.254 >/dev/null 2>&1; then
        fail "PREFLIGHT: Docker container networking is broken — containers cannot ping each other"
        docker rm -f preflight-probe-1 preflight-probe-2 >/dev/null 2>&1 || true
        return 1
    fi
    docker rm -f preflight-probe-1 preflight-probe-2 >/dev/null 2>&1 || true
    debug "preflight: Docker networking OK"
}

remove_network() {
    docker network rm "$E2E_NETWORK" >/dev/null 2>&1 || true
}

# ── Containers ────────────────────────────────────────────────

# Start a container. Args: <name> <ip>
# When E2E_BINARY_MOUNT is set, the local binary is volume-mounted into the
# container instead of using the one baked into the Docker image.
start_node() {
    local name="$1"
    local ip="$2"

    docker rm -f "$name" >/dev/null 2>&1 || true

    local volume_args=()
    if [ -n "${E2E_BINARY_MOUNT:-}" ]; then
        volume_args=(-v "${E2E_BINARY_MOUNT}:/usr/local/bin/syfrah:ro")
    fi

    debug "starting container $name at $ip${E2E_BINARY_MOUNT:+ (local binary)}"
    docker run -d \
        --name "$name" \
        --network "$E2E_NETWORK" \
        --ip "$ip" \
        --privileged \
        --hostname "$name" \
        --init \
        "${volume_args[@]+"${volume_args[@]}"}" \
        "$E2E_IMAGE" >/dev/null

    E2E_CONTAINERS+=("$name")
}

# Wait for the syfrah daemon to be responsive. Args: <container> [max_wait]
# Checks both socket existence AND that the daemon actually responds to commands.
wait_daemon() {
    local container="$1"
    local max_wait="${2:-30}"
    local i=0
    debug "waiting for daemon on $container (max ${max_wait}s)"
    while [ $i -lt "$max_wait" ]; do
        # Socket must exist AND daemon must respond
        if docker exec "$container" test -S /root/.syfrah/control.sock 2>/dev/null \
           && docker exec "$container" syfrah fabric status >/dev/null 2>&1; then
            debug "daemon ready on $container after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    fail "daemon on $container did not start within ${max_wait}s"
    info "Docker logs for $container:"
    docker logs "$container" 2>&1 | tail -30 || true
    info "Processes in $container:"
    docker exec "$container" ps aux 2>/dev/null || true
    info "Files in .syfrah:"
    docker exec "$container" ls -la /root/.syfrah/ 2>/dev/null || true
    info "Syfrah log:"
    docker exec "$container" cat /root/.syfrah/syfrah.log 2>/dev/null | tail -20 || true
    return 1
}

# ── WireGuard readiness ──────────────────────────────────────

# Wait for WireGuard interface to appear. Args: <container> [max_wait]
wait_for_wg_interface() {
    local container="$1"
    local max_wait="${2:-15}"
    local i=0
    debug "waiting for WireGuard interface on $container"
    while [ $i -lt "$max_wait" ]; do
        if docker exec "$container" ip link show syfrah0 >/dev/null 2>&1 \
           && docker exec "$container" wg show syfrah0 >/dev/null 2>&1; then
            debug "WireGuard interface up on $container after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    info "WireGuard interface not found on $container after ${max_wait}s"
    docker exec "$container" ip link 2>/dev/null || true
    docker exec "$container" wg show syfrah0 2>/dev/null || true
    return 1
}

# ── Mesh operations ───────────────────────────────────────────

# Initialize a mesh on a container. Args: <container> <ip> [node_name]
init_mesh() {
    local container="$1"
    local ip="$2"
    local node_name="${3:-$container}"

    debug "init_mesh: $container at $ip as $node_name"
    docker exec -d -e RUST_LOG=info,syfrah_compute=debug "$container" \
        syfrah fabric init \
        --name "$E2E_MESH" \
        --node-name "$node_name" \
        --endpoint "${ip}:51820"

    wait_daemon "$container"
    if ! wait_for_wg_interface "$container"; then
        info "WARNING: WireGuard interface not ready on $container after init_mesh"
    fi
    debug "init_mesh: $container done"
}

# Start peering with PIN on a container. Args: <container>
start_peering() {
    local container="$1"
    debug "start_peering: $container with PIN $E2E_PIN"
    timeout 10 docker exec "$container" \
        syfrah fabric peering start --pin "$E2E_PIN" || {
        info "start_peering timed out or failed on $container"
        info "Docker logs:"
        docker logs "$container" 2>&1 | tail -10 || true
        info "Control socket:"
        docker exec "$container" ls -la /root/.syfrah/control.sock 2>/dev/null || echo "(missing)"
        return 1
    }
    debug "start_peering: $container done"
}

# Join a mesh. Args: <container> <target_ip> <own_ip> [node_name]
join_mesh() {
    local container="$1"
    local target_ip="$2"
    local own_ip="$3"
    local node_name="${4:-$container}"

    debug "join_mesh: $container joining via $target_ip as $node_name"
    docker exec -d "$container" \
        syfrah fabric join "${target_ip}:51821" \
        --node-name "$node_name" \
        --endpoint "${own_ip}:51820" \
        --pin "$E2E_PIN"

    wait_daemon "$container"
    if ! wait_for_wg_interface "$container"; then
        info "WARNING: WireGuard interface not ready on $container after join_mesh"
    fi
    debug "join_mesh: $container done"
}

# Leave the mesh. Args: <container>
leave_mesh() {
    local container="$1"
    docker exec "$container" syfrah fabric leave --yes 2>&1 || true
}

# Stop the daemon. Args: <container>
stop_daemon() {
    local container="$1"
    docker exec "$container" syfrah fabric stop 2>&1 || true
}

# Ensure daemon is alive on a container, restarting it if needed. Args: <container>
ensure_daemon() {
    local container="$1"
    if docker exec "$container" syfrah fabric status >/dev/null 2>&1; then
        return 0
    fi
    debug "ensure_daemon: daemon not responding on $container, restarting"
    # Clean up stale socket if present
    docker exec "$container" rm -f /root/.syfrah/control.sock 2>/dev/null || true
    docker exec -d "$container" syfrah fabric start
    wait_daemon "$container"
}

# ── Assertions ────────────────────────────────────────────────

# Assert daemon is running (or was running and set up the mesh). Args: <container>
assert_daemon_running() {
    local container="$1"
    # Check via status command OR by verifying syfrah0 exists + state.json has peers
    if docker exec "$container" syfrah fabric status 2>&1 | grep -q "running"; then
        pass "$container daemon is running"
    elif docker exec "$container" ip link show syfrah0 >/dev/null 2>&1; then
        # Daemon may have exited but setup completed (interface exists)
        pass "$container daemon setup completed (interface active)"
    else
        fail "$container daemon is not running"
    fi
}

# Assert peer count. Args: <container> <expected_count>
assert_peer_count() {
    local container="$1"
    local expected="$2"
    local actual
    actual=$(docker exec "$container" syfrah fabric peers 2>&1 | grep -c "active" || true)
    actual=${actual:-0}
    if [ "$actual" -eq "$expected" ]; then
        pass "$container sees $expected peers"
    else
        fail "$container sees $actual peers (expected $expected)"
        docker exec "$container" syfrah fabric peers 2>&1 || true
    fi
}

# Assert syfrah0 interface exists. Args: <container>
assert_interface_exists() {
    local container="$1"
    if docker exec "$container" ip link show syfrah0 2>&1 | grep -q "syfrah0"; then
        pass "$container has syfrah0 interface"
    else
        fail "$container missing syfrah0 interface"
    fi
}

# Assert can ping a mesh IPv6 address. Args: <from_container> <ipv6>
assert_can_ping() {
    local from="$1"
    local ipv6="$2"
    if [ -z "$ipv6" ]; then
        fail "$from cannot ping (empty IPv6 address)"
        return
    fi
    if docker exec "$from" ping -6 -c 1 -W 3 "$ipv6" >/dev/null 2>&1; then
        pass "$from can ping $ipv6"
    else
        fail "$from cannot ping $ipv6"
    fi
}

# Get the mesh IPv6 of a container. Args: <container>
# Retries up to 5 times (1s apart) if the result is empty.
get_mesh_ipv6() {
    local container="$1"
    local ipv6=""
    for _attempt in $(seq 1 5); do
        ipv6=$(docker exec "$container" syfrah fabric status 2>&1 | grep "Mesh IPv6" | awk '{print $NF}' || echo "")
        if [ -n "$ipv6" ]; then
            echo "$ipv6"
            return 0
        fi
        sleep 1
    done
    echo ""
    fail "get_mesh_ipv6: could not get mesh IPv6 for $container after 5 attempts"
    return 1
}

# Assert syfrah0 interface does NOT exist. Args: <container>
assert_interface_gone() {
    local container="$1"
    if ! docker exec "$container" ip link show syfrah0 >/dev/null 2>&1; then
        pass "$container syfrah0 interface removed"
    else
        fail "$container syfrah0 interface still exists"
    fi
}

# Assert daemon is NOT running. Args: <container>
assert_daemon_stopped() {
    local container="$1"
    if docker exec "$container" syfrah fabric status 2>&1 | grep -q "stopped"; then
        pass "$container daemon is stopped"
    else
        # Also check if process is gone
        if ! docker exec "$container" pgrep -f "syfrah" >/dev/null 2>&1; then
            pass "$container daemon is stopped (process gone)"
        else
            fail "$container daemon is still running"
        fi
    fi
}

# ── Network manipulation ──────────────────────────────────────

# Block traffic between two containers. Args: <container> <target_ip>
block_traffic() {
    local container="$1"
    local target_ip="$2"
    debug "block_traffic: $container ↛ $target_ip"
    docker exec "$container" iptables -A OUTPUT -d "$target_ip" -j DROP 2>/dev/null || true
    docker exec "$container" iptables -A INPUT -s "$target_ip" -j DROP 2>/dev/null || true
}

# Unblock traffic. Args: <container> <target_ip>
unblock_traffic() {
    local container="$1"
    local target_ip="$2"
    debug "unblock_traffic: $container ↔ $target_ip"
    docker exec "$container" iptables -D OUTPUT -d "$target_ip" -j DROP 2>/dev/null || true
    docker exec "$container" iptables -D INPUT -s "$target_ip" -j DROP 2>/dev/null || true
}

# Assert CANNOT ping. Args: <from_container> <ipv6>
assert_cannot_ping() {
    local from="$1"
    local ipv6="$2"
    if [ -z "$ipv6" ]; then
        fail "$from assert_cannot_ping called with empty IPv6 address"
        return
    fi
    if ! docker exec "$from" ping -6 -c 1 -W 2 "$ipv6" >/dev/null 2>&1; then
        pass "$from cannot ping $ipv6 (expected)"
    else
        fail "$from CAN ping $ipv6 (should be blocked)"
    fi
}

# Assert a command fails (non-zero exit). Args: <container> <command...>
assert_command_fails() {
    local container="$1"
    shift
    debug "assert_command_fails: $container $*"
    if timeout 15 docker exec "$container" "$@" >/dev/null 2>&1; then
        fail "command should have failed: $*"
    else
        pass "command failed as expected: $*"
    fi
}

# Assert a command succeeds. Args: <container> <command...>
assert_command_succeeds() {
    local container="$1"
    shift
    debug "assert_command_succeeds: $container $*"
    if timeout 15 docker exec "$container" "$@" >/dev/null 2>&1; then
        pass "command succeeded: $*"
    else
        fail "command failed: $*"
    fi
}

# Get a field from state.json. Args: <container> <jq_filter>
get_state_field() {
    docker exec "$1" cat /root/.syfrah/state.json 2>/dev/null | jq -r "$2" 2>/dev/null
}

# Assert state.json exists. Args: <container>
assert_state_exists() {
    if docker exec "$1" test -f /root/.syfrah/state.json 2>/dev/null; then
        pass "$1 has state.json"
    else
        fail "$1 missing state.json"
    fi
}

# Assert state.json does NOT exist. Args: <container>
assert_state_gone() {
    if ! docker exec "$1" test -f /root/.syfrah/state.json 2>/dev/null; then
        pass "$1 state.json removed"
    else
        fail "$1 state.json still exists"
    fi
}

# Wait for all nodes to see expected peer count. Args: <prefix> <count> <expected_peers> <timeout>
wait_for_convergence() {
    local prefix="$1"
    local count="$2"
    local expected="$3"
    local timeout="${4:-120}"
    local deadline=$(($(date +%s) + timeout))

    while [ "$(date +%s)" -lt "$deadline" ]; do
        local all_ok=true
        for i in $(seq 1 "$count"); do
            local actual
            actual=$(docker exec "${prefix}${i}" syfrah fabric peers 2>&1 | grep -c "active" 2>/dev/null || echo "0")
            actual=$(echo "$actual" | tr -d '[:space:]')
            if [ "$actual" -ne "$expected" ] 2>/dev/null; then
                all_ok=false
                break
            fi
        done
        if [ "$all_ok" = true ]; then
            return 0
        fi
        sleep 2
    done
    info "Diagnostic dump (wait_for_convergence timed out after ${timeout}s):"
    for i in $(seq 1 "$count"); do
        local node="${prefix}${i}"
        info "  --- $node ---"
        docker exec "$node" syfrah fabric status 2>&1 || true
        docker exec "$node" syfrah fabric peers --json 2>&1 || true
        docker exec "$node" ip link show syfrah0 2>&1 || true
    done
    return 1
}

# Wait for a single container to see at least min_count active peers.
# Args: <container> <min_count> <timeout>
wait_for_peer_active() {
    local container="$1"
    local min_count="$2"
    local timeout="${3:-60}"
    local deadline=$(($(date +%s) + timeout))

    while [ "$(date +%s)" -lt "$deadline" ]; do
        local actual
        actual=$(docker exec "$container" syfrah fabric peers 2>&1 | grep -c "active" || echo "0")
        actual=$(echo "$actual" | tr -d '[:space:]')
        if [ "$actual" -ge "$min_count" ] 2>/dev/null; then
            return 0
        fi
        sleep 2
    done
    info "Diagnostic dump for $container (wait_for_peer_active timed out after ${timeout}s):"
    docker exec "$container" syfrah fabric status 2>&1 || true
    docker exec "$container" syfrah fabric peers --json 2>&1 || true
    docker exec "$container" ip link show syfrah0 2>&1 || true
    return 1
}

# ── UX output assertions ─────────────────────────────────────

# Verify command output contains an expected string
# Usage: assert_output_contains <container> "<command>" "<expected>"
assert_output_contains() {
    local container="$1" cmd="$2" expected="$3"
    local output
    output=$(docker exec "$container" sh -c "$cmd" 2>&1)
    if echo "$output" | grep -qF "$expected"; then
        pass "$cmd output contains '$expected'"
    else
        fail "$cmd output missing '$expected'. Got: $(echo "$output" | head -5)"
    fi
}

# Verify command output does NOT contain a forbidden string
# Usage: assert_output_not_contains <container> "<command>" "<forbidden>"
assert_output_not_contains() {
    local container="$1" cmd="$2" forbidden="$3"
    local output
    output=$(docker exec "$container" sh -c "$cmd" 2>&1)
    if echo "$output" | grep -qF "$forbidden"; then
        fail "$cmd output contains forbidden '$forbidden'"
    else
        pass "$cmd output clean (no '$forbidden')"
    fi
}

# Verify command output matches a regex
# Usage: assert_output_matches <container> "<command>" "<regex>"
assert_output_matches() {
    local container="$1" cmd="$2" regex="$3"
    local output
    output=$(docker exec "$container" sh -c "$cmd" 2>&1)
    if echo "$output" | grep -qE "$regex"; then
        pass "$cmd matches /$regex/"
    else
        fail "$cmd does not match /$regex/. Got: $(echo "$output" | head -5)"
    fi
}

# ── UX peer table assertions ────────────────────────────────

# Verify no duplicate peer names in peers output
# Usage: assert_no_duplicate_peers <container>
assert_no_duplicate_peers() {
    local container="$1"
    local output dupes
    output=$(docker exec "$container" syfrah fabric peers 2>&1 | tail -n +3)
    dupes=$(echo "$output" | awk '{print $1}' | sort | uniq -d)
    if [ -z "$dupes" ]; then
        pass "$container: no duplicate peers"
    else
        fail "$container: duplicate peers found: $dupes"
    fi
}

# Verify region and zone are populated (not "-") for all peers
# Usage: assert_regions_displayed <container>
assert_regions_displayed() {
    local container="$1"
    local output
    output=$(docker exec "$container" syfrah fabric peers 2>&1 | tail -n +3)
    if echo "$output" | grep -qE "\s-\s+-\s+"; then
        fail "$container: peers have empty region/zone"
    else
        pass "$container: all peers have region/zone"
    fi
}

# Verify no epoch dates (20535d, 19000d, etc.)
# Usage: assert_no_epoch_dates <container>
assert_no_epoch_dates() {
    local container="$1"
    local output
    output=$(docker exec "$container" syfrah fabric peers 2>&1)
    if echo "$output" | grep -qE "[0-9]{4,}d ago"; then
        fail "$container: epoch date found in peers output"
    else
        pass "$container: no epoch dates"
    fi
}

# ── UX state assertions ─────────────────────────────────────

# Verify state is fully clean after leave
# Usage: assert_clean_state <container>
assert_clean_state() {
    local container="$1"
    local remnants
    remnants=$(docker exec "$container" sh -c \
        'ls ~/.syfrah/state.json ~/.syfrah/daemon.pid 2>/dev/null; test -d ~/.syfrah && ls ~/.syfrah/*.redb 2>/dev/null' || true)
    if [ -z "$remnants" ]; then
        pass "$container: state fully clean"
    else
        fail "$container: state remnants found: $remnants"
    fi
}

# Verify join can be retried after failure without leave
# Usage: assert_join_retry_works <container> <target> <ip> <name>
assert_join_retry_works() {
    local container="$1" target="$2" ip="$3" name="$4"
    # First attempt should fail or succeed
    # Second attempt must not say "state already exists"
    local output
    output=$(docker exec "$container" syfrah fabric join "$target" \
        --node-name "$name" --endpoint "$ip:51820" 2>&1 || true)
    if echo "$output" | grep -qi "state already exists"; then
        fail "$container: retry blocked by phantom state"
    else
        pass "$container: join retry not blocked"
    fi
}

# ── UX command suggestion assertions ─────────────────────────

# Verify that every command mentioned in output actually exists
# Usage: assert_all_commands_valid <container> "<command>"
assert_all_commands_valid() {
    local container="$1" cmd="$2"
    local output
    output=$(docker exec "$container" sh -c "$cmd" 2>&1 || true)
    # Extract anything that looks like "syfrah ..."
    local suggested
    suggested=$(echo "$output" | grep -oE "syfrah [a-z]+ [a-z]+" | sort -u || true)
    for suggestion in $suggested; do
        local subcmd
        subcmd=$(echo "$suggestion" | awk '{print $2, $3}')
        if docker exec "$container" syfrah $subcmd --help >/dev/null 2>&1; then
            pass "suggested command '$suggestion' exists"
        else
            fail "suggested command '$suggestion' does NOT exist"
        fi
    done
}

# Verify error output suggests a specific next command
# Usage: assert_command_suggests <container> "<command>" "<expected_suggestion>"
assert_command_suggests() {
    local container="$1" cmd="$2" expected_suggestion="$3"
    local output
    output=$(docker exec "$container" sh -c "$cmd" 2>&1 || true)
    if echo "$output" | grep -qF "$expected_suggestion"; then
        pass "$cmd suggests '$expected_suggestion'"
    else
        fail "$cmd does not suggest '$expected_suggestion'"
    fi
}

# ── UX cross-command consistency assertions ──────────────────

# Verify region is the same across status and peers
# Usage: assert_consistent_region <container>
assert_consistent_region() {
    local container="$1"
    local status_region peers_region
    status_region=$(docker exec "$container" syfrah fabric status 2>&1 | grep -i region | awk '{print $2}' || echo "")
    peers_region=$(docker exec "$container" syfrah fabric peers 2>&1 | tail -n +3 | head -1 | awk '{print $2}' || echo "")
    if [ -z "$status_region" ] && [ -z "$peers_region" ]; then
        fail "$container: could not extract region from status or peers"
        return
    fi
    if [ "$status_region" = "$peers_region" ]; then
        pass "$container: region consistent (status=peers=$status_region)"
    else
        fail "$container: region mismatch (status=$status_region, peers=$peers_region)"
    fi
}

# Verify peer count in status matches lines in peers
# Usage: assert_consistent_peer_count <container>
assert_consistent_peer_count() {
    local container="$1"
    local status_count peers_count
    peers_count=$(docker exec "$container" syfrah fabric peers 2>&1 | tail -n +3 | grep -c "active\|unreach" || echo "0")
    # status should show same number
    status_count=$(docker exec "$container" syfrah fabric status 2>&1 | grep -i "peer" | grep -oE "[0-9]+" | head -1 || echo "0")
    if [ "$peers_count" = "$status_count" ]; then
        pass "$container: peer count consistent ($peers_count)"
    else
        fail "$container: peer count mismatch (peers=$peers_count, status=$status_count)"
    fi
}

# ── Catalog config ───────────────────────────────────────────

SYFRAH_CATALOG_URL="${SYFRAH_CATALOG_URL:-https://github.com/sacha-ops/syfrah-images/releases/latest/download/catalog.json}"
SYFRAH_IMAGE_DIR="/opt/syfrah/images"
SYFRAH_KERNEL_PATH="/opt/syfrah/vmlinux"
SYFRAH_CATALOG_PATH="/opt/syfrah/catalog.json"

# ── Image helpers ────────────────────────────────────────────

# List images via CLI. Args: <container> [--json]
list_images() {
    local node="$1"; shift
    docker exec "$node" syfrah compute image list "$@" 2>&1
}

# Inspect an image via CLI. Args: <container> <name>
inspect_image() {
    docker exec "$1" syfrah compute image inspect "$2" 2>&1
}

# Import a raw image via CLI. Args: <container> <path> <name>
import_image() {
    docker exec "$1" syfrah compute image import "$2" --name "$3" 2>&1
}

# Delete an image via CLI. Args: <container> <name>
delete_image() {
    docker exec "$1" syfrah compute image delete "$2" --yes 2>&1
}

# Assert image exists in the local store. Args: <container> <name>
assert_image_exists() {
    local node="$1" name="$2"
    if docker exec "$node" test -f "${SYFRAH_IMAGE_DIR}/${name}.raw" 2>/dev/null; then
        pass "$node has image $name"
    else
        fail "$node missing image $name"
    fi
}

# Assert image does NOT exist. Args: <container> <name>
assert_image_gone() {
    local node="$1" name="$2"
    if docker exec "$node" test -f "${SYFRAH_IMAGE_DIR}/${name}.raw" 2>/dev/null; then
        fail "$node still has image $name"
    else
        pass "$node image $name removed"
    fi
}

# Assert image count in list output. Args: <container> <expected>
assert_image_count() {
    local node="$1" expected="$2"
    local count
    count=$(list_images "$node" --json | jq 'length' 2>/dev/null || echo 0)
    if [ "$count" = "$expected" ]; then
        pass "Image count is $expected"
    else
        fail "Image count is $count, expected $expected"
    fi
}

# ── Compute helpers ────────────────────────────────────────────

create_vm() {
    local node="$1" name="$2"; shift 2
    docker exec "$node" syfrah compute vm create --name "$name" "$@" 2>&1
}

list_vms() {
    docker exec "$1" syfrah compute vm list --json 2>&1
}

get_vm() {
    docker exec "$1" syfrah compute vm get "$2" --json 2>&1
}

stop_vm() {
    local node="$1" id="$2"; shift 2
    docker exec "$node" syfrah compute vm stop "$id" "$@" 2>&1
}

delete_vm() {
    docker exec "$1" syfrah compute vm delete "$2" --yes 2>&1
}

assert_vm_phase() {
    local node="$1" id="$2" expected="$3"
    local phase
    phase=$(get_vm "$node" "$id" | jq -r '.phase // empty')
    if [ "$phase" = "$expected" ]; then
        pass "VM $id phase is $expected"
    else
        fail "VM $id phase is '$phase', expected '$expected'"
    fi
}

assert_vm_count() {
    local node="$1" expected="$2"
    local count
    count=$(list_vms "$node" | jq 'length')
    if [ "$count" = "$expected" ]; then
        pass "VM count is $expected"
    else
        fail "VM count is $count, expected $expected"
    fi
}

wait_for_vm_phase() {
    local node="$1" id="$2" phase="$3" timeout="${4:-30}"
    local elapsed=0
    while [ $elapsed -lt "$timeout" ]; do
        local current
        current=$(get_vm "$node" "$id" | jq -r '.phase // empty')
        [ "$current" = "$phase" ] && return 0
        sleep 1
        elapsed=$((elapsed + 1))
    done
    fail "VM $id did not reach phase $phase within ${timeout}s (current: $current)"
}

# ── Cleanup ───────────────────────────────────────────────────

cleanup() {
    debug "cleanup: removing ${#E2E_CONTAINERS[@]} containers"
    for c in "${E2E_CONTAINERS[@]}"; do
        docker rm -f "$c" >/dev/null 2>&1 || true
    done
    E2E_CONTAINERS=()
}

# ── Summary ───────────────────────────────────────────────────

# Print results summary. Returns exit code 1 if any tests failed.
summary() {
    local total=$((PASSED + FAILED))
    echo ""
    if [ "$FAILED" -eq 0 ]; then
        echo -e "  ${GREEN}$total/$total passed${NC}"
        return 0
    else
        echo -e "  ${RED}$FAILED/$total failed${NC}"
        return 1
    fi
}
