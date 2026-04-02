# 603 — Security Group Enforcement

Verify that security group rules are enforced at the network level — allowed traffic passes, denied traffic is blocked.

## Prerequisites

- Cluster bootstrapped with Raft leader elected
- Default VPC and subnet `web` exist under org `acme`, project `backend`, env `prod`
- At least one other VM exists for sending test traffic

## Setup

```bash
# Create a restrictive SG: only HTTP (TCP 80) and ICMP — NO SSH (TCP 22)
syfrah sg create http-only --vpc acme-backend-default
syfrah sg add-rule http-only --direction ingress --protocol tcp --port 80 --source 0.0.0.0/0
syfrah sg add-rule http-only --direction ingress --protocol icmp --source 0.0.0.0/0
# NOTE: No port 22 rule — SSH must be blocked

# Create a VM with the restrictive SG
syfrah compute vm create --name sg-test --image alpine-3.20 --vcpus 1 --memory 512 \
  --env prod --subnet web --project backend --org acme \
  --ssh-key ~/.ssh/id_ed25519.pub --sg http-only
```

## Assertions

1. **ICMP (ping) works** — ICMP rule is present.
   ```bash
   # From another VM in the same subnet:
   # ping <sg-test-ip> -c 4 -W 2
   # Expected: 0% packet loss
   ```

2. **SSH (TCP 22) is BLOCKED** — no port 22 rule in the SG.
   ```bash
   # From another VM:
   # ssh -o ConnectTimeout=3 -o StrictHostKeyChecking=no root@<sg-test-ip>
   # Expected: Connection refused or timeout (exit code != 0)
   ```

3. **SG check CLI confirms denial**.
   ```bash
   syfrah sg check --vm sg-test --port 22 --protocol tcp
   # Expected output: DENIED
   
   syfrah sg check --vm sg-test --port 80 --protocol tcp
   # Expected output: ALLOWED
   
   syfrah sg check --vm sg-test --port 443 --protocol tcp
   # Expected output: DENIED (no rule for 443)
   ```

4. **nftables rules are correctly applied**.
   ```bash
   # On the hypervisor hosting sg-test:
   # nft list chain inet syfrah vm-sg-test-ingress
   # Expected: TCP dport 80 accept, ICMP accept, no TCP dport 22 rule
   ```

## Expected results

| Check                           | Expected           |
|---------------------------------|--------------------|
| Ping sg-test                    | 0% loss (ALLOWED)  |
| SSH to sg-test                  | REFUSED/TIMEOUT    |
| sg check --port 22 --protocol tcp | DENIED           |
| sg check --port 80 --protocol tcp | ALLOWED          |
| sg check --port 443 --protocol tcp | DENIED          |
| nftables chain                  | Correct rules      |

## Pass criteria

- Allowed protocols (ICMP, TCP 80) pass through
- Denied protocols (TCP 22, TCP 443) are blocked
- `syfrah sg check` returns correct ALLOWED/DENIED verdicts
- nftables rules match the SG definition exactly
