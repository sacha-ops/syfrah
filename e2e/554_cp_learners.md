# E2E: Non-Voter Nodes — Learner Scaling (#1073)

## Scope

Verify that nodes beyond the max voter limit (default 5) join as learners
automatically, and that promote/demote commands work correctly.

## Prerequisites

- Two-node cluster with `controlplane init` + `controlplane join`

## Test Steps

### 1. Verify auto-promotion on join (under voter limit)

```bash
# With default max_voters=5 and only 1 voter, joining should auto-promote
ssh root@37.27.12.205 "syfrah controlplane join"
# Response should indicate "joined_as_voter" since we're under the limit
sleep 5
ssh root@65.109.130.108 "syfrah controlplane members"
# Both nodes should be "voter"
```

### 2. Verify voter/learner role in status

```bash
ssh root@65.109.130.108 "syfrah controlplane status"
# Should show voter_count and learner_count
```

### 3. Demote a voter to learner

```bash
ssh root@65.109.130.108 "syfrah controlplane demote hv-eu-2"
sleep 3
ssh root@65.109.130.108 "syfrah controlplane members"
# hv-eu-2 should now show as "learner"
```

### 4. Promote learner back to voter

```bash
ssh root@65.109.130.108 "syfrah controlplane promote hv-eu-2"
sleep 3
ssh root@65.109.130.108 "syfrah controlplane members"
# hv-eu-2 should be "voter" again
```

### 5. Verify writes still work with mixed voter/learner

```bash
# Demote one node
ssh root@65.109.130.108 "syfrah controlplane demote hv-eu-2"
sleep 3
# Create an org (should work — leader is still a voter)
ssh root@65.109.130.108 "syfrah org create learner-test"
# Verify replication to the learner
ssh root@37.27.12.205 "syfrah org list"
# learner-test should be visible on the learner node too
```

## Expected Behavior

- Default max_voters = 5
- Nodes under the limit are auto-promoted to voter on join
- Nodes at/above the limit stay as learner
- `controlplane promote <node>` promotes learner to voter
- `controlplane demote <node>` demotes voter to learner
- Status/members show voter/learner roles correctly
- Learners receive replicated data but don't vote

## Pass Criteria

- Auto-promotion works for nodes under the limit
- Promote/demote commands succeed
- Members output shows correct roles after promote/demote
- Data replicates to learner nodes
