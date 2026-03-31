# 344 — Explicit NAT Gateway required

## Preconditions
- Fabric initialized, daemon running
- Org, project, environment, subnet exist

## Steps

1. Create VM without NAT GW:
   - No NAT GW in VPC
   - VM should have private networking only
   - Warning logged: "no NAT gateway — VMs will not have internet egress"

2. Create NAT GW, then create VM:
   - NAT GW active in VPC
   - VM should have internet egress via masquerade

3. Delete NAT GW (remove routes first):
   - Existing VMs lose internet egress

## Pass criteria
- Without NAT GW: VMs cannot reach the internet
- With NAT GW: VMs have internet egress
- Warning is logged when no NAT GW exists
