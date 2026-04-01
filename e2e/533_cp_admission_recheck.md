# E2E 533 — Scheduler Forge Admission Recheck

## Objective
Verify the scheduler retry loop: pick -> Forge recheck -> retry if rejected.

## Test Steps

### 1. Admission accepted on first try
- Two available hypervisors, admission check always succeeds
- Scheduler picks top scorer, returns immediately

### 2. First rejection triggers retry on second hypervisor
- Two hypervisors, first admission check rejects, second accepts
- Verify second hypervisor is selected

### 3. All rejections exhaust retries
- Both hypervisors reject admission
- Error: "all N hypervisors rejected admission after M retries"

### 4. Local fallback skips admission check
- No gossip data -> local fallback
- Admission check function is never called

### 5. AdmissionResult type works correctly
- AdmissionResult::Accepted.is_accepted() == true
- AdmissionResult::Rejected.is_accepted() == false

## Pass Criteria
- Retry loop correctly excludes rejected hypervisors
- Max 3 retries (configurable via MAX_ADMISSION_RETRIES)
- Local fallback bypasses admission recheck
- All 47 unit tests pass
