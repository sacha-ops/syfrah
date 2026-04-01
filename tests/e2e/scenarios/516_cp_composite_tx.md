# 516 — Control Plane: Composite placement transaction

## Goal
Verify that AllocateIp + CreateNic + PlaceVm can be submitted as a single atomic Raft log entry via the Composite command.

## Steps

### 1. Submit a composite placement command
```
StateMachineCommand::Composite {
    commands: vec![
        StateMachineCommand::AllocateIp { subnet_id: "web" },
        StateMachineCommand::CreateNic { vm_id: "vm-1", subnet_id: "web", ip: "10.1.0.3", mac: "02:00:..." },
        StateMachineCommand::PlaceVm { vm_id: "vm-1", hypervisor_id: "hv-eu-1", subnet_id: "web", ip: "10.1.0.3", mac: "02:00:...", generation: 1 },
    ],
}
```

### 2. Verify all three operations applied atomically
- IP 10.1.0.3 is allocated in the IPAM bitmap
- NIC record exists for vm-1
- Placement record exists for vm-1 on hv-eu-1

### 3. If any sub-command fails, stop and report error
```
# Example: AllocateIp succeeds but PlaceVm fails (no placement store)
# → First error is reported, remaining commands are not applied
```

## Expected Outcome
- All sub-commands in a Composite are applied in order within a single Raft log entry.
- If any sub-command fails, the remaining are skipped and the error is returned.
- The response contains results from each sub-command (Composite variant).
