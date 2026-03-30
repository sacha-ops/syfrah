# Test: Project CLI commands

## Objective

Verify that `syfrah project create`, `syfrah project list`, and `syfrah project delete` work correctly end-to-end, including error messages for invalid inputs and duplicate/missing resources.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- No existing orgs or projects in the org database (`~/.syfrah/org.redb` does not exist or is empty)

## Steps

### 1. Create an organization

```bash
syfrah org create test-org
```

**Expected**: output contains `Organization 'test-org' created.`

### 2. Create a project under the org

```bash
syfrah project create backend --org test-org
```

**Expected**: output contains `Project 'backend' created in organization 'test-org'.`

### 3. Create a second project

```bash
syfrah project create frontend --org test-org
```

**Expected**: output contains `Project 'frontend' created in organization 'test-org'.`

### 4. List projects (all)

```bash
syfrah project list
```

**Expected**: table output showing both `backend` and `frontend` with org `test-org`.

### 5. List projects filtered by org

```bash
syfrah project list --org test-org
```

**Expected**: table output showing both projects.

### 6. List projects as JSON

```bash
syfrah project list --org test-org --json
```

**Expected**: valid JSON array with 2 project objects, each containing `name`, `org`, and `created_at` fields.

### 7. Duplicate project is rejected

```bash
syfrah project create backend --org test-org
```

**Expected**: error message contains `project 'backend' already exists in org 'test-org'`. Exit code 1.

### 8. Project in nonexistent org is rejected

```bash
syfrah project create api --org nonexistent
```

**Expected**: error message contains `org 'nonexistent' not found`. Exit code 1.

### 9. Invalid project name is rejected

```bash
syfrah project create AB --org test-org
```

**Expected**: error about name validation (length or invalid characters). Exit code 1.

### 10. Delete a project

```bash
syfrah project delete frontend --org test-org --yes
```

**Expected**: output contains `Project 'frontend' deleted from org 'test-org'.`

### 11. Verify deletion

```bash
syfrah project list --org test-org
```

**Expected**: only `backend` appears in the list.

### 12. Delete nonexistent project is rejected

```bash
syfrah project delete nonexistent --org test-org --yes
```

**Expected**: error message contains `project 'nonexistent' not found in org 'test-org'`. Exit code 1.

### 13. Cleanup

```bash
syfrah project delete backend --org test-org --yes
syfrah org delete test-org --yes
```

**Expected**: both deletions succeed.

## Pass criteria

- All 13 steps complete without unexpected errors
- Error messages are actionable and include the resource names
- JSON output is valid and parseable
- Deletion of nonexistent resources produces clear error messages
