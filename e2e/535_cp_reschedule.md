# E2E 535 — Reschedule on Hypervisor Failure

## Objective
Verify that when gossip marks a hypervisor as Down, VMs with
`restart_on_failure: true` are rescheduled to another hypervisor.

## Test Steps

### 1. Normal VM rescheduled
- Two hypervisors (hv-1, hv-2), VM on hv-1 with restart_on_failure=true
- hv-1 goes Down
- Rescheduler moves VM to hv-2 with incremented generation

### 2. VM without restart_on_failure skipped
- VM with restart_on_failure=false on failed hypervisor
- Rescheduler skips it (RescheduleOutcome::Skipped)

### 3. VM with local storage marked Failed
- VM with has_local_storage=true
- Rescheduler marks it as Failed (cannot auto-reschedule)

### 4. VM with GPU marked Failed
- VM with has_gpu=true
- Rescheduler marks it as Failed

### 5. No available hypervisor
- Only one hypervisor (the failed one)
- Rescheduler marks VM as Failed (no target available)

### 6. Mixed VMs on failed hypervisor
- Multiple VMs with different properties
- Normal: rescheduled, storage: failed, GPU: failed, no-restart: skipped

### 7. Generation fence
- Rescheduled VM gets generation = old_generation + 1
- Old placement is stale (lower generation)

## Pass Criteria
- All 8 reschedule unit tests pass
- Normal VMs are rescheduled to available hypervisors
- Unsafe VMs (local storage, GPU) are marked Failed, not moved
- Generation incremented for fencing
