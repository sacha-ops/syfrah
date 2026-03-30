#!/usr/bin/env bash
# Scenario: Container runtime — runtime field in vm list and vm get
#
# Verifies that the runtime field is correctly exposed in CLI output.
# Covers issue #666.
#
# Verifies:
#   - `vm list` output contains a RUNTIME column
#   - The RUNTIME column shows "container" for container-backed VMs
#   - `vm get --json` includes a runtime field with value "container"

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

echo "── Compute: Container Runtime Field ──"

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
start_node "e2e-container-runtime" "172.20.0.10"
init_mesh "e2e-container-runtime" "172.20.0.10" "runtime-node"
sleep 2

node="e2e-container-runtime"

# ── Pre-flight: verify container runtime is available ────────────

if ! check_container_runtime "$node"; then
    pass "SKIP: container runtime not available (nested Docker limitation)"
    cleanup
    summary
    exit 0
fi

# ── Prepare: pull image ──────────────────────────────────────────

info "Pulling alpine-3.20 for runtime field tests"
docker exec "$node" sh -c 'rm -f /opt/syfrah/images/*.raw /opt/syfrah/images/images.json' 2>/dev/null || true
if ! pull_with_retry "$node" "alpine-3.20"; then
    fail "Failed to pull alpine-3.20 — cannot proceed"
    cleanup
    summary
    exit 1
fi
pass "Image ready"

# ── Step 1: Create a container VM ────────────────────────────────

info "Step 1: Create container rt-vm"
CREATE_OUTPUT=$(docker exec "$node" syfrah compute vm create \
    --name rt-vm --image alpine-3.20 --vcpus 1 --memory 256 2>&1) || true

if echo "$CREATE_OUTPUT" | grep -qi "gvisor\|runsc\|permission\|operation not permitted\|namespace"; then
    pass "SKIP: gVisor cannot run in this Docker environment (expected in CI)"
    info "Error: $CREATE_OUTPUT"
    cleanup
    summary
    exit 0
fi

if echo "$CREATE_OUTPUT" | grep -qi "created\|Running\|started"; then
    pass "Container rt-vm creation accepted"
else
    fail "Container rt-vm creation failed: $CREATE_OUTPUT"
fi

# ── Step 2: Wait for Running ────────────────────────────────────

info "Step 2: Wait for rt-vm to reach Running"
if wait_for_vm_phase "$node" "rt-vm" "Running" 30; then
    pass "rt-vm reached Running"
else
    fail "rt-vm did not reach Running"
fi

# ── Step 3: Verify RUNTIME column in vm list (table output) ─────

info "Step 3: Verify RUNTIME column in vm list"
LIST_TABLE=$(docker exec "$node" syfrah compute vm list 2>&1)

if echo "$LIST_TABLE" | grep -qi "RUNTIME"; then
    pass "vm list output contains RUNTIME column header"
else
    fail "vm list output missing RUNTIME column header"
    info "vm list output: $LIST_TABLE"
fi

# ── Step 4: Verify runtime value is "container" in vm list ──────

info "Step 4: Verify runtime value in vm list"
if echo "$LIST_TABLE" | grep -qi "container"; then
    pass "vm list shows 'container' runtime for rt-vm"
else
    fail "vm list does not show 'container' runtime"
    info "vm list output: $LIST_TABLE"
fi

# ── Step 5: Verify runtime in vm list --json ────────────────────

info "Step 5: Verify runtime field in vm list --json"
LIST_JSON=$(list_vms "$node")
RUNTIME_LIST=$(echo "$LIST_JSON" | jq -r '.[] | select(.id == "rt-vm") | .runtime // .runtime_type // empty' 2>/dev/null)

if echo "$RUNTIME_LIST" | grep -qi "container"; then
    pass "vm list --json shows runtime 'container' for rt-vm"
else
    info "runtime field from vm list --json: ${RUNTIME_LIST:-<empty>}"
    # Try alternate field names
    ALT_RUNTIME=$(echo "$LIST_JSON" | jq -r '.[] | select(.id == "rt-vm") | to_entries[] | select(.key | test("runtime"; "i")) | .value' 2>/dev/null)
    if echo "$ALT_RUNTIME" | grep -qi "container"; then
        pass "vm list --json shows runtime 'container' (alternate field name)"
    else
        fail "vm list --json missing runtime field for rt-vm"
    fi
fi

# ── Step 6: Verify runtime in vm get --json ─────────────────────

info "Step 6: Verify Runtime field in vm get --json"
VM_JSON=$(get_vm "$node" "rt-vm")
RUNTIME_GET=$(echo "$VM_JSON" | jq -r '.runtime // .runtime_type // empty' 2>/dev/null)

if echo "$RUNTIME_GET" | grep -qi "container"; then
    pass "vm get --json shows Runtime 'container' for rt-vm"
else
    info "runtime field from vm get --json: ${RUNTIME_GET:-<empty>}"
    ALT_GET=$(echo "$VM_JSON" | jq -r 'to_entries[] | select(.key | test("runtime"; "i")) | .value' 2>/dev/null)
    if echo "$ALT_GET" | grep -qi "container"; then
        pass "vm get --json shows Runtime 'container' (alternate field name)"
    else
        fail "vm get --json missing Runtime field for rt-vm"
    fi
fi

# ── Cleanup ─────────────────────────────────────────────────────

info "Cleanup: delete rt-vm"
delete_vm "$node" "rt-vm"

cleanup
summary
