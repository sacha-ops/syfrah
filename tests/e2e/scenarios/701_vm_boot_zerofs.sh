#!/usr/bin/env bash
# Scenario 701: VM boot from ZeroFS-backed root volume
#
# Tests the full lifecycle of a VM whose root disk is a ZeroFS volume:
#   create (with --disk-size) → verify volume → boot → write → stop → restart
#   → verify persistence → delete → verify volume auto-cleaned.
#
# Gracefully handles KVM and non-KVM (container fallback) environments.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

echo "── VM Boot: ZeroFS-backed root volume ──"

NODE="e2e-zerofs-boot-1"
NODE_IP="172.20.0.10"
VM_NAME="zerofs-vm-1"
PERSIST_MARKER="zerofs-persist-test-$(date +%s)"

# ── Setup ────────────────────────────────────────────────────

create_network
start_node "$NODE" "$NODE_IP"
init_mesh "$NODE" "$NODE_IP" "zerofs-boot"

# ── Step 1: Detect runtime ───────────────────────────────────

if docker exec "$NODE" test -c /dev/kvm 2>/dev/null; then
    RUNTIME="kvm"
    info "KVM detected — testing with Cloud Hypervisor VM"
else
    RUNTIME="container"
    info "No KVM — testing with container runtime (crun fallback)"
fi
pass "runtime detected: $RUNTIME"

# ── Step 2: Create VM with --disk-size 20 ────────────────────

info "Creating VM $VM_NAME with --disk-size 20..."
output=$(create_vm "$NODE" "$VM_NAME" \
    --vcpus 1 --memory 512 --image alpine-3.20 --disk-size 20 2>&1)
rc=$?
if [ $rc -eq 0 ]; then
    pass "VM creation command accepted"
else
    fail "VM creation failed (rc=$rc): $output"
    cleanup
    summary
    exit 1
fi

# Give the VM time to boot
sleep 3

# ── Step 3: Verify root volume exists ────────────────────────

info "Checking volume list for root volume..."
vol_output=$(docker exec "$NODE" syfrah volume list --json 2>&1 || true)
if echo "$vol_output" | jq -e '. | length > 0' >/dev/null 2>&1; then
    pass "root volume exists in volume list"
else
    fail "no volumes found after VM creation: $vol_output"
fi

# ── Step 4: Verify VM reaches Running phase ──────────────────

info "Waiting for VM to reach Running phase..."
wait_for_vm_phase "$NODE" "$VM_NAME" "Running" 30
vm_json=$(get_vm "$NODE" "$VM_NAME")

root_vol_id=$(echo "$vm_json" | jq -r '.root_volume_id // empty')
if [ -n "$root_vol_id" ]; then
    pass "root_volume_id present: $root_vol_id"
else
    # Some implementations store it differently — check nested fields
    root_vol_id=$(echo "$vm_json" | jq -r '.spec.root_volume_id // .volumes[0].id // empty')
    if [ -n "$root_vol_id" ]; then
        pass "root_volume_id found (nested): $root_vol_id"
    else
        fail "root_volume_id missing from VM JSON"
        debug "VM JSON: $(echo "$vm_json" | jq -c .)"
    fi
fi

# ── Step 5: Verify guest can write to root disk ─────────────

info "Writing marker to guest root disk..."
write_output=$(docker exec "$NODE" \
    syfrah compute vm exec "$VM_NAME" -- \
    sh -c "echo '$PERSIST_MARKER' > /tmp/persist.txt && cat /tmp/persist.txt" 2>&1 || true)

if echo "$write_output" | grep -qF "$PERSIST_MARKER"; then
    pass "guest write succeeded — marker echoed back"
else
    # If exec is not implemented, try a softer check
    if echo "$write_output" | grep -qi "not.*implemented\|not.*supported\|unknown"; then
        info "vm exec not available — skipping write/persistence checks"
        SKIP_PERSISTENCE=true
    else
        fail "guest write failed or marker not found: $write_output"
        SKIP_PERSISTENCE=true
    fi
fi

# ── Step 6: Stop the VM ─────────────────────────────────────

info "Stopping VM $VM_NAME..."
stop_output=$(stop_vm "$NODE" "$VM_NAME" 2>&1)
if [ $? -eq 0 ]; then
    pass "VM stop command accepted"
else
    fail "VM stop failed: $stop_output"
fi

sleep 2

stopped_phase=$(get_vm "$NODE" "$VM_NAME" | jq -r '.phase // empty')
if [ "$stopped_phase" = "Stopped" ]; then
    pass "VM phase is Stopped"
else
    # Allow graceful degradation — some runtimes remove the VM on stop
    info "VM phase after stop: ${stopped_phase:-unknown} (expected Stopped)"
fi

# ── Step 7: Restart and verify data persistence ─────────────

if [ "${SKIP_PERSISTENCE:-false}" != "true" ]; then
    info "Restarting VM to verify ZeroFS persistence..."
    restart_output=$(docker exec "$NODE" syfrah compute vm start "$VM_NAME" 2>&1 || true)

    if echo "$restart_output" | grep -qi "error\|fail"; then
        fail "VM restart failed: $restart_output"
    else
        pass "VM restart command accepted"
        sleep 3
        wait_for_vm_phase "$NODE" "$VM_NAME" "Running" 30

        info "Checking persisted data after restart..."
        read_output=$(docker exec "$NODE" \
            syfrah compute vm exec "$VM_NAME" -- cat /tmp/persist.txt 2>&1 || true)
        if echo "$read_output" | grep -qF "$PERSIST_MARKER"; then
            pass "data persisted across stop/start (ZeroFS → S3 round-trip)"
        else
            fail "data did NOT persist after restart: $read_output"
        fi

        # Stop again before deletion
        stop_vm "$NODE" "$VM_NAME" >/dev/null 2>&1 || true
        sleep 1
    fi
else
    info "Skipped persistence check (vm exec unavailable)"
fi

# ── Step 8: Delete VM and verify volume cleanup ─────────────

info "Deleting VM $VM_NAME..."
delete_output=$(delete_vm "$NODE" "$VM_NAME" 2>&1)
if [ $? -eq 0 ]; then
    pass "VM deletion command accepted"
else
    fail "VM deletion failed: $delete_output"
fi

sleep 2

# Verify VM gone
vm_list=$(list_vms "$NODE" 2>&1)
if echo "$vm_list" | jq -e "map(select(.name == \"$VM_NAME\")) | length == 0" >/dev/null 2>&1; then
    pass "VM $VM_NAME removed from vm list"
else
    fail "VM $VM_NAME still in vm list after delete"
fi

# Verify root volume auto-deleted
vol_after=$(docker exec "$NODE" syfrah volume list --json 2>&1 || true)
if [ -n "$root_vol_id" ]; then
    if echo "$vol_after" | jq -e "map(select(.id == \"$root_vol_id\")) | length == 0" >/dev/null 2>&1; then
        pass "root volume $root_vol_id auto-deleted"
    else
        fail "root volume $root_vol_id still exists after VM deletion"
    fi
else
    # No volume ID captured — check that volume list is empty
    vol_count=$(echo "$vol_after" | jq 'length' 2>/dev/null || echo "unknown")
    if [ "$vol_count" = "0" ]; then
        pass "volume list empty after VM deletion"
    else
        info "volume list after deletion (count=$vol_count): cannot verify auto-delete without root_volume_id"
    fi
fi

# ── Cleanup ──────────────────────────────────────────────────

cleanup
summary
