# Test: Signed request authentication

## Objective

Verify the authentication middleware framework:
- Phase 1 (WireGuard trust): allows requests without Authorization header
- Logs caller identity when Authorization header is present
- Framework ready for future signed request enforcement

## Steps

### 1. Request without auth header (allowed in Phase 1)

```bash
FORGE_IP=$(ip -6 addr show syfrah0 | grep 'inet6 fd' | awk '{print $2}' | cut -d/ -f1 | head -1)
curl -s http://[$FORGE_IP]:7100/v1/hypervisor/health | python3 -m json.tool
```

**Expected:** 200 OK with health response.

### 2. Request with auth header (logged, allowed)

```bash
curl -s -H "Authorization: Bearer test-token-abc123" http://[$FORGE_IP]:7100/v1/hypervisor/health | python3 -m json.tool
```

**Expected:** 200 OK. Daemon logs show `authenticated request` with caller identity.

## Pass criteria

- Requests without auth headers work in Phase 1
- Requests with auth headers are logged with caller identity
- Auth middleware is wired into the Forge router
- Framework ready for RequireSigned mode
