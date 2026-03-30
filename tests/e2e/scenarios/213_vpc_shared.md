# Scenario 213: Shared VPC — attach/detach

## Objective

Validate that shared VPCs can be created, projects can be attached and detached,
and that non-shared VPCs reject attachment attempts.

## Preconditions

- `syfrah` binary is built and available on PATH
- No pre-existing org state (clean database)

## Steps

### 1. Create org and projects

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah project create frontend --org acme
syfrah project create data --org acme
```

**Expected**: All succeed with confirmation messages.

### 2. Create a shared VPC

```bash
syfrah vpc create platform --org acme --shared --cidr 10.100.0.0/16
```

**Expected**: VPC created with `shared: true`, VNI assigned.

### 3. Create a non-shared (project) VPC

```bash
syfrah vpc create private --org acme --project backend --cidr 10.1.0.0/16
```

**Expected**: VPC created with `shared: false`.

### 4. Attach projects to the shared VPC

```bash
syfrah vpc attach platform --project acme/backend
syfrah vpc attach platform --project acme/frontend
```

**Expected**: Both succeed with confirmation messages.

### 5. Verify attachments via list

```bash
syfrah vpc list --org acme
```

**Expected**: Both `platform` (shared) and `private` (non-shared) appear.

### 6. Attempt double-attach (should fail)

```bash
syfrah vpc attach platform --project acme/backend
```

**Expected**: Error — project already attached.

### 7. Attempt attach to non-shared VPC (should fail)

```bash
syfrah vpc attach private --project acme/frontend
```

**Expected**: Error — VPC is not shared.

### 8. Detach a project

```bash
syfrah vpc detach platform --project acme/backend
```

**Expected**: Success.

### 9. Attempt detach of non-attached project (should fail)

```bash
syfrah vpc detach platform --project acme/data
```

**Expected**: Error — project not attached.

### 10. Cleanup

```bash
syfrah vpc detach platform --project acme/frontend
syfrah vpc delete platform --yes
syfrah vpc delete private --yes
syfrah project delete data --org acme --yes
syfrah project delete frontend --org acme --yes
syfrah project delete backend --org acme --yes
syfrah org delete acme --yes
```

**Expected**: All resources cleaned up without errors.

## Pass criteria

- Shared VPC creation sets `shared: true`
- Attach/detach work for shared VPCs
- Non-shared VPCs reject attachment
- Double-attach is rejected
- Detach of non-attached project is rejected
- Full lifecycle (create, attach, detach, delete) completes cleanly
