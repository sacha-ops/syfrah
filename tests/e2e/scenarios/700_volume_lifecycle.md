# Test: Volume lifecycle with real ZeroFS + Hetzner S3

## Objective

End-to-end validation of the full volume lifecycle: create, mount via ZeroFS/NBD,
write data, persist across reconnect, attach/detach to a VM, and delete.

- Volumes backed by S3 object storage (Hetzner S3 or compatible)
- ZeroFS provides the block-device layer over S3
- NBD exposes the block device to the local kernel

## Prerequisites

- A test server with `syfrah` installed and in PATH
- `zerofs` binary available in PATH
- `nbd-client` installed and `nbd` kernel module loaded
- `/dev/nbd0` available (unused)
- S3 credentials provided via environment variables:
  - `SYFRAH_S3_ACCESS_KEY`
  - `SYFRAH_S3_SECRET_KEY`
  - `SYFRAH_S3_ENDPOINT` (e.g. `https://fsn1.your-objectstorage.com`)
  - `SYFRAH_S3_BUCKET`
- KVM support or container runtime fallback for attach/detach steps

## Steps

### 1. Initialize mesh + Raft on single node

```bash
syfrah fabric init --name vol-test-mesh --node-name vol-node-1 --endpoint 172.20.0.10:51820
```

Wait for daemon to be ready.

### 2. Configure storage backend with S3

```bash
syfrah storage config set \
  --backend s3 \
  --endpoint "$SYFRAH_S3_ENDPOINT" \
  --bucket "$SYFRAH_S3_BUCKET" \
  --access-key "$SYFRAH_S3_ACCESS_KEY" \
  --secret-key "$SYFRAH_S3_SECRET_KEY"
```

### 3. Create org / project / env

```bash
syfrah org create --name vol-test-org
syfrah project create --org vol-test-org --name vol-test-project
syfrah env create --org vol-test-org --project vol-test-project --name vol-test-env
```

### 4. Create volume (10 GB)

```bash
syfrah volume create --name e2e-vol-1 --size 10G \
  --org vol-test-org --project vol-test-project --env vol-test-env
```

### 5. Verify volume in list

```bash
syfrah volume list --org vol-test-org --project vol-test-project --env vol-test-env --json
```

- Verify: `e2e-vol-1` appears with size 10G and status Available

### 6. Start ZeroFS manually

Generate a zerofs config pointing at the volume's S3 prefix:

```bash
cat > /tmp/syfrah/e2e-vol-1/zerofs.toml <<EOF
[storage]
backend = "s3"
endpoint = "$SYFRAH_S3_ENDPOINT"
bucket = "$SYFRAH_S3_BUCKET"
prefix = "volumes/e2e-vol-1"
access_key = "$SYFRAH_S3_ACCESS_KEY"
secret_key = "$SYFRAH_S3_SECRET_KEY"

[nbd]
socket = "/tmp/syfrah/e2e-vol-1/zerofs.nbd.sock"
size = "10G"
EOF

zerofs run -c /tmp/syfrah/e2e-vol-1/zerofs.toml &
ZEROFS_PID=$!
```

Wait for socket to appear.

### 7. Connect NBD

```bash
nbd-client -unix /tmp/syfrah/e2e-vol-1/zerofs.nbd.sock /dev/nbd0
```

### 8. Format, mount, write test data

```bash
mkfs.ext4 /dev/nbd0
mkdir -p /mnt/e2e-vol
mount /dev/nbd0 /mnt/e2e-vol
echo "syfrah-e2e-persistence-check" > /mnt/e2e-vol/testfile.txt
sync
umount /mnt/e2e-vol
```

### 9. Disconnect NBD, stop ZeroFS

```bash
nbd-client -d /dev/nbd0
kill $ZEROFS_PID && wait $ZEROFS_PID
```

### 10. Reconnect ZeroFS + NBD, verify persistence

```bash
zerofs run -c /tmp/syfrah/e2e-vol-1/zerofs.toml &
ZEROFS_PID=$!
# wait for socket
nbd-client -unix /tmp/syfrah/e2e-vol-1/zerofs.nbd.sock /dev/nbd0
mount /dev/nbd0 /mnt/e2e-vol
```

- Verify: `/mnt/e2e-vol/testfile.txt` exists and contains `syfrah-e2e-persistence-check`

```bash
umount /mnt/e2e-vol
nbd-client -d /dev/nbd0
kill $ZEROFS_PID && wait $ZEROFS_PID
```

### 11. Attach volume to VM

```bash
syfrah volume attach --name e2e-vol-1 --vm e2e-test-vm \
  --org vol-test-org --project vol-test-project --env vol-test-env
```

- Verify: volume status is Attached, attached-to shows `e2e-test-vm`

### 12. Detach volume

```bash
syfrah volume detach --name e2e-vol-1 \
  --org vol-test-org --project vol-test-project --env vol-test-env
```

- Verify: volume status is Available, no attached-to

### 13. Delete volume

```bash
syfrah volume delete --name e2e-vol-1 \
  --org vol-test-org --project vol-test-project --env vol-test-env
```

### 14. Verify volume gone from list

```bash
syfrah volume list --org vol-test-org --project vol-test-project --env vol-test-env --json
```

- Verify: `e2e-vol-1` does NOT appear in the list

## Expected Results

| Step | Assertion |
|------|-----------|
| 1 | Mesh initialized, daemon responds to `fabric status` |
| 2 | Storage backend configured without error |
| 3 | Org, project, env created |
| 4 | Volume created, returns success |
| 5 | Volume appears in list with correct size |
| 6 | ZeroFS starts, NBD socket exists |
| 7 | NBD device connected |
| 8 | Filesystem created, test file written |
| 9 | NBD disconnected, ZeroFS stopped cleanly |
| 10 | Test file persists across ZeroFS restart |
| 11 | Volume attached to VM |
| 12 | Volume detached |
| 13 | Volume deleted |
| 14 | Volume absent from list |

## Cleanup

- Unmount `/mnt/e2e-vol` if still mounted
- Disconnect `/dev/nbd0` if still connected
- Kill ZeroFS process if still running
- Delete the volume if it still exists
- Delete env, project, org
- Remove `/tmp/syfrah/e2e-vol-1/`
