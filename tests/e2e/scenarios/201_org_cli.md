# Test: Org CLI — Create, List, Delete organizations

## Objective

Verify that the `syfrah org` CLI commands correctly create, list, and delete
organizations with proper validation and error handling.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- No existing organizations in the store (clean state)

## Steps

### 1. Create an organization

```bash
syfrah org create acme
```

**Expected output:**
```
Organization 'acme' created.
```

### 2. Create a second organization

```bash
syfrah org create beta-corp
```

**Expected output:**
```
Organization 'beta-corp' created.
```

### 3. List organizations (table format)

```bash
syfrah org list
```

**Expected output:**
- Header row with NAME and CREATED columns
- Two rows: `acme` and `beta-corp`, sorted alphabetically
- Created dates in YYYY-MM-DD format

### 4. List organizations (JSON format)

```bash
syfrah org list --json
```

**Expected output:**
- Valid JSON array with 2 objects
- Each object has `name` (string) and `created_at` (number) fields

### 5. Attempt to create a duplicate organization

```bash
syfrah org create acme
```

**Expected:** Exit code 1, error message: `org 'acme' already exists`

### 6. Attempt to create an org with invalid name (too short)

```bash
syfrah org create ab
```

**Expected:** Exit code 1, error message containing `must be between 3 and 63 characters`

### 7. Attempt to create an org with invalid name (uppercase)

```bash
syfrah org create MyOrg
```

**Expected:** Exit code 1, error message containing `must be lowercase`

### 8. Attempt to create an org with invalid name (leading hyphen)

```bash
syfrah org create -bad-name
```

**Expected:** Exit code 1, error message containing `must start and end with an alphanumeric character`

### 9. Delete an organization (with --yes)

```bash
syfrah org delete beta-corp --yes
```

**Expected output:**
```
Organization 'beta-corp' deleted.
```

### 10. Verify deletion

```bash
syfrah org list --json
```

**Expected:** JSON array with 1 object (`acme` only)

### 11. Attempt to delete a non-existent organization

```bash
syfrah org delete nope --yes
```

**Expected:** Exit code 1, error message: `org 'nope' not found`

### 12. Clean up

```bash
syfrah org delete acme --yes
```

**Expected output:**
```
Organization 'acme' deleted.
```

### 13. Verify empty state

```bash
syfrah org list
```

**Expected output:**
```
(no organizations)
```

## Pass criteria

- All commands exit 0 on success, 1 on error
- Validation rejects invalid names with actionable messages
- Duplicate creation is prevented
- Deletion of non-existent org is rejected
- List shows correct table/JSON output
- State persists across commands (redb)
