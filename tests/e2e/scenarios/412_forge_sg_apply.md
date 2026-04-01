# Test: Forge security group application endpoints

## Objective

- POST /v1/networks/sg/apply generates nftables from SG rules and applies per-VM chains
- POST /v1/networks/sg/remove flushes VM chains
- Uses sg_nft module for rule generation
- Invalid IP returns 400

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded
- `nft` (nftables) available

## Steps

### 1. Initialize the mesh

```bash
syfrah fabric stop 2>/dev/null; sleep 2
rm -rf ~/.syfrah/fabric.redb ~/.syfrah/state.json ~/.syfrah/*.redb
syfrah fabric init --name test --node-name n1 --endpoint $(hostname -I | awk '{print $1}'):51820
sleep 3
```

### 2. Get the fabric IPv6 address

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep inet6 | awk '{print $2}' | cut -d/ -f1 | head -1)
```

### 3. Apply SG rules for a VM

```bash
RESULT=$(curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/sg/apply \
  -H 'Content-Type: application/json' \
  -d '{
    "vm_id": "vm-sg-test",
    "ip": "10.1.0.3",
    "mac": "02:00:0a:01:00:03",
    "security_groups": ["web-sg"],
    "rules": [
      {"id":"r1","sg_id":"web-sg","direction":"ingress","protocol":"tcp","port_range_start":22,"port_range_end":22,"source":"0.0.0.0/0","priority":100},
      {"id":"r2","sg_id":"web-sg","direction":"ingress","protocol":"tcp","port_range_start":80,"port_range_end":80,"source":"0.0.0.0/0","priority":200}
    ],
    "sg_ip_map": {}
  }')
echo "$RESULT"
```

**Expected:** HTTP 200 with `FORGE_SG_APPLIED` code and `chains` array containing ingress and egress chain names.

### 4. Verify nftables chains exist

```bash
nft list table inet syfrah_sg 2>/dev/null
```

**Expected:** Table contains ingress and egress chains for the VM with the specified rules.

### 5. Remove SG rules for the VM

```bash
curl -s -X POST http://[$FORGE_IP]:7100/v1/networks/sg/remove \
  -H 'Content-Type: application/json' \
  -d '{"vm_id": "vm-sg-test"}'
```

**Expected:** HTTP 200 with `FORGE_SG_REMOVED` code.

### 6. Verify chains removed

```bash
nft list table inet syfrah_sg 2>/dev/null | grep "vm_"
```

**Expected:** No VM-specific chains remain.

### 7. Invalid IP returns 400

```bash
RESULT=$(curl -s -o /dev/null -w '%{http_code}' -X POST http://[$FORGE_IP]:7100/v1/networks/sg/apply \
  -H 'Content-Type: application/json' \
  -d '{"vm_id":"vm-1","ip":"not-an-ip","mac":"02:00:0a:01:00:03","security_groups":["default"]}')
echo "$RESULT"
```

**Expected:** HTTP 400.

## Cleanup

```bash
syfrah fabric stop
sleep 2
```
