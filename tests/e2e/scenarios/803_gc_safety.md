# Test: GC safety under snapshot/restore/delete churn

## Objective

Validate that the garbage collector never deletes SST files still referenced
by an active volume or snapshot.  This is a GA-gate test: it must prove that
no premature SST deletion can occur under realistic churn.

## Prerequisites

- Storage layer daemon running with S3 backend configured
- `syfrah` binary in PATH
- Sufficient S3 quota for ~20 volumes/snapshots

## Steps

### 1. Create a volume and write data

Create volume V1 (10 GB), attach it, write a known data pattern (1 GB),
and flush to S3 so SST files exist in the bucket.

### 2. Create snapshot S1

```bash
syfrah volume snapshot create s1 --volume v1
```

Record the SST file list from S3 after the snapshot.

### 3. Restore snapshot S1 to a new volume V2

```bash
syfrah volume snapshot restore s1 --target-volume v2
```

V2 now shares SST files with V1 via the snapshot's manifest.

### 4. Delete snapshot S1

```bash
syfrah volume snapshot delete s1 --yes
```

SST files referenced by S1 should move to `pending_gc` but must NOT be
deleted yet — V2 still references them.

### 5. Verify shared SSTs are NOT deleted

List S3 objects and confirm every SST that V2 references is still present.

### 6. Delete V1

```bash
syfrah volume delete v1 --yes
```

Additional SSTs that were exclusive to V1 become eligible for GC.

### 7. Run GC cycle

Trigger a GC pass (or wait for the periodic cycle) and verify:
- SSTs still referenced by V2 are **not** deleted.
- SSTs that were exclusive to V1 and no longer referenced by any
  volume or snapshot **are** deleted.

### 8. Rapid churn: create 10 snapshots, restore 5, delete 8

```bash
for i in $(seq 1 10); do
    syfrah volume snapshot create "churn-s${i}" --volume v2
done
for i in 1 3 5 7 9; do
    syfrah volume snapshot restore "churn-s${i}" --target-volume "churn-v${i}"
done
for i in 1 2 3 5 6 7 8 10; do
    syfrah volume snapshot delete "churn-s${i}" --yes
done
```

Verify that SST refcounts are correct: remaining snapshots (s4, s9) and
restored volumes (churn-v1, churn-v3, churn-v5, churn-v7, churn-v9) all
have their referenced SSTs intact.

### 9. Verify V2 is still readable after all GC

Attach V2, read back the data pattern written in step 1, and confirm
integrity (checksum match).

### 10. WAL retention for active snapshots

Verify that WAL segments required by active snapshots (s4, s9) are
retained and not pruned by the GC. Confirm WAL segments for deleted
snapshots have been cleaned up.

## Expected results

- No SST file is deleted while any volume or snapshot still references it
- GC deletes only truly unreferenced SSTs
- V2 data remains fully readable after all churn and GC cycles
- Refcounts converge to correct values after rapid create/restore/delete
- WAL segments are retained for active snapshots and pruned for deleted ones

## Failure criteria

- Any volume read returns corrupted or missing data after GC
- An SST file is deleted while still referenced (premature deletion)
- Refcount mismatch between expected and actual values
- WAL segments missing for an active snapshot
- GC fails to delete unreferenced SSTs (leak, not safety — still flagged)
