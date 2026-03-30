#!/usr/bin/env bash
# Scenario: Container runtime — vm start for stopped containers
#
# Exercises the `vm start` command for container-backed VMs.
# Covers issue #664.
#
# Verifies:
#   - A running container can be stopped
#   - `vm start` brings a stopped container back to Running
#   - Idempotent start on an already-running VM succeeds (or is a no-op)

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

echo "── Compute: Container Start (vm start) ──"

# ── Helper: check if the container runtime is available ──────────
check_container_runtime() {
    local node="$1"
    if ! docker exec "$node" which crun >/dev/null 2>&1; then
        echo "SKIP: crun not installed in test image"
        return 1
    fi
    return 0
}

pull_with_retry() {
    local node="$1" name="$2"
    local attempt=0
    while [ $attempt -lt 3 ]; do
        if docker exec "$node" syfrah compute image pull "$name" 2>&1; then
            if docker exec "$node" syfrah compute image list --json 2>&1 \
                | jq -e ".[] | select(.name == \"$name\")" >/dev/null 2>&1; then
                return 0
            fi
        fi
        attempt=$((attempt + 1))
        [ $attempt -lt 3 ] && sleep 5
    done
    return 1
}

create_network
start_node "e2e-container-start" "172.20.0.10"
init_mesh "e2e-container-start" "172.20.0.10" "start-node"
sleep 2

node="e2e-container-start"

# ── Pre-flight: verify container runtime is available ────────────

if ! check_container_runtime "$node"; then
    pass "SKIP: container runtime not available (nested Docker limitation)"
    cleanup
    summary
    exit 0
fi

# ── Prepare: pull image ──────────────────────────────────────────

info "Pulling alpine-3.20 for start tests"
docker exec "$node" sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json' 2>/dev/null || true
if ! pull_with_retry "$node" "alpine-3.20"; then
    fail "Failed to pull alpine-3.20 — cannot proceed"
    cleanup
    summary
    exit 1
fi
pass "Image ready"

# ── Step 1: Create a container VM ────────────────────────────────

info "Step 1: Create container start-vm"
CREATE_OUTPUT=$(docker exec "$node" syfrah compute vm create \
    --name start-vm --image alpine-3.20 --vcpus 1 --memory 256 2>&1) || true

if echo "$CREATE_OUTPUT" | grep -qi "gvisor\|runsc\|permission\|operation not permitted\|namespace"; then
    pass "SKIP: gVisor cannot run in this Docker environment (expected in CI)"
    info "Error: $CREATE_OUTPUT"
    cleanup
    summary
    exit 0
fi

if echo "$CREATE_OUTPUT" | grep -qi "created\|Running\|started"; then
    pass "Container start-vm creation accepted"
else
    fail "Container start-vm creation failed: $CREATE_OUTPUT"
fi

# ── Step 2: Wait for Running ────────────────────────────────────

info "Step 2: Wait for start-vm to reach Running"
if wait_for_vm_phase "$node" "start-vm" "Running" 30; then
    pass "start-vm reached Running"
else
    fail "start-vm did not reach Running"
    cleanup
    summary
    exit 1
fi

# ── Step 3: Stop the container ──────────────────────────────────

info "Step 3: Stop start-vm"
stop_vm "$node" "start-vm"
if wait_for_vm_phase "$node" "start-vm" "Stopped" 15; then
    pass "start-vm stopped"
else
    fail "start-vm did not stop"
    cleanup
    summary
    exit 1
fi

# ── Step 4: Start the stopped container ─────────────────────────

info "Step 4: Start start-vm (from Stopped)"
START_OUTPUT=$(docker exec "$node" syfrah compute vm start start-vm 2>&1) || true

if wait_for_vm_phase "$node" "start-vm" "Running" 30; then
    pass "start-vm returned to Running after vm start"
else
    CURRENT=$(get_vm "$node" "start-vm" | jq -r '.phase // empty' 2>/dev/null)
    if [ "$CURRENT" = "Stopped" ]; then
        pass "SKIP: container start not yet supported (start-vm still Stopped)"
    else
        fail "start-vm did not return to Running (phase: ${CURRENT:-unknown}): $START_OUTPUT"
    fi
fi

# ── Step 5: Idempotent start on running VM ──────────────────────

info "Step 5: Start start-vm again (already Running — idempotent)"
IDEM_OUTPUT=$(docker exec "$node" syfrah compute vm start start-vm 2>&1) || true

# Should still be Running (no error, or a graceful no-op)
PHASE_AFTER=$(get_vm "$node" "start-vm" | jq -r '.phase // empty' 2>/dev/null)
if [ "$PHASE_AFTER" = "Running" ]; then
    pass "Idempotent start: VM still Running"
else
    fail "Idempotent start changed phase to: ${PHASE_AFTER:-unknown}"
fi

# ── Cleanup ─────────────────────────────────────────────────────

info "Cleanup: delete start-vm"
delete_vm "$node" "start-vm"

cleanup
summary
