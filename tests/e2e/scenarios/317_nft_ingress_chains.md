# E2E: nftables ingress chain generation from SG rules

## Objective

Verify that `generate_ingress_chain()` correctly converts merged security
group rules into nftables per-VM ingress chain statements, with proper
priority ordering and implicit deny.

## Prerequisites

- A VPC with a default security group exists.
- A VM is running with a NIC attached to the default SG.

## Scenarios

### 1. Default SG produces SSH + ICMP ingress rules

**Given** a NIC attached to the default SG (SSH TCP/22 + ICMP)
**When** `generate_ingress_chain` is called
**Then** the chain contains:
```
tcp dport 22 accept
icmp type echo-request accept
drop
```

### 2. Custom TCP port rule

**Given** an SG with a rule allowing TCP/5432 from 10.1.0.0/16
**When** `generate_ingress_chain` is called
**Then** the chain contains:
```
ip saddr 10.1.0.0/16 tcp dport 5432 accept
drop
```

### 3. UDP port range

**Given** an SG with a rule allowing UDP 8000-9000 from 0.0.0.0/0
**When** `generate_ingress_chain` is called
**Then** the chain contains:
```
udp dport 8000-9000 accept
drop
```

### 4. SG-to-SG reference uses named set

**Given** an SG with a rule allowing TCP/5432 from sg:web-sg
**When** `generate_ingress_chain` is called
**Then** the chain contains:
```
ip saddr @sg_{hash}_ips tcp dport 5432 accept
drop
```
where `{hash}` is the short hash of `web-sg`.

### 5. Multiple SGs merged by priority

**Given** a NIC attached to two SGs:
- SG-A: TCP/22 at priority 200
- SG-B: TCP/443 at priority 100
**When** rules are merged and `generate_ingress_chain` is called
**Then** TCP/443 (priority 100) appears before TCP/22 (priority 200):
```
tcp dport 443 accept
tcp dport 22 accept
drop
```

### 6. No ingress rules produce only implicit deny

**Given** a NIC with no ingress rules (or only egress rules)
**When** `generate_ingress_chain` is called
**Then** the chain contains only:
```
drop
```

### 7. Protocol All with CIDR source

**Given** an SG rule with protocol=All, source=10.1.0.0/16
**When** `generate_ingress_chain` is called
**Then** the chain contains:
```
ip saddr 10.1.0.0/16 accept
drop
```

## Verification

All scenarios are covered by unit tests in `layers/overlay/src/sg_nft.rs`:
- `test_generate_ingress_tcp`
- `test_generate_ingress_udp_range`
- `test_generate_ingress_icmp`
- `test_generate_ingress_cidr_source`
- `test_generate_ingress_sg_source`
- `test_merge_multiple_sgs`
- `test_implicit_deny`
- `test_protocol_all_cidr_source`
- `test_egress_rules_filtered_out`

Run: `cargo test -p syfrah-overlay sg_nft`
