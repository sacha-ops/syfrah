# Test: Forge create orchestration in dependency order

## Objective

- POST /v1/instances with subnet triggers full orchestration flow
- Subnet resolution: name -> CIDR, gateway, VPC from org store
- Full network setup: IPAM alloc -> bridge -> VXLAN -> TAP -> SG -> NAT -> FDB -> config-drive -> boot
- Task record tracks the full operation
- Compensating cleanup on any step failure (IP release, TAP cleanup)

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon running with fabric initialized
- Org hierarchy created: org, project, env, subnet, nat-gw, sg
- Cloud Hypervisor binary available

## Steps

### 1. Set up org hierarchy

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
syfrah subnet create frontend --env production --project backend --org acme
syfrah nat-gw create main-gw --vpc acme-backend-default --subnet frontend
syfrah sg create web-sg --vpc acme-backend-default
syfrah sg add-rule web-sg --direction ingress --protocol tcp --port 22 --source 0.0.0.0/0
```

### 2. Get Forge endpoint

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep "inet6 fd" | awk '{print $2}' | cut -d/ -f1 | head -1)
FORGE="http://[$FORGE_IP]:7100"
```

### 3. Create instance with full orchestration

```bash
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"orch-vm","image":"alpine-3.20","vcpus":1,"memory_mb":512,"subnet":"frontend","security_groups":["web-sg"]}' | jq .
```

Expected: 201 Created with VmStatus showing the VM booted.

### 4. Verify network was set up

```bash
# Check bridge exists
ip link show syfb-acme-backend-default 2>/dev/null && echo "Bridge OK"

# Check VXLAN exists
ip link show syfx-acme-backend-default 2>/dev/null && echo "VXLAN OK"

# Check TAP exists
ip link show syft-orch-vm 2>/dev/null && echo "TAP OK"
```

### 5. Verify task tracking

```bash
curl -s "$FORGE/v1/tasks?resource_id=orch-vm" | jq .
# Should show a create_instance task in Completed state
```

### 6. Verify instance is reachable

```bash
curl -s $FORGE/v1/instances/orch-vm | jq .
# Should show Running status with network info
```

### 7. Verify via CLI backward compatibility

```bash
syfrah compute vm list
syfrah compute vm get orch-vm
```

### 8. Test failure with nonexistent subnet

```bash
curl -s -X POST $FORGE/v1/instances \
  -H "Content-Type: application/json" \
  -d '{"name":"fail-vm","image":"alpine-3.20","subnet":"nonexistent"}' | jq .
# Should return 404 with FORGE_SUBNET_NOT_FOUND
```

### 9. Cleanup

```bash
curl -s -X DELETE $FORGE/v1/instances/orch-vm | jq .
```

## Expected Results

- Create with subnet triggers full IPAM -> bridge -> VXLAN -> TAP -> SG -> NAT -> boot flow
- Subnet name is resolved from the org store
- Task record is created and completed on success
- Failed subnet resolution returns 404 FORGE_SUBNET_NOT_FOUND
- Network resources (bridge, VXLAN, TAP) are created on the host
- CLI `vm list` / `vm get` still works alongside the API
