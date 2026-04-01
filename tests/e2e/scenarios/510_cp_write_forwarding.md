# 510 — Control Plane: Route writes through Raft with leader forwarding

## Goal
Verify that mutations (org create, project create, env create, etc.) are routed through Raft when the control plane is initialized, and that reads are served directly from local redb.

## Preconditions
- Single-node mesh initialized (`syfrah fabric init`)
- Control plane bootstrapped (`syfrah controlplane init`)
- Daemon restarted to activate Raft

## Steps

### 1. Verify Raft is active
```bash
syfrah controlplane status
# Should show: State = Leader, Members = [node_id]
```

### 2. Create org through Raft
```bash
syfrah org create test-raft-org
# Should succeed — mutation goes through Raft state machine
```

### 3. Verify org was created (read from local redb)
```bash
syfrah org list
# Should show test-raft-org
```

### 4. Create project through Raft
```bash
syfrah project create backend --org test-raft-org
# Should succeed
```

### 5. Create environment through Raft
```bash
syfrah env create staging --project backend --org test-raft-org
# Should succeed
```

### 6. Verify full hierarchy
```bash
syfrah project list --org test-raft-org
# Should show backend
syfrah env list --project backend --org test-raft-org
# Should show staging
```

### 7. Delete through Raft
```bash
syfrah env destroy staging --project backend --org test-raft-org --yes
syfrah project delete backend --org test-raft-org --yes
syfrah org delete test-raft-org --yes
# All should succeed
```

### 8. Backward compatibility — no Raft
```bash
# On a node where controlplane is NOT initialized, the same commands
# should work via direct writes to redb (fallback path).
```

## Expected Outcome
- All mutations are applied atomically through the Raft state machine.
- All reads are served from local redb (no Raft round-trip).
- If Raft is not initialized, direct writes continue to work.
- The RaftOrgHandler transparently routes based on Raft availability.
