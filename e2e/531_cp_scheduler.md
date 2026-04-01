# E2E 531 — Control Plane Scheduler (filter-then-score)

## Objective
Verify the placement scheduler correctly filters and scores hypervisors
for VM placement decisions.

## Test Steps

### 1. Fallback to local when no gossip data
- Create scheduler with empty GossipCluster
- Schedule should return local fallback with `is_local_fallback=true`

### 2. Picks least-loaded hypervisor
- Two hypervisors: hv-1 (50% used), hv-2 (25% used)
- Scheduler should pick hv-2 (lower utilization = higher score)

### 3. Zone filtering
- Two hypervisors in different zones
- With `--zone az-2`, only hv-2 (in az-2) should be selected

### 4. Zone filter with no match returns error
- One hypervisor in az-1, request zone=az-99
- Error: "no hypervisor matches constraints: zone=az-99"

### 5. Capacity filtering
- hv-1 with 1 free vCPU, hv-2 with 8 free
- Request 4 vCPUs: only hv-2 passes filter

### 6. Excluded hypervisors are skipped
- Both hypervisors available, exclude hv-1
- Only hv-2 should be selected

### 7. Anti-affinity penalizes colocation
- Equal hypervisors, hv-1 has 3 existing VMs from same group
- hv-2 should be preferred

### 8. Non-Available state filtered
- hv-1 in Draining state, hv-2 Available
- Only hv-2 should be selected

## Pass Criteria
- All unit tests in `scheduler.rs` pass
- Scheduler correctly implements filter-then-score pipeline
- Fallback to local placement works when gossip is empty
