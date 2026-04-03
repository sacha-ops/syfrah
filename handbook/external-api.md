# External API

This document covers the **Gateway API** and **Admin API** — the two external-facing API levels served on port 8443. Both share the same TLS endpoint; the caller's IAM role determines which endpoints are accessible. Tenant developers see only their own resources (Gateway API), while platform admins get full cluster visibility (Admin API).

The third API level, **Forge API**, is internal and machine-to-machine only (port 7100 over WireGuard). It is not covered here. See [api-architecture.md](api-architecture.md#three-api-levels) for the full overview of all three levels.

This document includes gateway setup, API key management, authentication, rate limiting, a full endpoint reference, and troubleshooting.

For internal architecture details, see [api-architecture.md](api-architecture.md).
For the Terraform provider, multi-language SDKs, and SDK generation pipeline, see [api-architecture.md](api-architecture.md#sdk-generation).

---

## Gateway Node Setup

A gateway node is a syfrah node explicitly designated by the operator to terminate TLS and serve both the tenant-facing Gateway API and the Admin API on a single endpoint. The caller's IAM role (attached to the API key) determines which endpoints are accessible — a `developer` key sees tenant resources only, while an `admin` or `owner` key unlocks cluster-wide operations. Gateway nodes are **not** dynamically elected; the operator promotes them via configuration.

### Configuration

Add a `[gateway]` section to `~/.syfrah/config.toml`:

```toml
[gateway]
enabled = true
bind_address = "0.0.0.0:8443"
tls_cert_path = "/etc/syfrah/tls/cert.pem"
tls_key_path  = "/etc/syfrah/tls/key.pem"
```

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `false` | Whether this node acts as a gateway. |
| `bind_address` | string (ip:port) | `0.0.0.0:8443` | Socket address to bind the external API to. |
| `tls_cert_path` | string (file path) | *(none)* | Path to a PEM-encoded TLS certificate. If omitted, a self-signed certificate is generated at startup. |
| `tls_key_path` | string (file path) | *(none)* | Path to a PEM-encoded TLS private key. Required when `tls_cert_path` is set. |

### TLS

The gateway **always** serves TLS. Plaintext HTTP is never exposed on public interfaces.

- **Operator-provided certificates:** Set both `tls_cert_path` and `tls_key_path`. Use certificates from Let's Encrypt or your internal CA. Both fields must be set together; setting one without the other is an error.
- **Self-signed (development only):** Omit both TLS fields. The daemon generates a self-signed certificate for the hostname `syfrah-gateway` at startup.

### Enabling the gateway

After editing `config.toml`, restart the daemon:

```bash
syfrah fabric stop
syfrah fabric start
```

The daemon logs `gateway API listening on 0.0.0.0:8443 (TLS)` on success.

### Multiple gateways

For high availability, designate multiple nodes as gateways and place them behind DNS round-robin or a load balancer. API key state is replicated to all gateway nodes through Raft-backed IAM state. If a gateway fails, promote another node or rely on DNS failover.

---

## API Key Creation and Management

API keys follow the format `syf_key_{project}_{random_256bit_base58}`. Only the SHA-256 hash is stored; the plaintext is shown once at creation time and never persisted.

### Create a key

```bash
syfrah-cli iam apikey create \
  --project backend \
  --role developer \
  --name ci-deploy \
  --ttl 3600
```

Output:

```
syf_key_bknd_7Fa9x2Qm...
```

Save this value immediately. It cannot be retrieved later.

**Parameters:**

| Flag | Required | Description |
|---|---|---|
| `--project` | yes | Short project identifier the key belongs to. |
| `--role` | yes | Permission role: `owner`, `admin`, `developer`, or `viewer`. |
| `--name` | yes | Human-readable name (unique within the project). |
| `--ttl` | no | Time-to-live in seconds. `0` (default) means no expiry. |
| `--allowed-cidrs` | no | Comma-separated CIDR allowlist (e.g. `203.0.113.0/24,10.0.0.0/8`). Empty means any source IP is accepted. Supports both IPv4 and IPv6. |

### Rotate a key

Rotation creates a new key with the same project and role, then puts the old key into a grace period during which both keys are valid.

```bash
syfrah-cli iam apikey rotate --name ci-deploy --grace 5
```

- `--grace` sets the grace period in minutes (old key remains valid for this duration after the new key is issued).
- The new raw key is printed to stdout. The old key stops working when the grace period expires.

### Delete (revoke) a key

```bash
syfrah-cli iam apikey delete --name ci-deploy
```

Deletion is immediate. The key is removed from the store and all gateway nodes.

### List keys

```bash
syfrah-cli iam apikey list --project backend
```

Returns key names, roles, and creation timestamps. Never exposes hashes or raw keys.

### CIDR allowlists

When `allowed_cidrs` is set on a key, requests are only accepted from IP addresses that fall within at least one of the listed CIDR ranges. If no CIDRs are configured, any source IP is accepted.

CIDR enforcement rules:
- Supports IPv4 (`192.168.1.0/24`) and IPv6 (`fd00::/16`).
- Single-host entries use `/32` (IPv4) or `/128` (IPv6).
- If CIDRs are configured but the source IP cannot be determined, the request is rejected.

### TTL (time-to-live)

Keys with a TTL automatically expire after `created_at + ttl` seconds. Expired keys return `AUTH_UNAUTHORIZED`. Use short TTLs for CI pipelines (e.g. `--ttl 3600` for one hour).

---

## Authentication

All external API requests require authentication via an API key.

### Header format

```
Authorization: Bearer syf_key_{project}_{secret}
```

### Using environment variables

```bash
export SYFRAH_API_KEY=syf_key_bknd_7Fa9x2Qm...
```

The laptop CLI (`syfrah-cli`) reads this variable automatically. For Terraform, set it in your provider configuration or environment.

### Using the CLI login flow

```bash
syfrah-cli login --endpoint https://api.prod.example.com
```

This stores the API key in the OS keychain.

### Context switching

```bash
syfrah-cli context set --name prod --endpoint https://api.prod.example.com
syfrah-cli context use prod
```

### Role hierarchy

Roles are checked per-endpoint. Higher roles inherit lower-role permissions:

| Role | Permissions |
|---|---|
| `owner` | Full access to everything including IAM management. |
| `admin` | Full access including secret rotation and config reload. |
| `developer` / `operator` | Create/manage peers, VMs, VPCs, volumes. Cannot manage IAM or rotate secrets. |
| `viewer` / `read_only` | Read-only access (status, list operations). |

Endpoint-to-role mapping:

| Endpoint | Minimum role |
|---|---|
| `GET /v1/fabric/status` | read_only |
| `GET /v1/fabric/peering/requests` | read_only |
| `POST /v1/fabric/peering/start` | operator |
| `POST /v1/fabric/peering/stop` | operator |
| `POST /v1/fabric/peering/accept` | operator |
| `POST /v1/fabric/peering/reject` | operator |
| `POST /v1/fabric/peers/remove` | operator |
| `POST /v1/fabric/peers/update-endpoint` | operator |
| `POST /v1/fabric/rotate-secret` | admin |
| `POST /v1/fabric/reload` | admin |

---

## Rate Limiting

Rate limiting applies to the external API only (not the local Unix socket, not internal node-to-node traffic).

### Defaults

| Scope | Sustained rate | Burst capacity |
|---|---|---|
| Per API key | 100 req/s | 200 |
| Aggregate (all keys) | 1 000 req/s | 2 000 |

### How it works

Rate limiting uses an in-memory token-bucket algorithm per gateway instance. Each API key gets its own bucket, and there is a shared aggregate bucket across all keys.

- Tokens refill at the sustained rate (e.g. 100 tokens/s per key).
- Burst allows short spikes above the sustained rate, up to the bucket capacity.
- When a bucket is empty, the request is rejected with HTTP 429 and a `Retry-After` header indicating how many milliseconds to wait.

### Per-key behavior

Each API key is isolated. Exhausting one key's limit does not affect other keys (unless the aggregate limit is also hit).

### Multi-gateway note

Rate limits are enforced per gateway instance. The effective burst for a given API key across multiple gateways is `burst x gateway_count`. Global rate limiting across gateways may be added in a future version.

### Rejection response

When rate-limited, the gateway returns:

```
HTTP/1.1 429 Too Many Requests
Retry-After: 10
Content-Type: application/json
```

```json
{
  "error": {
    "code": "RESOURCE_EXHAUSTED",
    "message": "rate limit exceeded for key 'ci-deploy'",
    "trace_id": "req-a7f3e29b1c04"
  }
}
```

---

## REST Endpoint Reference

All endpoints are served under `/v1/fabric/` on the gateway address (default `https://<gateway>:8443`). Requests and responses use JSON (`Content-Type: application/json`).

### GET /v1/fabric/status

Health check. Returns the gateway's operational status.

**Minimum role:** read_only

**Request:**

```bash
curl -s https://api.example.com:8443/v1/fabric/status \
  -H "Authorization: Bearer $SYFRAH_API_KEY"
```

**Response (200 OK):**

```json
{
  "status": "ok"
}
```

---

### GET /v1/fabric/peering/requests

List pending join requests from nodes waiting to be accepted into the mesh.

**Minimum role:** read_only

**Request:**

```bash
curl -s https://api.example.com:8443/v1/fabric/peering/requests \
  -H "Authorization: Bearer $SYFRAH_API_KEY"
```

**Response (200 OK):**

```json
{
  "requests": [
    {
      "request_id": "a1b2c3d4",
      "node_name": "web-03",
      "wg_public_key": "kH5...=",
      "endpoint": "203.0.113.50:51820",
      "wg_listen_port": 51820,
      "received_at": 1711555200,
      "region": "us-east",
      "zone": "us-east-1a"
    }
  ]
}
```

---

### POST /v1/fabric/peering/start

Start listening for peering (join) requests on the specified port.

**Minimum role:** operator

**Request body:**

| Field | Type | Required | Description |
|---|---|---|---|
| `port` | integer | yes | TCP port to listen on for peering connections. |
| `pin` | string | no | PIN code for auto-accept mode. Joining nodes that present this PIN are accepted automatically. |

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peering/start \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"port": 7946, "pin": "1234"}'
```

**Response (200 OK):**

```json
{
  "ok": true
}
```

---

### POST /v1/fabric/peering/stop

Stop listening for peering requests.

**Minimum role:** operator

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peering/stop \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json"
```

**Response (200 OK):**

```json
{
  "ok": true
}
```

---

### POST /v1/fabric/peering/accept

Accept a pending join request by its request ID.

**Minimum role:** operator

**Request body:**

| Field | Type | Required | Description |
|---|---|---|---|
| `request_id` | string | yes | The ID of the pending join request to accept. |

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peering/accept \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"request_id": "a1b2c3d4"}'
```

**Response (200 OK):**

```json
{
  "peer_name": "peer-a1b2c3d4"
}
```

---

### POST /v1/fabric/peering/reject

Reject a pending join request.

**Minimum role:** operator

**Request body:**

| Field | Type | Required | Description |
|---|---|---|---|
| `request_id` | string | yes | The ID of the pending join request to reject. |
| `reason` | string | no | Human-readable reason for the rejection. |

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peering/reject \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"request_id": "a1b2c3d4", "reason": "unauthorized node"}'
```

**Response (200 OK):**

```json
{
  "ok": true
}
```

---

### POST /v1/fabric/peers/remove

Remove a peer from the mesh by name or WireGuard public key.

**Minimum role:** operator

**Request body:**

| Field | Type | Required | Description |
|---|---|---|---|
| `name_or_key` | string | yes | Peer name or WireGuard public key. |

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peers/remove \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name_or_key": "web-03"}'
```

**Response (200 OK):**

```json
{
  "peer_name": "web-03",
  "announced_to": 3
}
```

---

### POST /v1/fabric/peers/update-endpoint

Update the public endpoint address of an existing peer.

**Minimum role:** operator

**Request body:**

| Field | Type | Required | Description |
|---|---|---|---|
| `name_or_key` | string | yes | Peer name or WireGuard public key. |
| `endpoint` | string | yes | New endpoint address in `ip:port` format. |

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peers/update-endpoint \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name_or_key": "web-03", "endpoint": "203.0.113.51:51820"}'
```

**Response (200 OK):**

```json
{
  "peer_name": "web-03",
  "old_endpoint": "203.0.113.50:51820",
  "new_endpoint": "203.0.113.51:51820"
}
```

**Error (400 Bad Request):**

```json
{
  "error": "invalid endpoint address: invalid socket address syntax"
}
```

---

### POST /v1/fabric/reload

Reload the daemon configuration from `~/.syfrah/config.toml` without restarting.

**Minimum role:** admin

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/reload \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json"
```

**Response (200 OK):**

```json
{
  "changes": ["keepalive"],
  "skipped": []
}
```

---

### POST /v1/fabric/rotate-secret

Rotate the mesh WireGuard secret. Generates a new key pair, updates the local node, and notifies all peers.

**Minimum role:** admin

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/rotate-secret \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json"
```

**Response (200 OK):**

```json
{
  "new_secret": "kH5...=",
  "new_ipv6": "fd00::abcd:1234",
  "peers_notified": 5,
  "peers_failed": 0
}
```

---

### Topology Endpoints (Internal HTTP API)

The following read-only endpoints are served on the internal HTTP API (default `127.0.0.1:9100`). They do not require API key authentication and are intended for monitoring and observability.

Enable them in `config.toml`:

```toml
[api]
enabled = true
listen = "127.0.0.1:9100"
```

| Method | Path | Description |
|---|---|---|
| GET | `/v1/topology` | Full topology overview (regions, zones, peer counts). |
| GET | `/v1/topology/regions` | List all regions. |
| GET | `/v1/topology/regions/{region}` | Region detail with zones and peer counts. |
| GET | `/v1/topology/zones/{zone}/peers` | List peers in a specific zone. |
| GET | `/v1/topology/health` | Zone-level health status (active/unreachable counts). |
| GET | `/v1/peers` | List all peers across all regions. |
| GET | `/v1/health` | Simple health check with peer counts. |
| GET | `/metrics` | Prometheus text exposition format. |

---

## Error Response Format

All errors follow a consistent JSON structure:

```json
{
  "error": {
    "code": "FABRIC_PEER_NOT_FOUND",
    "message": "No peer named 'web-03' in the mesh. Did you mean 'web-3'?",
    "trace_id": "req-a7f3e29b1c04"
  }
}
```

| Field | Description |
|---|---|
| `code` | Machine-readable, stable across versions. Match on this in scripts and automation. |
| `message` | Human-readable, may change between releases. |
| `trace_id` | Unique request ID for correlating with server-side logs. |

### Error codes

| Code | HTTP Status | Meaning |
|---|---|---|
| `AUTH_UNAUTHORIZED` | 401 | Missing, invalid, expired, or revoked API key. |
| `AUTH_FORBIDDEN` | 403 | Valid key but insufficient role, or source IP not in CIDR allowlist. |
| `RESOURCE_EXHAUSTED` | 429 | Rate limit exceeded. |
| `FABRIC_PEER_NOT_FOUND` | 400 | The specified peer does not exist. |
| `FABRIC_MESH_NOT_INITIALIZED` | 400 | The mesh has not been initialized yet. |
| `FABRIC_DAEMON_NOT_RUNNING` | 503 | The daemon is not running or unreachable. |
| `COMPUTE_VM_NOT_FOUND` | 404 | The specified VM does not exist. |
| `COMPUTE_INSUFFICIENT_RESOURCES` | 409 | Not enough resources to fulfill the request. |
| `IAM_KEY_EXPIRED` | 401 | The API key has exceeded its TTL. |
| `IAM_UNAUTHORIZED` | 403 | IAM policy denies the requested action. |
| `INTERNAL_ERROR` | 500 | Unexpected server error. |

Each layer prefixes its codes with the layer name (e.g. `FABRIC_`, `COMPUTE_`, `IAM_`). The gRPC status codes map accordingly: `NOT_FOUND`, `UNAUTHENTICATED`, `PERMISSION_DENIED`, `RESOURCE_EXHAUSTED`, etc.

---

## Troubleshooting

### 401 Unauthorized

**Symptoms:** `{"error": "authentication failed: ..."}` with HTTP 401.

**Causes and fixes:**
- **Missing header.** Ensure every request includes `Authorization: Bearer syf_key_...`.
- **Invalid key format.** Keys must start with `syf_key_` and be at least 12 characters.
- **Expired key.** Check the key's TTL. Create a new key if expired.
- **Revoked key.** The key was deleted. Create a new one.
- **Grace period expired.** After rotation, the old key's grace period has passed. Use the new key.

### 403 Forbidden

**Symptoms:** `{"error": "insufficient permissions: ..."}` with HTTP 403.

**Causes and fixes:**
- **Wrong role.** The key's role does not have access to this endpoint. Check the role-to-endpoint mapping above. Create a key with a higher-privilege role if needed.
- **CIDR rejection.** The source IP is not in the key's CIDR allowlist. Verify your IP and update the allowlist.

### 429 Too Many Requests

**Symptoms:** `{"error": {"code": "RESOURCE_EXHAUSTED", ...}}` with HTTP 429.

**Causes and fixes:**
- **Per-key limit hit.** Your key exceeded 100 req/s sustained or 200 burst. Back off and retry after the `Retry-After` header value (in milliseconds).
- **Aggregate limit hit.** Total traffic across all keys on this gateway exceeded 1 000 req/s. Distribute traffic across multiple gateways or reduce request volume.
- **Terraform parallelism.** If Terraform is making too many concurrent calls, reduce parallelism with `terraform apply -parallelism=5`.

### Connection refused

**Symptoms:** `curl: (7) Failed to connect to ... port 8443: Connection refused`

**Causes and fixes:**
- **Gateway not enabled.** Verify `[gateway] enabled = true` in `config.toml`.
- **Daemon not running.** Start the daemon with `syfrah fabric start`.
- **Wrong port.** Check `bind_address` in the gateway config. Default is `0.0.0.0:8443`.
- **Firewall.** Ensure port 8443 (or your configured port) is open in your firewall and security group rules.

### TLS errors

**Symptoms:** `curl: (60) SSL certificate problem` or `TLS handshake failed`

**Causes and fixes:**
- **Self-signed certificate.** If using the auto-generated self-signed cert (development only), pass `-k` to curl or configure your client to trust the cert. Do not use self-signed certs in production.
- **Certificate mismatch.** The certificate's Subject Alternative Name (SAN) must match the hostname you connect to. Re-issue the certificate with the correct domain.
- **Expired certificate.** Renew the certificate and restart the daemon.
- **Wrong file paths.** Verify `tls_cert_path` and `tls_key_path` point to valid PEM files. Both must be set together.
- **Key/cert mismatch.** Ensure the private key matches the certificate. The daemon logs `gateway TLS config error` if they do not match.

### 503 Service Unavailable

**Symptoms:** `{"error": "store unavailable: ..."}` with HTTP 503.

**Causes and fixes:**
- **State store not ready.** The daemon is starting up or the state store is not yet initialized. Wait a moment and retry.
- **Disk full.** Check available disk space for `~/.syfrah/`.

---

## Examples

### Check gateway health

```bash
curl -s https://api.example.com:8443/v1/fabric/status \
  -H "Authorization: Bearer $SYFRAH_API_KEY" | jq .
```

### List pending join requests and accept one

```bash
# List pending requests
curl -s https://api.example.com:8443/v1/fabric/peering/requests \
  -H "Authorization: Bearer $SYFRAH_API_KEY" | jq .

# Accept a specific request
curl -s -X POST https://api.example.com:8443/v1/fabric/peering/accept \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"request_id": "a1b2c3d4"}' | jq .
```

### Start peering with auto-accept PIN

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peering/start \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"port": 7946, "pin": "secure-pin-1234"}' | jq .
```

### Remove a peer from the mesh

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peers/remove \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name_or_key": "decommissioned-node"}' | jq .
```

### Update a peer's endpoint after IP change

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/peers/update-endpoint \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name_or_key": "web-03", "endpoint": "198.51.100.10:51820"}' | jq .
```

### Rotate the mesh WireGuard secret

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/rotate-secret \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" | jq .
```

### Reload daemon configuration

```bash
curl -s -X POST https://api.example.com:8443/v1/fabric/reload \
  -H "Authorization: Bearer $SYFRAH_API_KEY" \
  -H "Content-Type: application/json" | jq .
```

### Self-signed certificate (development only)

```bash
# Skip TLS verification for development with self-signed certs
curl -sk https://localhost:8443/v1/fabric/status \
  -H "Authorization: Bearer $SYFRAH_API_KEY" | jq .
```

---

## Source

| Component | Source file |
|---|---|
| Gateway config | `layers/fabric/src/config.rs` — `GatewayConfig`, `resolve_gateway_tls` |
| Gateway TLS server | `layers/fabric/src/grpc_api.rs` — `serve_gateway_tls` |
| REST endpoints | `layers/fabric/src/grpc_api.rs` — router and handlers |
| Auth middleware | `layers/fabric/src/auth_middleware.rs` |
| API key lifecycle | `layers/api/src/apikey.rs` |
| Rate limiting | `layers/api/src/rate_limit.rs` |
| Error codes | `layers/api/src/error.rs` |
| Topology (internal API) | `layers/fabric/src/http_api.rs` |
