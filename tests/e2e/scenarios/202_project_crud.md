# Test: Project CRUD Operations

## Objective

Verify that projects can be created, listed, and deleted within organizations, with proper validation and cascade protection.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is running
- No pre-existing orgs (clean state)

## Steps

### 1. Create an organization

```bash
syfrah org create test-org
```

Expected: success, org "test-org" created.

### 2. Create a project

```bash
syfrah project create backend --org test-org
```

Expected: success, project "backend" created in org "test-org".

### 3. Create a second project

```bash
syfrah project create frontend --org test-org
```

Expected: success, project "frontend" created in org "test-org".

### 4. Reject duplicate project name in same org

```bash
syfrah project create backend --org test-org
```

Expected: error — project "backend" already exists in org "test-org".

### 5. Allow same project name in different org

```bash
syfrah org create other-org
syfrah project create backend --org other-org
```

Expected: success — "backend" in "other-org" is independent from "backend" in "test-org".

### 6. Reject project with invalid name

```bash
syfrah project create "AB" --org test-org
```

Expected: error — name too short / invalid characters.

```bash
syfrah project create "My Project" --org test-org
```

Expected: error — invalid characters (uppercase, spaces).

### 7. Reject project creation in nonexistent org

```bash
syfrah project create backend --org nonexistent-org
```

Expected: error — org "nonexistent-org" not found.

### 8. List projects

```bash
syfrah project list --org test-org
```

Expected: shows "backend" and "frontend".

```bash
syfrah project list --org other-org
```

Expected: shows "backend" only.

### 9. Delete an empty project

```bash
syfrah project delete frontend --org test-org
```

Expected: success, project deleted.

```bash
syfrah project list --org test-org
```

Expected: shows "backend" only.

### 10. Reject deletion of project with environments

```bash
syfrah env create staging --project backend --org test-org
syfrah project delete backend --org test-org
```

Expected: error — project has environments and cannot be deleted.

### 11. Delete environment, then delete project

```bash
syfrah env destroy staging --project backend --org test-org
syfrah project delete backend --org test-org
```

Expected: success.

### 12. Reject deletion of nonexistent project

```bash
syfrah project delete nonexistent --org test-org
```

Expected: error — project not found.

## Cleanup

```bash
syfrah org delete other-org
syfrah org delete test-org
```

## Expected Results

| Step | Result |
|------|--------|
| 1 | Org created |
| 2 | Project created |
| 3 | Second project created |
| 4 | Duplicate rejected |
| 5 | Same name in different org allowed |
| 6 | Invalid names rejected |
| 7 | Nonexistent org rejected |
| 8 | List shows correct projects per org |
| 9 | Empty project deleted |
| 10 | Project with environments not deleted |
| 11 | After env removal, project deleted |
| 12 | Nonexistent project delete rejected |
