# Test: Forge capacity reporting via gossip

## Objective

Verify that hypervisor capacity is reported via the gossip/announcement mechanism in the fabric mesh. The `HypervisorReport` is carried as an optional field in `PeerRecord` and is advisory (telemetry only, not source of truth).

## Prerequisites

- Two test servers with `syfrah` installed and in PATH
- Both running syfrah daemons connected in a mesh

## Steps

### 1. Initialize mesh on both nodes

```bash
# Server 1
ssh root@server1 "syfrah fabric stop 2>/dev/null; sleep 2; rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json"
ssh root@server1 "syfrah fabric init --name test --node-name n1 --endpoint SERVER1_IP:51820 --region eu-west --zone az-1 && sleep 2 && syfrah fabric peering start --pin 1234"

# Server 2
ssh root@server2 "syfrah fabric stop 2>/dev/null; sleep 2; rm -rf ~/.syfrah/*.redb ~/.syfrah/state.json"
ssh root@server2 "syfrah fabric join SERVER1_IP --pin 1234 --node-name n2 --endpoint SERVER2_IP:51820 --region eu-west --zone az-2"
sleep 3
```

### 2. Register hypervisors on both

```bash
ssh root@server1 "syfrah hypervisor register --region eu-west --zone az-1 && syfrah hypervisor enable n1"
ssh root@server2 "syfrah hypervisor register --region eu-west --zone az-2 && syfrah hypervisor enable n2"
```

### 3. Verify gossip carries hypervisor report

The `HypervisorReport` is an optional field on `PeerRecord`. When a hypervisor is registered and enabled, the peer announcement includes:
- `hypervisor_id`
- `region`, `zone`, `state`
- `allocatable_vcpus`, `used_vcpus`
- `allocatable_memory_mb`, `used_memory_mb`
- `instance_count`, `drain_status`
- `reported_at` (unix timestamp)

```bash
# Check that peer records include hypervisor_report (visible via topology JSON)
ssh root@server1 "syfrah fabric topology --json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for peer in data.get('peers', data if isinstance(data, list) else []):
    name = peer.get('name', '')
    report = peer.get('hypervisor_report')
    print(f'{name}: report={report}')
"
```

**Expected:** Each peer with a registered hypervisor shows a `hypervisor_report` with the capacity telemetry fields.

### 4. Verify report updates with state changes

```bash
# Drain a hypervisor
ssh root@server2 "syfrah hypervisor drain n2"
sleep 15

# Check gossip carries updated drain_status
ssh root@server1 "syfrah fabric topology --json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for peer in data.get('peers', data if isinstance(data, list) else []):
    name = peer.get('name', '')
    report = peer.get('hypervisor_report')
    if report:
        print(f'{name}: state={report.get(\"state\")}, drain={report.get(\"drain_status\")}')
"
```

**Expected:** Server 2's report shows `state=Draining`, `drain_status=true`.

### 5. Cleanup

```bash
ssh root@server2 "syfrah hypervisor activate n2"
ssh root@server1 "syfrah fabric stop 2>/dev/null"
ssh root@server2 "syfrah fabric stop 2>/dev/null"
```

## Pass criteria

- `HypervisorReport` struct added to `PeerRecord` in syfrah-core
- Report is optional (`None` when no hypervisor registered)
- Report includes: hypervisor_id, region, zone, state, allocatable/used capacity, instance_count, drain_status
- Report serializes/deserializes correctly via serde
- Backward compatible (old peers without the field still deserialize correctly via `#[serde(default)]`)
