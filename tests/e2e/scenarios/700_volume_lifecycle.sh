#!/usr/bin/env bash
# Scenario 700: Volume lifecycle with real ZeroFS + Hetzner S3
#
# End-to-end test for the full volume lifecycle:
#   create -> list -> ZeroFS mount -> write -> reconnect -> persist check
#   -> attach -> detach -> delete -> verify gone
#
# Required env vars:
#   SYFRAH_S3_ACCESS_KEY   — S3 access key
#   SYFRAH_S3_SECRET_KEY   — S3 secret key
#   SYFRAH_S3_ENDPOINT     — S3 endpoint URL (e.g. https://fsn1.your-objectstorage.com)
#   SYFRAH_S3_BUCKET       — S3 bucket name

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$SCRIPT_DIR/lib.sh"

echo "── Volume Lifecycle: ZeroFS + S3 ──"

# ── Validate required env vars ───────────────────────────────

for var in SYFRAH_S3_ACCESS_KEY SYFRAH_S3_SECRET_KEY SYFRAH_S3_ENDPOINT SYFRAH_S3_BUCKET; do
    if [ -z "${!var:-}" ]; then
        fail "required env var $var is not set"
        summary
        exit 1
    fi
done

# ── State for cleanup ───────────────────────────────────────

VOL_NAME="e2e-vol-$$"
VOL_DIR="/tmp/syfrah/${VOL_NAME}"
MOUNT_POINT="/mnt/e2e-vol-$$"
NBD_DEV="/dev/nbd0"
ZEROFS_PID=""
CONTAINER_NAME="e2e-vol-lifecycle"
ORG="vol-test-org"
PROJECT="vol-test-project"
ENV_NAME="vol-test-env"
VM_NAME="e2e-test-vm"
TEST_STRING="syfrah-e2e-persistence-check-$$"

# ── Cleanup trap ─────────────────────────────────────────────

cleanup_volume_test() {
    info "Cleaning up volume test resources..."

    # Unmount if still mounted
    docker exec "$CONTAINER_NAME" umount "$MOUNT_POINT" 2>/dev/null || true

    # Disconnect NBD
    docker exec "$CONTAINER_NAME" nbd-client -d "$NBD_DEV" 2>/dev/null || true

    # Kill ZeroFS
    if [ -n "$ZEROFS_PID" ]; then
        docker exec "$CONTAINER_NAME" kill "$ZEROFS_PID" 2>/dev/null || true
    fi

    # Delete volume (best effort)
    docker exec "$CONTAINER_NAME" syfrah volume delete --name "$VOL_NAME" \
        --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" 2>/dev/null || true

    # Delete org hierarchy (best effort)
    docker exec "$CONTAINER_NAME" syfrah env delete \
        --org "$ORG" --project "$PROJECT" --name "$ENV_NAME" 2>/dev/null || true
    docker exec "$CONTAINER_NAME" syfrah project delete \
        --org "$ORG" --name "$PROJECT" 2>/dev/null || true
    docker exec "$CONTAINER_NAME" syfrah org delete --name "$ORG" 2>/dev/null || true

    # Remove temp dir
    docker exec "$CONTAINER_NAME" rm -rf "$VOL_DIR" 2>/dev/null || true
    docker exec "$CONTAINER_NAME" rm -rf "$MOUNT_POINT" 2>/dev/null || true

    # Standard cleanup (containers, network)
    cleanup 2>/dev/null || true
}

trap 'cleanup_volume_test' EXIT

# ── Step 0: Setup ────────────────────────────────────────────

create_network
start_node "$CONTAINER_NAME" "172.20.0.10"

# ── Step 1: Initialize mesh + Raft ───────────────────────────

info "Step 1: Initialize mesh + Raft on single node"
init_mesh "$CONTAINER_NAME" "172.20.0.10" "vol-node-1"

if docker exec "$CONTAINER_NAME" syfrah fabric status >/dev/null 2>&1; then
    pass "Step 1: mesh initialized, daemon responsive"
else
    fail "Step 1: daemon not responsive after init"
    summary
    exit 1
fi

# ── Step 2: Configure S3 storage backend ─────────────────────

info "Step 2: Configure storage backend with S3"
if docker exec "$CONTAINER_NAME" syfrah storage config set \
    --backend s3 \
    --endpoint "$SYFRAH_S3_ENDPOINT" \
    --bucket "$SYFRAH_S3_BUCKET" \
    --access-key "$SYFRAH_S3_ACCESS_KEY" \
    --secret-key "$SYFRAH_S3_SECRET_KEY" 2>&1; then
    pass "Step 2: S3 storage backend configured"
else
    fail "Step 2: failed to configure S3 storage backend"
    summary
    exit 1
fi

# ── Step 3: Create org / project / env ───────────────────────

info "Step 3: Create org/project/env hierarchy"
STEP3_OK=true

docker exec "$CONTAINER_NAME" syfrah org create --name "$ORG" 2>&1 || STEP3_OK=false
docker exec "$CONTAINER_NAME" syfrah project create \
    --org "$ORG" --name "$PROJECT" 2>&1 || STEP3_OK=false
docker exec "$CONTAINER_NAME" syfrah env create \
    --org "$ORG" --project "$PROJECT" --name "$ENV_NAME" 2>&1 || STEP3_OK=false

if $STEP3_OK; then
    pass "Step 3: org/project/env created"
else
    fail "Step 3: failed to create org/project/env"
    summary
    exit 1
fi

# ── Step 4: Create volume (10 GB) ───────────────────────────

info "Step 4: Create volume ${VOL_NAME} (10G)"
if docker exec "$CONTAINER_NAME" syfrah volume create \
    --name "$VOL_NAME" --size 10G \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" 2>&1; then
    pass "Step 4: volume created"
else
    fail "Step 4: failed to create volume"
    summary
    exit 1
fi

# ── Step 5: Verify volume in list ───────────────────────────

info "Step 5: Verify volume appears in list"
VOL_LIST=$(docker exec "$CONTAINER_NAME" syfrah volume list \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" --json 2>&1)

if echo "$VOL_LIST" | jq -e ".[] | select(.name == \"$VOL_NAME\")" >/dev/null 2>&1; then
    pass "Step 5: volume $VOL_NAME found in list"
else
    fail "Step 5: volume $VOL_NAME not found in list"
    debug "volume list output: $VOL_LIST"
fi

# ── Step 6: Start ZeroFS ────────────────────────────────────

info "Step 6: Start ZeroFS with S3 config"
docker exec "$CONTAINER_NAME" mkdir -p "$VOL_DIR"

docker exec "$CONTAINER_NAME" bash -c "cat > ${VOL_DIR}/zerofs.toml <<ZEOF
[storage]
backend = \"s3\"
endpoint = \"${SYFRAH_S3_ENDPOINT}\"
bucket = \"${SYFRAH_S3_BUCKET}\"
prefix = \"volumes/${VOL_NAME}\"
access_key = \"${SYFRAH_S3_ACCESS_KEY}\"
secret_key = \"${SYFRAH_S3_SECRET_KEY}\"

[nbd]
socket = \"${VOL_DIR}/zerofs.nbd.sock\"
size = \"10G\"
ZEOF"

docker exec -d "$CONTAINER_NAME" zerofs run -c "${VOL_DIR}/zerofs.toml"

# Wait for NBD socket to appear
SOCK_WAIT=0
while [ $SOCK_WAIT -lt 15 ]; do
    if docker exec "$CONTAINER_NAME" test -S "${VOL_DIR}/zerofs.nbd.sock" 2>/dev/null; then
        break
    fi
    sleep 1
    SOCK_WAIT=$((SOCK_WAIT + 1))
done

# Capture ZeroFS PID for cleanup
ZEROFS_PID=$(docker exec "$CONTAINER_NAME" pgrep -f "zerofs run" 2>/dev/null || echo "")

if docker exec "$CONTAINER_NAME" test -S "${VOL_DIR}/zerofs.nbd.sock" 2>/dev/null; then
    pass "Step 6: ZeroFS started, NBD socket exists"
else
    fail "Step 6: ZeroFS NBD socket not found after 15s"
    summary
    exit 1
fi

# ── Step 7: Connect NBD ─────────────────────────────────────

info "Step 7: Connect NBD device"
if docker exec "$CONTAINER_NAME" \
    nbd-client -unix "${VOL_DIR}/zerofs.nbd.sock" "$NBD_DEV" 2>&1; then
    pass "Step 7: NBD connected to $NBD_DEV"
else
    fail "Step 7: failed to connect NBD"
    summary
    exit 1
fi

# ── Step 8: Format, mount, write test data ──────────────────

info "Step 8: mkfs.ext4, mount, write test file"
docker exec "$CONTAINER_NAME" mkfs.ext4 -F "$NBD_DEV" 2>&1
docker exec "$CONTAINER_NAME" mkdir -p "$MOUNT_POINT"
docker exec "$CONTAINER_NAME" mount "$NBD_DEV" "$MOUNT_POINT"
docker exec "$CONTAINER_NAME" bash -c "echo '$TEST_STRING' > ${MOUNT_POINT}/testfile.txt"
docker exec "$CONTAINER_NAME" sync
docker exec "$CONTAINER_NAME" umount "$MOUNT_POINT"

pass "Step 8: filesystem created, test file written, unmounted"

# ── Step 9: Disconnect NBD, stop ZeroFS ─────────────────────

info "Step 9: Disconnect NBD + stop ZeroFS"
docker exec "$CONTAINER_NAME" nbd-client -d "$NBD_DEV" 2>&1
if [ -n "$ZEROFS_PID" ]; then
    docker exec "$CONTAINER_NAME" kill "$ZEROFS_PID" 2>/dev/null || true
    sleep 2
fi

pass "Step 9: NBD disconnected, ZeroFS stopped"

# ── Step 10: Reconnect, verify persistence ──────────────────

info "Step 10: Reconnect ZeroFS + NBD, verify data persists"
docker exec -d "$CONTAINER_NAME" zerofs run -c "${VOL_DIR}/zerofs.toml"

SOCK_WAIT=0
while [ $SOCK_WAIT -lt 15 ]; do
    if docker exec "$CONTAINER_NAME" test -S "${VOL_DIR}/zerofs.nbd.sock" 2>/dev/null; then
        break
    fi
    sleep 1
    SOCK_WAIT=$((SOCK_WAIT + 1))
done

ZEROFS_PID=$(docker exec "$CONTAINER_NAME" pgrep -f "zerofs run" 2>/dev/null || echo "")

docker exec "$CONTAINER_NAME" \
    nbd-client -unix "${VOL_DIR}/zerofs.nbd.sock" "$NBD_DEV" 2>&1
docker exec "$CONTAINER_NAME" mount "$NBD_DEV" "$MOUNT_POINT"

PERSISTED=$(docker exec "$CONTAINER_NAME" cat "${MOUNT_POINT}/testfile.txt" 2>&1 || echo "")

if [ "$PERSISTED" = "$TEST_STRING" ]; then
    pass "Step 10: test file persists after ZeroFS restart"
else
    fail "Step 10: test file content mismatch (expected '$TEST_STRING', got '$PERSISTED')"
fi

docker exec "$CONTAINER_NAME" umount "$MOUNT_POINT"
docker exec "$CONTAINER_NAME" nbd-client -d "$NBD_DEV" 2>&1
if [ -n "$ZEROFS_PID" ]; then
    docker exec "$CONTAINER_NAME" kill "$ZEROFS_PID" 2>/dev/null || true
    sleep 1
fi

# ── Step 11: Attach volume to VM ─────────────────────────────

info "Step 11: Attach volume to VM"
if docker exec "$CONTAINER_NAME" syfrah volume attach \
    --name "$VOL_NAME" --vm "$VM_NAME" \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" 2>&1; then
    pass "Step 11: volume attached to $VM_NAME"
else
    fail "Step 11: failed to attach volume to VM"
fi

# Verify attached status
VOL_INFO=$(docker exec "$CONTAINER_NAME" syfrah volume list \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" --json 2>&1)
ATTACHED_TO=$(echo "$VOL_INFO" | jq -r ".[] | select(.name == \"$VOL_NAME\") | .attached_to // empty" 2>/dev/null || echo "")

if [ "$ATTACHED_TO" = "$VM_NAME" ]; then
    pass "Step 11: volume shows attached to $VM_NAME"
else
    debug "volume info: $VOL_INFO"
    fail "Step 11: volume attached_to field mismatch (expected '$VM_NAME', got '$ATTACHED_TO')"
fi

# ── Step 12: Detach volume ───────────────────────────────────

info "Step 12: Detach volume"
if docker exec "$CONTAINER_NAME" syfrah volume detach \
    --name "$VOL_NAME" \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" 2>&1; then
    pass "Step 12: volume detached"
else
    fail "Step 12: failed to detach volume"
fi

# Verify detached
VOL_INFO=$(docker exec "$CONTAINER_NAME" syfrah volume list \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" --json 2>&1)
ATTACHED_TO=$(echo "$VOL_INFO" | jq -r ".[] | select(.name == \"$VOL_NAME\") | .attached_to // empty" 2>/dev/null || echo "")

if [ -z "$ATTACHED_TO" ]; then
    pass "Step 12: volume shows no attachment"
else
    fail "Step 12: volume still attached to '$ATTACHED_TO'"
fi

# ── Step 13: Delete volume ───────────────────────────────────

info "Step 13: Delete volume"
if docker exec "$CONTAINER_NAME" syfrah volume delete \
    --name "$VOL_NAME" \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" 2>&1; then
    pass "Step 13: volume deleted"
else
    fail "Step 13: failed to delete volume"
fi

# ── Step 14: Verify volume gone ──────────────────────────────

info "Step 14: Verify volume absent from list"
VOL_LIST=$(docker exec "$CONTAINER_NAME" syfrah volume list \
    --org "$ORG" --project "$PROJECT" --env "$ENV_NAME" --json 2>&1)

if echo "$VOL_LIST" | jq -e ".[] | select(.name == \"$VOL_NAME\")" >/dev/null 2>&1; then
    fail "Step 14: volume $VOL_NAME still present after delete"
else
    pass "Step 14: volume $VOL_NAME no longer in list"
fi

# ── Summary ──────────────────────────────────────────────────

summary
